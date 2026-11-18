# DM and relationship smoke test for a running steeld. Usage: python3 scripts/dm_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, urllib.request, websockets
BASE=sys.argv[1]; B=BASE+"/api/v10"
def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(B+path, data=data, method=method, headers={"content-type":"application/json", **({"authorization":"Bearer "+token} if token else {})})
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read(); return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")
def reg(n): return req("POST","/auth/register",{"username":n,"password":"password123"})[1]
a, b, c = reg("dm_a"), reg("dm_b"), reg("dm_c")
ta, tb, tc = a["token"], b["token"], c["token"]
ida, idb, idc = a["user"]["id"], b["user"]["id"], c["user"]["id"]
async def main():
    async with websockets.connect(BASE.replace("http","ws")+"/gateway") as wsb:
        await wsb.recv(); await wsb.send(json.dumps({"op":2,"d":{"token":tb}}))
        ready = json.loads(await wsb.recv()); print("bob READY private_channels:", len(ready["d"]["private_channels"]))
        s, dm = req("POST","/users/@me/channels",{"recipient_id":idb},ta); print("create dm", s, dm["type"] if s==200 else dm, [r["username"] for r in dm["recipients"]])
        s2, dm2 = req("POST","/users/@me/channels",{"recipient_id":idb},ta); print("reuse dm same id", dm2["id"]==dm["id"])
        ev = json.loads(await wsb.recv()); print("bob got", ev["t"])
        m = req("POST",f"/channels/{dm['id']}/messages",{"content":"dm hi"},ta)[1]
        ev = json.loads(await wsb.recv()); print("bob got", ev["t"], ev["d"]["content"])
        print("carol can read dm ->", req("GET",f"/channels/{dm['id']}/messages",token=tc)[0])
        print("bob reads dm ->", req("GET",f"/channels/{dm['id']}/messages",token=tb)[0])
        g = req("POST","/users/@me/channels",{"recipients":[idb,idc],"name":"trio"},ta)[1]
        print("group dm type", g["type"], "recipients", len(g["recipients"]), "name", g.get("name"))
        print("bob sees group in ready? (separate socket later)")
        # friends
        print("friend request a->b", req("POST","/users/@me/relationships",{"username":"dm_b"},ta)[0])
        rel_b = req("GET","/users/@me/relationships",token=tb)[1]; print("b sees", [(r["user"]["username"], r["type"]) for r in rel_b])
        print("b accepts", req("PUT",f"/users/@me/relationships/{ida}",{},tb)[0])
        print("a list", [(r["user"]["username"], r["type"]) for r in req("GET","/users/@me/relationships",token=ta)[1]])
        print("a blocks c", req("PUT",f"/users/@me/relationships/{idc}",{"type":2},ta)[0])
        print("c tries dm with a ->", req("POST","/users/@me/channels",{"recipient_id":ida},tc)[0])
        print("a unblocks c", req("DELETE",f"/users/@me/relationships/{idc}",token=ta)[0])
        print("a self friend ->", req("POST","/users/@me/relationships",{"username":"dm_a"},ta)[0])
        print("unknown user dm ->", req("POST","/users/@me/channels",{"recipient_id":"1"},ta)[0])
asyncio.run(main())
