# Peer-to-peer call signaling smoke test for a running steeld.
# Usage: python3 scripts/calls_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, urllib.request, websockets
BASE = sys.argv[1]; API = BASE + "/api/v10"

def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(API + path, data=data, method=method, headers={"content-type": "application/json", **({"authorization": "Bearer " + token} if token else {})})
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read(); return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")

def reg(n): return req("POST", "/auth/register", {"username": n, "password": "password123"})[1]

async def recv_until(ws, event, timeout=3):
    while True:
        ev = json.loads(await asyncio.wait_for(ws.recv(), timeout))
        if ev.get("t") == event: return ev

async def main():
    a, b, c = reg("cs_a"), reg("cs_b"), reg("cs_c")
    ta, tb, tc = a["token"], b["token"], c["token"]; ida, idb = a["user"]["id"], b["user"]["id"]
    print("friends", req("POST", "/users/@me/relationships", {"username": "cs_b"}, ta)[0], req("PUT", f"/users/@me/relationships/{ida}", {}, tb)[0])
    ws_url = BASE.replace("http", "ws") + "/gateway"
    async with websockets.connect(ws_url) as wb:
        await wb.recv(); await wb.send(json.dumps({"op": 2, "d": {"token": tb}})); await wb.recv()
        s, _ = req("POST", "/calls/signal", {"to": idb, "call_id": "call-1", "kind": "offer", "payload": {"sdp": "v=0"}}, ta)
        print("friend offer ->", s)
        ev = await recv_until(wb, "CALL_SIGNAL")
        d = ev["d"]; print("bob got offer", d["kind"] == "offer", d["from"]["username"] == "cs_a", d["payload"]["sdp"] == "v=0")
        s, _ = req("POST", "/calls/signal", {"to": idb, "call_id": "call-1", "kind": "offer", "payload": {}}, tc)
        print("stranger offer (expect 403) ->", s)
    print("bad kind (expect 400) ->", req("POST", "/calls/signal", {"to": idb, "call_id": "x", "kind": "nope"}, ta)[0])
    print("bad call id (expect 400) ->", req("POST", "/calls/signal", {"to": idb, "call_id": "a b", "kind": "ice"}, ta)[0])
    print("self (expect 400) ->", req("POST", "/calls/signal", {"to": ida, "call_id": "c", "kind": "ice"}, ta)[0])
    print("no auth (expect 401) ->", req("POST", "/calls/signal", {"to": idb, "call_id": "c", "kind": "ice"})[0])
    s, cfg = req("GET", "/calls/ice-servers", None, ta)
    print("ice servers", s, bool(cfg and cfg.get("iceServers")))

asyncio.run(main())
