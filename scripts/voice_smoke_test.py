# Voice smoke test for a running steeld (voice gateway, IP discovery, UDP relay, leave).
# Usage: python3 scripts/voice_smoke_test.py http://127.0.0.1:8080 127.0.0.1 50000
# The server must run with STEEL_VOICE_PORT set to the UDP port and STEEL_VOICE_ENDPOINT set to its host:port.
# Requires: pip install websockets
import asyncio, json, os, socket, struct, sys, urllib.request, websockets

BASE = sys.argv[1]
API = BASE + "/api/v10"
VOICE_HOST = sys.argv[2] if len(sys.argv) > 2 else "127.0.0.1"
VOICE_PORT = int(sys.argv[3]) if len(sys.argv) > 3 else int(os.environ.get("STEEL_VOICE_PORT", "50000"))
MODE = "aead_xchacha20_poly1305_rtpsize"
results = []

def check(name, ok):
    results.append(bool(ok))
    print(("PASS " if ok else "FAIL ") + name)

def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(API + path, data=data, method=method, headers={"content-type": "application/json", **({"authorization": "Bearer " + token} if token else {})})
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")

def reg(name):
    return req("POST", "/auth/register", {"username": name, "password": "password123"})[1]

async def recv_event(ws, event, timeout=5):
    while True:
        ev = json.loads(await asyncio.wait_for(ws.recv(), timeout))
        if ev.get("t") == event:
            return ev

async def recv_op(ws, op, timeout=5):
    while True:
        ev = json.loads(await asyncio.wait_for(ws.recv(), timeout))
        if ev.get("op") == op:
            return ev

def discovery(sock, ssrc):
    probe = bytearray(74)
    probe[0:4] = bytes([0, 1, 0, 70])
    probe[4:8] = struct.pack(">I", ssrc)
    sock.sendto(bytes(probe), (VOICE_HOST, VOICE_PORT))
    data, _ = sock.recvfrom(2048)
    return data

async def main():
    a, b = reg("vc_a"), reg("vc_b")
    ta, tb = a["token"], b["token"]
    ida, idb = a["user"]["id"], b["user"]["id"]
    st, dm = req("POST", "/users/@me/channels", {"recipient_id": idb}, ta)
    channel = dm["id"]
    ws_url = BASE.replace("http", "ws") + "/gateway"

    async with websockets.connect(ws_url) as ga, websockets.connect(ws_url) as gb:
        await ga.recv(); await ga.send(json.dumps({"op": 2, "d": {"token": ta}})); await ga.recv()
        await gb.recv(); await gb.send(json.dumps({"op": 2, "d": {"token": tb}})); await gb.recv()

        async def join(gw):
            await gw.send(json.dumps({"op": 4, "d": {"guild_id": None, "channel_id": channel, "self_mute": False, "self_deaf": False, "self_video": False}}))
            # The gateway sends the state update and the server update in either order.
            got = {}
            while len(got) < 2:
                ev = json.loads(await asyncio.wait_for(gw.recv(), 5))
                if ev.get("t") in ("VOICE_SERVER_UPDATE", "VOICE_STATE_UPDATE"):
                    got[ev["t"]] = ev["d"]
            return got["VOICE_SERVER_UPDATE"], got["VOICE_STATE_UPDATE"]
        sa, state_a = await join(ga)
        sb, state_b = await join(gb)
        check("VOICE_SERVER_UPDATE carries token and endpoint", bool(sa["token"]) and bool(sa["endpoint"]))
        check("VOICE_STATE_UPDATE has session id", bool(state_a["session_id"]))

        async def voice_identify(server, state, uid):
            ws = await websockets.connect(f"ws://{server['endpoint']}/?v=8")
            hello = json.loads(await ws.recv())
            check("voice hello (op 8)", hello["op"] == 8)
            await ws.send(json.dumps({"op": 0, "d": {"server_id": server["channel_id"], "user_id": uid, "session_id": state["session_id"], "token": server["token"], "video": False, "streams": [], "max_dave_protocol_version": 1}}))
            ready = await recv_op(ws, 2)
            return ws, ready["d"]

        wa, ready_a = await voice_identify(sa, state_a, ida)
        wb, ready_b = await voice_identify(sb, state_b, idb)
        check("voice ready advertises mode", MODE in ready_a["modes"])
        check("ready port points at UDP", ready_a["port"] == VOICE_PORT)

        udp_a = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); udp_a.settimeout(3)
        udp_b = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); udp_b.settimeout(3)
        reply_a = discovery(udp_a, ready_a["ssrc"])
        discovery(udp_b, ready_b["ssrc"])
        check("IP discovery reply is 74 bytes", len(reply_a) == 74 and reply_a[0:2] == bytes([0, 2]))
        check("IP discovery reports client port", struct.unpack(">H", reply_a[72:74])[0] == udp_a.getsockname()[1])

        for ws in (wa, wb):
            await ws.send(json.dumps({"op": 1, "d": {"protocol": "udp", "data": {"address": "127.0.0.1", "port": 0, "mode": MODE}}}))
        desc_a = await recv_op(wa, 4)
        desc_b = await recv_op(wb, 4)
        key_a, key_b = desc_a["d"]["secret_key"], desc_b["d"]["secret_key"]
        check("session description has 32-byte key", len(key_a) == 32)
        check("both participants get the same transport key", key_a == key_b)
        check("session description mode", desc_a["d"]["mode"] == MODE)

        # Media relay: a's datagram reaches b unchanged, and not back to a.
        payload = bytes([0x80, 0x78]) + os.urandom(98)
        udp_a.sendto(payload, (VOICE_HOST, VOICE_PORT))
        got, _ = udp_b.recvfrom(2048)
        check("relay forwards RTP to the other participant unchanged", got == payload)
        try:
            udp_a.recvfrom(2048)
            check("relay does not echo to the sender", False)
        except socket.timeout:
            check("relay does not echo to the sender", True)

        # Keepalive is echoed back to the sender.
        keepalive = bytes([0x13, 0x37, 0xca, 0xfe, 0, 0, 0, 1])
        udp_a.sendto(keepalive, (VOICE_HOST, VOICE_PORT))
        echo, _ = udp_a.recvfrom(2048)
        check("keepalive is echoed", echo == keepalive)

        # Speaking fans out to the other participant.
        await wa.send(json.dumps({"op": 5, "d": {"speaking": 1, "delay": 0, "ssrc": ready_a["ssrc"]}}))
        speaking = await recv_op(wb, 5)
        check("speaking op reaches the other participant", speaking["d"]["user_id"] == ida)

        # Leaving ends the membership and tells the other participant.
        await ga.send(json.dumps({"op": 4, "d": {"guild_id": None, "channel_id": None, "self_mute": False, "self_deaf": False, "self_video": False}}))
        left = await recv_event(gb, "VOICE_STATE_UPDATE")
        check("leave is announced as channel null", left["d"]["user_id"] == ida and left["d"]["channel_id"] is None)
        disconnect = await recv_op(wb, 13)
        check("other participant sees client disconnect (op 13)", disconnect["d"]["user_id"] == ida)

        for ws in (wa, wb):
            await ws.close()
        udp_a.close(); udp_b.close()

asyncio.run(main())
print(f"{sum(results)}/{len(results)} checks passed")
sys.exit(0 if all(results) else 1)
