# Smoke test for a running steeld. Usage: STEEL_URL=http://127.0.0.1:8080 python3 scripts/smoke_test.py
# Requires: pip install websockets
import asyncio, json, urllib.request, websockets
import os
BASE=os.environ.get("STEEL_URL","http://127.0.0.1:8099")
B=BASE+"/api/v10"
def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(B+path, data=data, method=method, headers={"content-type":"application/json", **({"authorization":"Bearer "+token} if token else {})})
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read(); return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")
def must(res, ok=(200,201,204)):
    assert res[0] in ok, res; return res[1]
alice = must(req("POST","/auth/register",{"username":"alice_e2e","password":"password123"}))["token"]
bob = must(req("POST","/auth/register",{"username":"bob_e2e","password":"password123"}))["token"]
guild = must(req("POST","/guilds",{"name":"Spark Club"}, alice))
gid = guild["id"]; chan = guild["channels"][0]["id"]
print("guild", guild["name"], "channels", [c["name"] for c in guild["channels"]])
inv = must(req("POST",f"/channels/{chan}/invites",{},alice))
print("invite", inv["code"])
async def main():
    async with websockets.connect(BASE.replace("http","ws")+"/gateway") as ws:
        await ws.recv(); await ws.send(json.dumps({"op":2,"d":{"token":bob}}))
        ready = json.loads(await ws.recv()); print("bob READY guilds:", len(ready["d"]["guilds"]))
        joined = must(req("POST",f"/invites/{inv['code']}",token=bob))
        print("bob joined", joined["name"])
        ev = json.loads(await ws.recv()); print("bob got", ev["t"])
        m = must(req("POST",f"/channels/{chan}/messages",{"content":"hello from alice"},alice))
        ev = json.loads(await ws.recv()); print("bob got", ev["t"], ev["d"]["content"], ev["d"]["author"]["username"])
        must(req("POST",f"/channels/{chan}/typing",token=bob))
        ev = json.loads(await ws.recv()); print("typing event", ev["t"])
        r = must(req("POST",f"/channels/{chan}/messages",{"content":"reply","message_reference":{"message_id":m["id"]}},bob))
        print("reply ref", r["message_reference"]["message_id"] == m["id"])
        ev = json.loads(await ws.recv()); print("reply create event", ev["t"])
        upd = must(req("PATCH",f"/channels/{chan}/messages/{m['id']}",{"content":"edited"},alice))
        ev = json.loads(await ws.recv()); print("update event", ev["t"], ev["d"]["content"], ev["d"]["edited_timestamp"] is not None)
        print("bob edit someone else's ->", req("PATCH",f"/channels/{chan}/messages/{m['id']}",{"content":"x"},bob)[0])
        hist = must(req("GET",f"/channels/{chan}/messages?limit=10",token=bob))
        print("history", [h["content"] for h in hist])
        ch2 = must(req("POST",f"/guilds/{gid}/channels",{"name":"random"},alice))
        print("channel create by member ->", req("POST",f"/guilds/{gid}/channels",{"name":"nope"},bob)[0])
        must(req("DELETE",f"/channels/{chan}/messages/{m['id']}",token=alice))
        while True:
            ev = json.loads(await ws.recv())
            if ev["t"] == "MESSAGE_DELETE": break
        print("delete event", ev["t"], ev["d"]["id"]==m["id"])
        print("stranger guild access ->", req("GET",f"/guilds/{gid}", token=must(req("POST","/auth/register",{"username":"carol_e2e","password":"password123"}))["token"])[0])
        print("empty content ->", req("POST",f"/channels/{chan}/messages",{"content":"   "},alice)[0])
asyncio.run(main())
