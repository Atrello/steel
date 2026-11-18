# Member list smoke test for a running steeld. Usage: python3 scripts/member_list_smoke_test.py http://127.0.0.1:8080
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
    own, mem, third = reg("mlown"), reg("mlmem"), reg("mlthird")
    to, tm, t3 = own["token"], mem["token"], third["token"]
    g = req("POST", "/guilds", {"name": "Members"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]
    req("POST", f"/invites/{inv}", token=tm); req("POST", f"/invites/{inv}", token=t3)

    ws = await gateway(to)
    # First subscribe with no channels only sets the guild up, and must not get a reply.
    await ws.send(json.dumps({"op": 37, "d": {"subscriptions": {gid: {"typing": True, "channels": {}}}}}))
    await ws.send(json.dumps({"op": 37, "d": {"subscriptions": {gid: {"typing": True, "channels": {ch: [[0, 99]]}}}}}))
    ev = await wait_for(ws, "GUILD_MEMBER_LIST_UPDATE")
    d = ev["d"]
    names = [i["member"]["user"]["username"] for i in d["ops"][0]["items"]]
    print("list id", d["id"], "count", d["member_count"], "range", d["ops"][0]["range"])
    print("sorted and complete", names == sorted(names, key=str.lower) and len(names) == 3)
    await ws.send(json.dumps({"op": 37, "d": {"subscriptions": {gid: {"typing": False, "channels": {ch: [[1, 1]]}}}}}))
    ev = await wait_for(ws, "GUILD_MEMBER_LIST_UPDATE")
    print("narrow range items", len(ev["d"]["ops"][0]["items"]), ev["d"]["ops"][0]["range"])

    wt = await gateway(t3)
    await wt.send(json.dumps({"op": 37, "d": {"subscriptions": {gid: {"typing": True, "channels": {ch: [[0, 9]]}}}}}))
    ev = await wait_for(wt, "GUILD_MEMBER_LIST_UPDATE")
    print("third member gets list", ev["d"]["member_count"])
    outsider = reg("mloutside")
    wo = await gateway(outsider["token"])
    await wo.send(json.dumps({"op": 37, "d": {"subscriptions": {gid: {"typing": True, "channels": {ch: [[0, 9]]}}}}}))
    try:
        await wait_for(wo, "GUILD_MEMBER_LIST_UPDATE", timeout=1)
        print("outsider got list (bad)")
    except asyncio.TimeoutError:
        print("outsider gets no list")
    for s in (ws, wt, wo): await s.close()
asyncio.run(main())
