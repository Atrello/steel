# Gateway Resume smoke test for a running steeld. Usage: python3 scripts/resume_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, time, urllib.request, websockets
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
async def recv(ws, timeout=3):
    return json.loads(await asyncio.wait_for(ws.recv(), timeout))
async def identify(token):
    ws = await websockets.connect(WS)
    hello = await recv(ws)
    await ws.send(json.dumps({"op": 2, "d": {"token": token}}))
    ready = await recv(ws)
    return ws, ready
async def main():
    a = req("POST", "/auth/register", {"username": f"rs_a_{RUN}", "password": "password123"})[1]
    b = req("POST", "/auth/register", {"username": f"rs_b_{RUN}", "password": "password123"})[1]
    ta, tb = a["token"], b["token"]
    g = req("POST", "/guilds", {"name": "Resume"}, ta)[1]
    ch = g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, ta)[1]
    req("POST", f"/invites/{inv['code']}", token=tb)

    ws_b, ready = await identify(tb)
    session_id = ready["d"]["session_id"]
    print("READY seq", ready["s"], "guilds", len(ready["d"]["guilds"]))
    req("POST", f"/channels/{ch}/messages", {"content": "live 1"}, ta)
    live = await recv(ws_b)
    while live["t"] != "MESSAGE_CREATE":
        live = await recv(ws_b)
    seen = live["s"]
    print("live event seq", seen, live["d"]["content"])
    await ws_b.close()

    # Missed while disconnected.
    req("POST", f"/channels/{ch}/messages", {"content": "missed 1"}, ta)
    req("POST", f"/channels/{ch}/messages", {"content": "missed 2"}, ta)
    await asyncio.sleep(0.2)

    ws_r = await websockets.connect(WS)
    await recv(ws_r)
    await ws_r.send(json.dumps({"op": 6, "d": {"token": tb, "session_id": session_id, "seq": seen}}))
    replayed = []
    first = await recv(ws_r)
    print("resume answer", first["op"], first.get("t"))
    while len(replayed) < 2:
        ev = await recv(ws_r)
        if ev["t"] == "MESSAGE_CREATE":
            replayed.append((ev["s"], ev["d"]["content"]))
    print("replayed", replayed)
    print("sequence continues", replayed[0][0] == seen + 1)

    req("POST", f"/channels/{ch}/messages", {"content": "after resume"}, ta)
    nxt = await recv(ws_r)
    while nxt["t"] != "MESSAGE_CREATE":
        nxt = await recv(ws_r)
    print("live after resume", nxt["d"]["content"], nxt["s"] == replayed[-1][0] + 1)
    await ws_r.close()

    ws_bad = await websockets.connect(WS); await recv(ws_bad)
    await ws_bad.send(json.dumps({"op": 6, "d": {"token": tb, "session_id": "nope", "seq": 1}}))
    bad = await recv(ws_bad)
    print("unknown session ->", bad["op"], bad["d"])
    await ws_bad.close()

    # Too far behind to replay: the buffer must not have a gap, so asking for a seq ahead of the server is refused.
    ws_ahead = await websockets.connect(WS); await recv(ws_ahead)
    await ws_ahead.send(json.dumps({"op": 6, "d": {"token": tb, "session_id": session_id, "seq": 999999}}))
    ahead = await recv(ws_ahead)
    print("seq ahead ->", ahead["op"], ahead["d"])
    await ws_ahead.close()

asyncio.run(main())
