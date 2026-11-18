# Settings-proto smoke test for a running steeld. Usage: python3 scripts/settings_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, time, base64, urllib.request, urllib.error, websockets
BASE = sys.argv[1]; API = BASE + "/api/v10"; WS = BASE.replace("http", "ws") + "/gateway"
RUN = str(int(time.time()))[-6:]
def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(API + path, data=data, method=method, headers={"content-type": "application/json", **({"authorization": "Bearer " + token} if token else {})})
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read(); return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")
def b64(raw): return base64.b64encode(raw).decode()
async def gateway(token):
    ws = await websockets.connect(WS); await ws.recv()
    await ws.send(json.dumps({"op": 2, "d": {"token": token}})); await ws.recv(); return ws
async def wait_for(ws, event, timeout=3):
    while True:
        ev = json.loads(await asyncio.wait_for(ws.recv(), timeout))
        if ev.get("t") == event: return ev
async def main():
    me = req("POST", "/auth/register", {"username": f"set_{RUN}", "password": "password123"})[1]
    t = me["token"]
    ws = await gateway(t)
    s, cur = req("GET", "/users/@me/settings-proto/1", token=t)
    print("default blob ->", s, cur["settings"] == "CAA=")
    v1 = b64(bytes([0x08, 0x01, 0x12, 0x03, 0x61, 0x62, 0x63]))  # version 1, plus an opaque field 2
    print("write version 1 ->", req("PATCH", "/users/@me/settings-proto/1", {"settings": v1, "required_data_version": 0}, t)[0])
    ev = await wait_for(ws, "USER_SETTINGS_PROTO_UPDATE"); print("event carries blob", ev["d"]["settings"]["type"] == 1 and ev["d"]["settings"]["proto"] == v1)
    print("stale write ->", req("PATCH", "/users/@me/settings-proto/1", {"settings": v1, "required_data_version": 0}, t)[0])
    s, out = req("PATCH", "/users/@me/settings-proto/1", {"settings": b64(bytes([0x08, 0x01])), "required_data_version": 0}, t)
    print("stale returns out_of_date", s, out.get("out_of_date") is True, out.get("settings") == v1)
    print("version must increase ->", req("PATCH", "/users/@me/settings-proto/1", {"settings": v1, "required_data_version": 1}, t)[0])
    print("bad base64 ->", req("PATCH", "/users/@me/settings-proto/1", {"settings": "!!", "required_data_version": 1}, t)[0])
    v2 = b64(bytes([0x08, 0x02]))
    print("write version 2 ->", req("PATCH", "/users/@me/settings-proto/1", {"settings": v2, "required_data_version": 1}, t)[0])
    s, cur = req("GET", "/users/@me/settings-proto/1", token=t)
    print("read back ->", cur["settings"] == v2)
    print("kinds are separate ->", req("GET", "/users/@me/settings-proto/2", token=t)[1]["settings"] == "CAA=")
    print("unknown kind ->", req("GET", "/users/@me/settings-proto/9", token=t)[0])
    print("no auth ->", req("GET", "/users/@me/settings-proto/1")[0])
    await ws.close()
asyncio.run(main())
