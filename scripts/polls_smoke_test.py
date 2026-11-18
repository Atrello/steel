# Polls smoke test for a running steeld. Usage: python3 scripts/polls_smoke_test.py http://127.0.0.1:8080
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
def poll_body(question, answers, multi=False, duration=24):
    return {"content": "", "poll": {"question": {"text": question}, "answers": [{"poll_media": {"text": a}} for a in answers], "duration": duration, "allow_multiselect": multi, "layout_type": 1}}
async def main():
    own, mem = reg("pollown"), reg("pollmem")
    to, tm, mid = own["token"], mem["token"], mem["user"]["id"]
    g = req("POST", "/guilds", {"name": "Polls"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]; req("POST", f"/invites/{inv}", token=tm)

    s, m = req("POST", f"/channels/{ch}/messages", poll_body("Lunch?", ["Pizza", "Tacos", "Salad"]), to)
    print("create poll ->", s, len(m["poll"]["answers"]) == 3, m["poll"]["results"]["is_finalized"] is False)
    mid_msg = m["id"]
    print("empty poll rejected (1 answer) ->", req("POST", f"/channels/{ch}/messages", {"content": "", "poll": {"question": {"text": "x"}, "answers": [{"poll_media": {"text": "a"}}], "duration": 24, "layout_type": 1}}, to)[0])
    print("too many answers ->", req("POST", f"/channels/{ch}/messages", poll_body("x", [str(i) for i in range(11)]), to)[0])
    print("empty content, no poll ->", req("POST", f"/channels/{ch}/messages", {"content": ""}, to)[0])

    ws_o = await gateway(to)
    print("member votes Tacos ->", req("PUT", f"/channels/{ch}/polls/{mid_msg}/answers/@me", {"answer_ids": [2]}, tm)[0])
    ev = await wait_for(ws_o, "MESSAGE_POLL_VOTE_ADD"); print("owner sees vote add", ev["d"]["answer_id"] == 2, ev["d"]["user_id"] == mid)
    print("single-select two answers ->", req("PUT", f"/channels/{ch}/polls/{mid_msg}/answers/@me", {"answer_ids": [1, 3]}, tm)[0])
    print("unknown answer ->", req("PUT", f"/channels/{ch}/polls/{mid_msg}/answers/@me", {"answer_ids": [9]}, tm)[0])
    print("member switches to Pizza ->", req("PUT", f"/channels/{ch}/polls/{mid_msg}/answers/@me", {"answer_ids": [1]}, tm)[0])
    ev = await wait_for(ws_o, "MESSAGE_POLL_VOTE_REMOVE"); print("owner sees vote remove", ev["d"]["answer_id"] == 2)
    ev = await wait_for(ws_o, "MESSAGE_POLL_VOTE_ADD"); print("owner sees pizza add", ev["d"]["answer_id"] == 1)
    hist = next(x for x in req("GET", f"/channels/{ch}/messages?limit=5", token=tm)[1] if x["id"] == mid_msg)
    counts = {c["id"]: (c["count"], c["me_voted"]) for c in hist["poll"]["results"]["answer_counts"]}
    print("member counts", counts)
    print("owner counts", {c["id"]: c["count"] for c in next(x for x in req("GET", f"/channels/{ch}/messages?limit=5", token=to)[1] if x["id"] == mid_msg)["poll"]["results"]["answer_counts"]})

    print("member expires owner's poll ->", req("POST", f"/channels/{ch}/polls/{mid_msg}/expire", token=tm)[0])
    print("owner expires ->", req("POST", f"/channels/{ch}/polls/{mid_msg}/expire", token=to)[0])
    ev = await wait_for(ws_o, "MESSAGE_UPDATE"); print("owner sees finalized", ev["d"]["poll"]["results"]["is_finalized"])
    print("vote after end ->", req("PUT", f"/channels/{ch}/polls/{mid_msg}/answers/@me", {"answer_ids": [3]}, tm)[0])

    ms, mm = req("POST", f"/channels/{ch}/messages", poll_body("Multi?", ["A", "B", "C"], multi=True), to)
    print("multi vote ->", req("PUT", f"/channels/{ch}/polls/{mm['id']}/answers/@me", {"answer_ids": [1, 3]}, tm)[0])
    print("clear vote ->", req("PUT", f"/channels/{ch}/polls/{mm['id']}/answers/@me", {"answer_ids": []}, tm)[0])
    print("vote on missing poll ->", req("PUT", f"/channels/{ch}/polls/1/answers/@me", {"answer_ids": [1]}, tm)[0])
    await ws_o.close()
asyncio.run(main())
