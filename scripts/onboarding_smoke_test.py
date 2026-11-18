# Onboarding smoke test for a running steeld. Usage: python3 scripts/onboarding_smoke_test.py http://127.0.0.1:8080
import json, sys, time, urllib.request, urllib.error
BASE = sys.argv[1]; API = BASE + "/api/v10"
RUN = str(int(time.time()))[-6:]
def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(API + path, data=data, method=method, headers={"content-type": "application/json", **({"authorization": "Bearer " + token} if token else {})})
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read(); return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")
def reg(n): return req("POST", "/auth/register", {"username": f"{n}_{RUN}", "password": "password123"})[1]
own, mem = reg("obown"), reg("obmemb")
to, tm = own["token"], mem["token"]
g = req("POST", "/guilds", {"name": "Onboard"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]; req("POST", f"/invites/{inv}", token=tm)

prompts = [
    {"title": "What brings you here?", "single_select": False, "required": True, "options": [
        {"title": "Games", "description": "Play together", "emoji": {"name": "🎮"}},
        {"title": "Art", "options_hint": None},
    ]},
    {"title": "Pick a region", "single_select": True, "required": False, "options": [
        {"title": "North"}, {"title": "South"},
    ]},
]
print("disabled GET ->", req("GET", f"/guilds/{gid}/onboarding", token=tm)[1]["enabled"] is False)
print("member write ->", req("PUT", f"/guilds/{gid}/onboarding", {"enabled": True, "prompts": prompts}, tm)[0])
s, doc = req("PUT", f"/guilds/{gid}/onboarding", {"enabled": True, "prompts": prompts}, to)
print("owner write ->", s, doc["enabled"], len(doc["prompts"]), [len(p["options"]) for p in doc["prompts"]])
games = doc["prompts"][0]["options"][0]["id"]; art = doc["prompts"][0]["options"][1]["id"]
north = doc["prompts"][1]["options"][0]["id"]; south = doc["prompts"][1]["options"][1]["id"]
print("emoji shape ->", doc["prompts"][0]["options"][0]["emoji"] == {"id": None, "name": "🎮"})
print("bad id ->", req("PUT", f"/guilds/{gid}/onboarding", {"enabled": True, "prompts": [{"id": "abc", "title": "x", "options": [{"title": "y"}]}]}, to)[0])
print("duplicate id ->", req("PUT", f"/guilds/{gid}/onboarding", {"enabled": True, "prompts": [{"id": "5", "title": "x", "options": [{"id": "5", "title": "y"}]}]}, to)[0])
print("no options ->", req("PUT", f"/guilds/{gid}/onboarding", {"enabled": True, "prompts": [{"title": "x", "options": []}]}, to)[0])
print("member view ->", req("GET", f"/guilds/{gid}/onboarding", token=tm)[1]["enabled"] is True)
print("missing required ->", req("POST", f"/guilds/{gid}/onboarding-responses", {"onboarding_responses": [north]}, tm)[0])
print("two single-select ->", req("POST", f"/guilds/{gid}/onboarding-responses", {"onboarding_responses": [games, north, south]}, tm)[0])
print("unknown option ->", req("POST", f"/guilds/{gid}/onboarding-responses", {"onboarding_responses": [games, "999"]}, tm)[0])
s, _ = req("POST", f"/guilds/{gid}/onboarding-responses", {"onboarding_responses": [games, art, north],
    "onboarding_prompts_seen": {doc["prompts"][0]["id"]: 1, doc["prompts"][1]["id"]: 1},
    "onboarding_responses_seen": {games: 1, art: 1, north: 1}}, tm)
print("submit ->", s)
print("responses on GET ->", sorted(req("GET", f"/guilds/{gid}/onboarding", token=tm)[1]["responses"]) == sorted([games, art, north]))
print("owner responses empty ->", req("GET", f"/guilds/{gid}/onboarding", token=to)[1]["responses"] == [])
print("disabled submit ->", end=" ")
req("PUT", f"/guilds/{gid}/onboarding", {"enabled": False, "prompts": prompts}, to)
print(req("POST", f"/guilds/{gid}/onboarding-responses", {"onboarding_responses": []}, tm)[0])
print("outsider GET ->", req("GET", f"/guilds/{gid}/onboarding", token=reg("obout")["token"])[0])
