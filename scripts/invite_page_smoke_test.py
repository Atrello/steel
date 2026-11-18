# Smoke test for GET /invite/{code} against a running steeld.
# Usage: python3 scripts/invite_page_smoke_test.py http://127.0.0.1:8080
import html, json, sys, time, urllib.error, urllib.request

BASE = sys.argv[1].rstrip("/")
API = BASE + "/api/v10"
RUN = str(int(time.time()))[-6:]
failures = []


def check(label, ok):
    print(("PASS " if ok else "FAIL ") + label)
    if not ok:
        failures.append(label)


def call(method, url, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = "Bearer " + token
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, resp.headers.get("content-type", ""), resp.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("content-type", ""), e.read().decode()


status, _, raw = call("POST", API + "/auth/register", {"username": f"inv_{RUN}", "password": "password123"})
token = json.loads(raw)["token"]
guild_name = f"Invite & <Club> {RUN}"
status, _, raw = call("POST", API + "/guilds", {"name": guild_name}, token)
guild = json.loads(raw)
channel = guild["channels"][0]["id"]
status, _, raw = call("POST", f"{API}/channels/{channel}/invites", {}, token)
code = json.loads(raw)["code"]

status, ctype, body = call("GET", f"{BASE}/invite/{code}")
check("valid invite returns 200", status == 200)
check("valid invite is HTML", ctype.startswith("text/html"))
check("page names the server, escaped", html.escape(guild_name, quote=True) in body)
check("page shows member count", "1 member" in body)
check("page explains how to join in Flint", "Join a server" in body and code in body)

status, ctype, body = call("GET", f"{BASE}/invite/doesnotexist{RUN}")
check("unknown invite returns 404", status == 404)
check("unknown invite is HTML", ctype.startswith("text/html"))

if failures:
    print("FAILED:", failures)
    sys.exit(1)
print("ALL CHECKS PASSED")
