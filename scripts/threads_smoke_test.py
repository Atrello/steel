# Threads smoke test for a running steeld. Usage: python3 scripts/threads_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, time, urllib.request, urllib.error, websockets
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
def reg(n): return req("POST", "/auth/register", {"username": f"{n}_{RUN}", "password": "password123"})[1]
async def gateway(token):
    ws = await websockets.connect(WS); await ws.recv()
    await ws.send(json.dumps({"op": 2, "d": {"token": token}})); await ws.recv(); return ws
async def wait_for(ws, event, timeout=3):
    while True:
        ev = json.loads(await asyncio.wait_for(ws.recv(), timeout))
        if ev.get("t") == event: return ev
async def main():
    own, mem, out = reg("thrown"), reg("thrmem"), reg("throut")
    to, tm, tx = own["token"], mem["token"], out["token"]
    g = req("POST", "/guilds", {"name": "Threads"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]; req("POST", f"/invites/{inv}", token=tm)

    ws_m = await gateway(tm)
    m = req("POST", f"/channels/{ch}/messages", {"content": "Discuss here"}, to)[1]
    s, t = req("POST", f"/channels/{ch}/messages/{m['id']}/threads", {"name": "Launch talk", "auto_archive_duration": 4320}, tm)
    print("thread from message ->", s, t.get("type") == 11, t.get("parent_id") == ch, t.get("id") == m["id"], t.get("thread_metadata", {}).get("archived") is False)
    ev = await wait_for(ws_m, "THREAD_CREATE"); print("member sees THREAD_CREATE", ev["d"]["guild_id"] == gid)
    print("second thread same message ->", req("POST", f"/channels/{ch}/messages/{m['id']}/threads", {"name": "again"}, tm)[1].get("code"))
    print("bad name ->", req("POST", f"/channels/{ch}/messages/{m['id']}/threads", {"name": "   "}, tm)[0])
    print("bad archive window ->", req("POST", f"/channels/{ch}/threads", {"name": "x", "auto_archive_duration": 5}, to)[0])
    s, st = req("POST", f"/channels/{ch}/threads", {"name": "Standalone", "type": 11}, to)
    print("standalone ->", s, st.get("type") == 11)
    print("private unsupported ->", req("POST", f"/channels/{ch}/threads", {"name": "p", "type": 12}, to)[0])
    print("non-member cannot create ->", req("POST", f"/channels/{ch}/threads", {"name": "z"}, tx)[0])

    ids = {x["id"] for x in req("GET", f"/guilds/{gid}/threads/active", token=tm)[1]["threads"]}
    print("active lists both ->", t["id"] in ids and st["id"] in ids)
    guild_now = req("GET", f"/guilds/{gid}", token=tm)[1]
    print("guild channel list excludes threads ->", all(c["type"] not in (10, 11, 12) for c in guild_now["channels"]))
    print("non-member active ->", req("GET", f"/guilds/{gid}/threads/active", token=tx)[0])
    print("archived public empty ->", req("GET", f"/channels/{ch}/threads/archived/public", token=tm)[1]["threads"] == [])
    print("archived private forbidden ->", req("GET", f"/channels/{ch}/threads/archived/private", token=tm)[0])
    print("member private list empty ->", req("GET", f"/channels/{ch}/users/@me/threads/archived/private", token=tm)[1]["threads"] == [])

    ws_m2 = await gateway(tm)
    s, msg = req("POST", f"/channels/{t['id']}/messages", {"content": "Hi thread"}, tm)
    print("post into thread ->", s, msg.get("channel_id") == t["id"])
    ev = await wait_for(ws_m2, "MESSAGE_CREATE"); print("gateway MESSAGE_CREATE in thread", ev["d"]["channel_id"] == t["id"])
    print("get thread channel ->", req("GET", f"/channels/{t['id']}", token=tm)[1].get("thread_metadata") is not None)
    await ws_m.close(); await ws_m2.close()
asyncio.run(main())
