# Custom emoji smoke test for a running steeld. Usage: python3 scripts/emojis_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, time, urllib.request, urllib.error, websockets
BASE = sys.argv[1]; API = BASE + "/api/v10"; WS = BASE.replace("http", "ws") + "/gateway"
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
async def gateway(token):
    ws = await websockets.connect(WS); await ws.recv()
    await ws.send(json.dumps({"op": 2, "d": {"token": token}})); await ws.recv(); return ws
async def wait_for(ws, event, timeout=3):
    while True:
        ev = json.loads(await asyncio.wait_for(ws.recv(), timeout))
        if ev.get("t") == event: return ev
async def main():
    own, mem = reg("emown"), reg("ememb")
    to, tm = own["token"], mem["token"]
    g = req("POST", "/guilds", {"name": "Emoji"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]; req("POST", f"/invites/{inv}", token=tm)

    ws_m = await gateway(tm)
    s, e = req("POST", f"/guilds/{gid}/emojis", {"name": "wave_1", "image": PNG}, to)
    print("create ->", s, e.get("name") == "wave_1", e.get("animated") is False)
    ev = await wait_for(ws_m, "GUILD_EMOJIS_UPDATE"); print("member sees list update", [x["name"] for x in ev["d"]["emojis"]] == ["wave_1"])
    print("member create ->", req("POST", f"/guilds/{gid}/emojis", {"name": "nope1", "image": PNG}, tm)[0])
    print("duplicate name ->", req("POST", f"/guilds/{gid}/emojis", {"name": "wave_1", "image": PNG}, to)[0])
    print("bad name ->", req("POST", f"/guilds/{gid}/emojis", {"name": "no-dash", "image": PNG}, to)[0])
    print("jpeg refused ->", req("POST", f"/guilds/{gid}/emojis", {"name": "jpg_1", "image": "data:image/jpeg;base64,AAAA"}, to)[0])
    print("list as member ->", [x["name"] for x in req("GET", f"/guilds/{gid}/emojis", token=tm)[1]])
    print("rename ->", req("PATCH", f"/guilds/{gid}/emojis/{e['id']}", {"name": "wave_2"}, to)[1].get("name"))
    print("served image ->", urllib.request.urlopen(f"{BASE}/cdn/emojis/{e['id']}.png").headers.get("content-type"))
    print("delete by member ->", req("DELETE", f"/guilds/{gid}/emojis/{e['id']}", token=tm)[0])
    print("delete by owner ->", req("DELETE", f"/guilds/{gid}/emojis/{e['id']}", token=to)[0])
    print("list empty ->", req("GET", f"/guilds/{gid}/emojis", token=to)[1] == [])
    try:
        urllib.request.urlopen(f"{BASE}/cdn/emojis/{e['id']}.png"); print("served after delete -> 200 (bad)")
    except urllib.error.HTTPError as err:
        print("served after delete ->", err.code)
    await ws_m.close()
asyncio.run(main())
