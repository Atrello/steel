# Audit log smoke test for a running steeld. Usage: python3 scripts/audit_smoke_test.py http://127.0.0.1:8080
import sys, time, json, urllib.request, urllib.error
BASE = sys.argv[1]; API = BASE + "/api/v10"
RUN = str(int(time.time()))[-6:]
PNG = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(API + path, data=data, method=method, headers={"content-type": "application/json", **({"authorization": "Bearer " + token} if token else {})})
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read(); return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")
def reg(n): return req("POST", "/auth/register", {"username": f"{n}_{RUN}", "password": "password123"})[1]
own, mem, victim = reg("auown"), reg("aumem"), reg("auvic")
to, tm, tv = own["token"], mem["token"], victim["token"]
g = req("POST", "/guilds", {"name": "Audit"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]
req("POST", f"/invites/{inv}", token=tm); req("POST", f"/invites/{inv}", token=tv)

s, newch = req("POST", f"/guilds/{gid}/channels", {"name": "audit-chan"}, to)
req("DELETE", f"/channels/{newch['id']}", token=to)
req("DELETE", f"/guilds/{gid}/members/{victim['user']['id']}", token=to)
req("PUT", f"/guilds/{gid}/bans/{mem['user']['id']}", token=to)
req("DELETE", f"/guilds/{gid}/bans/{mem['user']['id']}", token=to)
s, em = req("POST", f"/guilds/{gid}/emojis", {"name": "audit_e", "image": PNG}, to)
req("DELETE", f"/guilds/{gid}/emojis/{em['id']}", token=to)

s, page = req("GET", f"/guilds/{gid}/audit-logs?limit=50", token=to)
actions = [e["action_type"] for e in page["audit_log_entries"]]
print("owner reads log ->", s, sorted(set(actions)) == sorted({10, 12, 20, 22, 23, 60, 62}))
print("newest first", actions == sorted(actions, key=lambda a: 0) and page["audit_log_entries"][0]["action_type"] == 62)
print("entry has user and target", all(e["user_id"] == own["user"]["id"] for e in page["audit_log_entries"]))
kick = next(e for e in page["audit_log_entries"] if e["action_type"] == 20)
print("kick target", kick["target_id"] == victim["user"]["id"], "users listed", any(u["id"] == own["user"]["id"] for u in page["users"]))
ch_del = next(e for e in page["audit_log_entries"] if e["action_type"] == 12)
print("channel delete keeps old name", ch_del["changes"][0]["old_value"] == "audit-chan")
s, filt = req("GET", f"/guilds/{gid}/audit-logs?action_type=60", token=to)
print("filter by action ->", [e["action_type"] for e in filt["audit_log_entries"]] == [60])
s, p1 = req("GET", f"/guilds/{gid}/audit-logs?limit=2", token=to)
s, p2 = req("GET", f"/guilds/{gid}/audit-logs?limit=2&before={p1['audit_log_entries'][-1]['id']}", token=to)
print("paging continues", p2["audit_log_entries"][0]["id"] != p1["audit_log_entries"][-1]["id"] and len(p2["audit_log_entries"]) == 2)
print("member without permission ->", req("GET", f"/guilds/{gid}/audit-logs", token=tm)[0])
