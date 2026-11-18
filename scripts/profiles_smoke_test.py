# Profile edit smoke test for a running steeld. Usage: python3 scripts/profiles_smoke_test.py http://127.0.0.1:8080
import sys, time, json, urllib.request, urllib.error
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
me = req("POST", "/auth/register", {"username": f"pf_{RUN}", "password": "password123"})[1]
t, uid = me["token"], me["user"]["id"]
s, out = req("PATCH", "/users/@me", {"global_name": "Pat", "bio": "  Hi there  ", "pronouns": "they/them", "accent_color": 1193046}, t)
print("edit ->", s, out["global_name"] == "Pat")
s, prof = req("GET", f"/users/{uid}/profile", token=t)
print("profile shows edits", prof["user"]["global_name"] == "Pat", prof["user_profile"]["bio"] == "Hi there", prof["user_profile"]["pronouns"] == "they/them", prof["user_profile"]["accent_color"] == 1193046)
s, out = req("PATCH", "/users/@me", {"bio": None}, t)
s, prof = req("GET", f"/users/{uid}/profile", token=t)
print("null clears bio, keeps others", prof["user_profile"]["bio"] is None, prof["user_profile"]["pronouns"] == "they/them")
print("absent keeps global name", req("GET", f"/users/{uid}/profile", token=t)[1]["user"]["global_name"] == "Pat")
print("clear accent ->", req("PATCH", "/users/@me", {"accent_color": None}, t)[0], req("GET", f"/users/{uid}/profile", token=t)[1]["user_profile"]["accent_color"] is None)
print("too long bio ->", req("PATCH", "/users/@me", {"bio": "x" * 191}, t)[0])
print("bad accent ->", req("PATCH", "/users/@me", {"accent_color": 0x1000000}, t)[0])
print("no auth ->", req("PATCH", "/users/@me", {"bio": "x"})[0])
