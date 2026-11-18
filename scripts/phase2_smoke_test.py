# Reactions, pins and search smoke test for a running steeld. Usage: python3 scripts/phase2_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, time, urllib.parse, urllib.request, urllib.error, websockets
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
    own, mem = reg("p2own"), reg("p2mem")
    to, tm, oid, mid = own["token"], mem["token"], own["user"]["id"], mem["user"]["id"]
    g = req("POST", "/guilds", {"name": "Phase2"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]; req("POST", f"/invites/{inv}", token=tm)

    m1 = req("POST", f"/channels/{ch}/messages", {"content": "Hello World"}, to)[1]
    m2 = req("POST", f"/channels/{ch}/messages", {"content": "100% done_ok"}, to)[1]
    m3 = req("POST", f"/channels/{ch}/messages", {"content": "hello again"}, tm)[1]

    ws_o = await gateway(to)
    thumb = urllib.parse.quote("👍", safe="")
    print("react unicode ->", req("PUT", f"/channels/{ch}/messages/{m1['id']}/reactions/{thumb}/@me", token=tm)[0])
    ev = await wait_for(ws_o, "MESSAGE_REACTION_ADD"); print("owner sees add", ev["d"]["emoji"]["name"], ev["d"]["user_id"] == mid)
    print("react custom ->", req("PUT", f"/channels/{ch}/messages/{m1['id']}/reactions/wave%3A123/@me", token=to)[0])
    print("react too long ->", req("PUT", f"/channels/{ch}/messages/{m1['id']}/reactions/{'x'*70}/@me", token=to)[0])
    s, out = req("GET", f"/channels/{ch}/messages?limit=5", token=mem and tm)
    got = next(x for x in out if x["id"] == m1["id"])
    print("history reactions", sorted((r["emoji"]["name"], r["count"], r["me"]) for r in got["reactions"]))
    print("list users", [u["username"] == mem["user"]["username"] for u in req("GET", f"/channels/{ch}/messages/{m1['id']}/reactions/{thumb}", token=to)[1]])
    print("member removes owner's? (not allowed) ->", req("DELETE", f"/channels/{ch}/messages/{m1['id']}/reactions/wave%3A123/{oid}", token=tm)[0])
    print("owner removes member reaction ->", req("DELETE", f"/channels/{ch}/messages/{m1['id']}/reactions/{thumb}/{mid}", token=to)[0])
    ev = await wait_for(ws_o, "MESSAGE_REACTION_REMOVE"); print("owner sees remove", ev["d"]["user_id"] == mid)
    print("remove own ->", req("DELETE", f"/channels/{ch}/messages/{m1['id']}/reactions/wave%3A123/@me", token=to)[0])
    print("clear all by member ->", req("DELETE", f"/channels/{ch}/messages/{m1['id']}/reactions", token=tm)[0])
    print("clear all by owner ->", req("DELETE", f"/channels/{ch}/messages/{m1['id']}/reactions", token=to)[0])
    print("react on missing message ->", req("PUT", f"/channels/{ch}/messages/1/reactions/{thumb}/@me", token=to)[0])

    # pins
    print("member pins ->", req("PUT", f"/channels/{ch}/messages/pins/{m1['id']}", token=tm)[0])
    ws_m = await gateway(tm)
    print("owner pins ->", req("PUT", f"/channels/{ch}/messages/pins/{m1['id']}", token=to)[0])
    ev = await wait_for(ws_m, "CHANNEL_PINS_UPDATE"); print("member sees pin update", ev["d"]["channel_id"] == ch)
    req("PUT", f"/channels/{ch}/messages/pins/{m2['id']}", token=to)
    page = req("GET", f"/channels/{ch}/messages/pins?limit=1", token=tm)[1]
    print("pins page1", len(page["items"]), page["has_more"], page["items"][0]["message"]["id"] == m2["id"])
    page2 = req("GET", f"/channels/{ch}/messages/pins?limit=5&before={urllib.parse.quote(page['items'][0]['pinned_at'])}", token=tm)[1]
    print("pins page2", [i["message"]["id"] == m1["id"] for i in page2["items"]], page2["has_more"])
    print("member unpins ->", req("DELETE", f"/channels/{ch}/messages/pins/{m1['id']}", token=tm)[0])
    print("owner unpins ->", req("DELETE", f"/channels/{ch}/messages/pins/{m1['id']}", token=to)[0])
    print("pins after unpin", [i["message"]["id"] for i in req("GET", f"/channels/{ch}/messages/pins", token=to)[1]["items"]] == [m2["id"]])

    # search
    print("guild search 'hello' (case-insensitive)", sorted(x[0]["id"] for x in req("GET", f"/guilds/{gid}/messages/search?content=hello", token=to)[1]["messages"]) == sorted([m1["id"], m3["id"]]))
    s, out = req("GET", f"/guilds/{gid}/messages/search?content=" + urllib.parse.quote("100%"), token=to)
    print("escaped % matches only the literal", [x[0]["id"] for x in out["messages"]] == [m2["id"]], out["total_results"])
    s, out = req("GET", f"/guilds/{gid}/messages/search?content=hello&limit=1&offset=1", token=to)
    print("paged", len(out["messages"]), out["total_results"])
    print("channel search", req("GET", f"/channels/{ch}/messages/search?content=done", token=tm)[1]["total_results"])
    print("empty query", req("GET", f"/guilds/{gid}/messages/search?content=", token=to)[1]["total_results"])
    stranger = reg("p2stranger")["token"]
    print("stranger search ->", req("GET", f"/guilds/{gid}/messages/search?content=hello", token=stranger)[0])
    await ws_o.close(); await ws_m.close()

asyncio.run(main())
