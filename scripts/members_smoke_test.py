# Membership, moderation and read-state smoke test for a running steeld. Usage: python3 scripts/members_smoke_test.py http://127.0.0.1:8080
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
def reg(n):
    return req("POST", "/auth/register", {"username": f"{n}_{RUN}", "password": "password123"})[1]
async def gateway(token):
    ws = await websockets.connect(WS); await ws.recv()
    await ws.send(json.dumps({"op": 2, "d": {"token": token}})); return ws, json.loads(await ws.recv())
async def wait_for(ws, event, timeout=3):
    while True:
        ev = json.loads(await asyncio.wait_for(ws.recv(), timeout))
        if ev.get("t") == event: return ev
async def main():
    own, mem, dm3 = reg("own"), reg("mem"), reg("dm3")
    to, tm, t3 = own["token"], mem["token"], dm3["token"]
    oid, mid, id3 = own["user"]["id"], mem["user"]["id"], dm3["user"]["id"]
    g = req("POST", "/guilds", {"name": "Mods"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]

    print("join", req("POST", f"/invites/{inv}", token=tm)[0])
    print("get member", req("GET", f"/guilds/{gid}/members/{mid}", token=to)[0])
    ws_o, _ = await gateway(to)

    s, m = req("PATCH", f"/guilds/{gid}/members/{mid}", {"nick": "  Mo  "}, tm)
    print("self nick", s, m["nick"])
    ev = await wait_for(ws_o, "GUILD_MEMBER_UPDATE"); print("owner sees nick update", ev["d"]["nick"])
    print("member edits owner nick ->", req("PATCH", f"/guilds/{gid}/members/{oid}", {"nick": "x"}, tm)[0])
    print("owner edits member nick ->", req("PATCH", f"/guilds/{gid}/members/{mid}", {"nick": None}, to)[0])
    print("mute via PATCH (phase 3) ->", req("PATCH", f"/guilds/{gid}/members/{mid}", {"mute": True}, to)[0])

    print("member kicks owner ->", req("DELETE", f"/guilds/{gid}/members/{oid}", token=tm)[0])
    ws_m, _ = await gateway(tm)
    print("owner kicks member ->", req("DELETE", f"/guilds/{gid}/members/{mid}", token=to)[0])
    gone = await wait_for(ws_m, "GUILD_DELETE"); print("kicked user gets GUILD_DELETE", gone["d"]["id"] == gid)
    print("kicked user reads channel ->", req("GET", f"/channels/{ch}", token=tm)[0])
    print("rejoin after kick ->", req("POST", f"/invites/{inv}", token=tm)[0])

    print("ban ->", req("PUT", f"/guilds/{gid}/bans/{mid}", token=to)[0])
    print("banned user rejoins ->", req("POST", f"/invites/{inv}", token=tm)[0])
    print("unban ->", req("DELETE", f"/guilds/{gid}/bans/{mid}", token=to)[0])
    print("rejoin after unban ->", req("POST", f"/invites/{inv}", token=tm)[0])
    print("member leaves ->", req("DELETE", f"/users/@me/guilds/{gid}", token=tm)[0])
    print("owner leaves own guild ->", req("DELETE", f"/users/@me/guilds/{gid}", token=to)[0])

    # DMs
    d = req("POST", "/users/@me/channels", {"recipients": [mid, id3], "name": "trio"}, to)[1]
    print("group dm", d["type"], len(d["recipients"]))
    ws_3, _ = await gateway(t3)
    print("member leaves group ->", req("DELETE", f"/channels/{d['id']}/recipients/@me", token=tm)[0])
    rem = await wait_for(ws_3, "CHANNEL_RECIPIENT_REMOVE"); print("remaining member sees removal", rem["d"]["user"]["id"] == mid)
    print("leaver can read group ->", req("GET", f"/channels/{d['id']}", token=tm)[0])
    print("leave group again ->", req("DELETE", f"/channels/{d['id']}/recipients/@me", token=tm)[0])
    one = req("POST", "/users/@me/channels", {"recipient_id": oid}, tm)[1]
    print("1:1 leave ->", req("DELETE", f"/channels/{one['id']}/recipients/@me", token=tm)[0])
    print("owner still sees 1:1 ->", req("GET", f"/channels/{one['id']}", token=to)[0])

    # read state
    m1 = req("POST", f"/channels/{ch}/messages", {"content": "a"}, to)[1]
    m2 = req("POST", f"/channels/{ch}/messages", {"content": "b"}, to)[1]
    print("guild ack ->", req("POST", f"/guilds/{gid}/ack", token=to)[0])
    print("bulk ack ->", req("POST", "/read-states/ack-bulk", {"read_states": [{"channel_id": ch, "message_id": m2["id"], "read_state_type": 0}]}, to)[0])
    print("bulk ack no access ->", req("POST", "/read-states/ack-bulk", {"read_states": [{"channel_id": one["id"], "message_id": "1"}]}, t3)[0])
    print("bulk ack too many ->", req("POST", "/read-states/ack-bulk", {"read_states": [{"channel_id": ch, "message_id": m1["id"]}] * 101}, to)[0])
    await ws_o.close(); await ws_m.close(); await ws_3.close()

asyncio.run(main())
