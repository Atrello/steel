# Presence and read-state smoke test for a running steeld. Usage: python3 scripts/presence_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, urllib.request, websockets
BASE=sys.argv[1]; API=BASE+"/api/v10"
def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(API+path, data=data, method=method, headers={"content-type":"application/json", **({"authorization":"Bearer "+token} if token else {})})
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read(); return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")
def reg(n): return req("POST","/auth/register",{"username":n,"password":"password123"})[1]
async def recv_until(ws, event, timeout=3):
    while True:
        ev = json.loads(await asyncio.wait_for(ws.recv(), timeout))
        if ev.get("t") == event: return ev
async def main():
    a, b = reg("pr_a"), reg("pr_b")
    ta, tb = a["token"], b["token"]; ida, idb = a["user"]["id"], b["user"]["id"]
    print("friend", req("POST","/users/@me/relationships",{"username":"pr_b"},ta)[0], req("PUT",f"/users/@me/relationships/{ida}",{},tb)[0])
    ws_url = BASE.replace("http","ws")+"/gateway"
    async with websockets.connect(ws_url) as wa:
        await wa.recv(); await wa.send(json.dumps({"op":2,"d":{"token":ta}})); await wa.recv()
        async with websockets.connect(ws_url) as wb:
            await wb.recv(); await wb.send(json.dumps({"op":2,"d":{"token":tb}})); await wb.recv()
            ev = await recv_until(wa, "PRESENCE_UPDATE"); print("alice sees bob", ev["d"]["user"]["id"]==idb, ev["d"]["status"])
            await wb.send(json.dumps({"op":3,"d":{"status":"idle","since":None,"afk":False,"activities":[]}}))
            ev = await recv_until(wa, "PRESENCE_UPDATE"); print("idle ->", ev["d"]["status"])
            await wb.send(json.dumps({"op":3,"d":{"status":"invisible","since":None,"afk":False,"activities":[]}}))
            ev = await recv_until(wa, "PRESENCE_UPDATE"); print("invisible shows as ->", ev["d"]["status"])
        ev = await recv_until(wa, "PRESENCE_UPDATE"); print("bob disconnected ->", ev["d"]["status"])
    # read state
    g = req("POST","/guilds",{"name":"Read Test"},ta)[1]; ch = g["channels"][0]["id"]
    m1 = req("POST",f"/channels/{ch}/messages",{"content":"one"},ta)[1]
    m2 = req("POST",f"/channels/{ch}/messages",{"content":"two"},ta)[1]
    print("ack m2", req("POST",f"/channels/{ch}/messages/{m2['id']}/ack",token=ta)[0])
    print("ack older m1 (should not move back)", req("POST",f"/channels/{ch}/messages/{m1['id']}/ack",token=ta)[0])
    async with websockets.connect(ws_url) as wa:
        await wa.recv(); await wa.send(json.dumps({"op":2,"d":{"token":ta}}))
        ready = json.loads(await wa.recv())
        rs = [r for r in ready["d"]["read_state"] if r["id"]==ch]
        print("READY read_state last ack is m2:", rs and rs[0]["last_message_id"]==m2["id"])
    print("ack in channel without access ->", req("POST",f"/channels/{ch}/messages/{m1['id']}/ack",token=tb)[0])
asyncio.run(main())
