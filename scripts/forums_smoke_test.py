# Forum posts smoke test for a running steeld. Usage: python3 scripts/forums_smoke_test.py http://127.0.0.1:8080
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
    own, mem = reg("fuown"), reg("fumem")
    to, tm = own["token"], mem["token"]
    g = req("POST", "/guilds", {"name": "Forums"}, to)[1]; gid = g["id"]; text = g["channels"][0]["id"]
    inv = req("POST", f"/channels/{text}/invites", {}, to)[1]["code"]; req("POST", f"/invites/{inv}", token=tm)
    s, forum = req("POST", f"/guilds/{gid}/channels", {"name": "ideas", "type": 15}, to)
    print("create forum ->", s, forum.get("type") == 15)
    fid = forum["id"]

    ws_m = await gateway(tm)
    s, p1 = req("POST", f"/channels/{fid}/threads", {"name": "First idea", "auto_archive_duration": 4320, "message": {"content": "Hello forum"}}, to)
    print("create post ->", s, p1.get("type") == 11, p1.get("parent_id") == fid)
    ev = await wait_for(ws_m, "THREAD_CREATE"); print("member sees THREAD_CREATE", ev["d"]["parent_id"] == fid)
    s, p2 = req("POST", f"/channels/{fid}/threads", {"name": "Second idea", "message": {"content": "Second"}}, tm)
    print("second post by member ->", s)
    print("post without starter ->", req("POST", f"/channels/{fid}/threads", {"name": "empty", "message": {"content": "  "}}, to)[0])
    print("post without message field ->", req("POST", f"/channels/{fid}/threads", {"name": "none"}, to)[0])

    s, page = req("GET", f"/channels/{fid}/threads/search?archived=false&sort_by=last_message_time&sort_order=desc&limit=1&offset=0", token=tm)
    print("search page 1 ->", s, len(page["threads"]), page["has_more"], page["first_messages"][0]["content"] == "Second")
    s, page2 = req("GET", f"/channels/{fid}/threads/search?archived=false&limit=25&offset=0", token=tm)
    ids = {t["id"] for t in page2["threads"]}
    print("search all ->", p1["id"] in ids and p2["id"] in ids, page2["has_more"])
    starter = next(m for m in page2["first_messages"] if m["id"] == p1["id"])
    print("starter message id equals post id", starter["channel_id"] == p1["id"])
    print("search on text channel refused ->", req("GET", f"/channels/{text}/threads/search", token=tm)[0])
    print("post reply in post ->", req("POST", f"/channels/{p1['id']}/messages", {"content": "reply"}, tm)[0])
    ev = await wait_for(ws_m, "MESSAGE_CREATE"); print("member sees reply", ev["d"]["channel_id"] == p1["id"])
    print("message thread on text still works ->", req("POST", f"/channels/{text}/messages/{req('POST', f'/channels/{text}/messages', {'content': 'x'}, to)[1]['id']}/threads", {"name": "t"}, to)[0])
    await ws_m.close()
asyncio.run(main())
