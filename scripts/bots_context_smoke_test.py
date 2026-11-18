# Bot DMs, autocomplete and user-installed apps against a running steeld.
# Usage: python3 scripts/bots_context_smoke_test.py http://127.0.0.1:8080
# Requires: pip install websockets
import asyncio, json, sys, time, urllib.request, urllib.error, websockets

BASE = sys.argv[1]
API = BASE + "/api/v10"
WS = BASE.replace("http", "ws") + "/gateway"
RUN = str(int(time.time()))[-6:]
failures = []


def check(label, ok):
    print(("PASS " if ok else "FAIL ") + label)
    if not ok:
        failures.append(label)


def req(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = token
    r = urllib.request.Request(API + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read()
        return e.code, (json.loads(raw) if raw else None)


def reg(name):
    return req("POST", "/auth/register", {"username": f"{name}_{RUN}", "password": "password123"})[1]


async def gateway(token):
    ws = await websockets.connect(WS)
    await ws.recv()
    await ws.send(json.dumps({"op": 2, "d": {"token": token}}))
    ready = json.loads(await ws.recv())
    assert ready.get("t") == "READY", ready
    return ws


async def wait_for(ws, event, predicate=lambda d: True, timeout=3):
    deadline = time.time() + timeout
    while time.time() < deadline:
        ev = json.loads(await asyncio.wait_for(ws.recv(), max(0.1, deadline - time.time())))
        if ev.get("t") == event and predicate(ev.get("d") or {}):
            return ev["d"]
    raise TimeoutError(event)


async def main():
    owner, member, outsider = reg("ctxown"), reg("ctxmem"), reg("ctxout")
    to, tm, tx = owner["token"], member["token"], outsider["token"]
    mid, xid, oid = member["user"]["id"], outsider["user"]["id"], owner["user"]["id"]

    g = req("POST", "/guilds", {"name": "Ctx"}, to)[1]
    gid, ch = g["id"], g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]
    req("POST", f"/invites/{inv}", token=tm)

    app = req("POST", "/applications", {"name": "Ctx Bot"}, to)[1]
    aid, bot_id = app["id"], app["bot"]["id"]
    btoken = "Bot " + app["token"]
    req("PUT", f"/applications/{aid}/guilds/{gid}", token=to)
    ws_bot = await gateway(btoken)
    ws_mem = await gateway(tm)
    ws_out = await gateway(tx)

    s, ping = req("POST", f"/applications/{aid}/commands", {"name": "ping", "description": "Ping"}, btoken)
    s, echo = req("POST", f"/applications/{aid}/commands", {"name": "echo", "description": "Echo", "options": [
        {"type": 3, "name": "text", "description": "Text", "autocomplete": True}]}, btoken)
    check("bot registers commands", s == 201 and ping["name"] == "ping" and echo["name"] == "echo")

    # Bot DM: a user opens a DM with the bot, then uses its commands there.
    s, dm = req("POST", "/users/@me/channels", {"recipient_id": bot_id}, tm)
    check("user opens a DM with the bot", s in (200, 201) and dm["id"])
    dm_id = dm["id"]
    s, index = req("GET", f"/channels/{dm_id}/application-command-index", token=tm)
    check("DM index lists the bot's global commands", sorted(c["name"] for c in index["application_commands"]) == ["echo", "ping"])
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": dm_id, "nonce": "101",
                                          "data": {"id": ping["id"], "name": "ping", "options": []}}, tm)
    check("command in DM accepted", s == 204)
    created = await wait_for(ws_bot, "INTERACTION_CREATE")
    check("DM interaction carries user, not member", "user" in created and "member" not in created and "guild_id" not in created)
    await wait_for(ws_mem, "INTERACTION_SUCCESS", lambda d: d.get("nonce") == "101")
    req("POST", f"/interactions/{created['id']}/{created['token']}/callback", {"type": 4, "data": {"content": "dm pong"}})
    m = await wait_for(ws_mem, "MESSAGE_CREATE", lambda d: d.get("content") == "dm pong")
    check("bot reply arrives in the DM", m["author"]["bot"] is True and m["channel_id"] == dm_id)
    req("POST", f"/channels/{dm_id}/messages", {"content": "hi bot"}, tm)
    dm_msg = await wait_for(ws_bot, "MESSAGE_CREATE", lambda d: d.get("content") == "hi bot")
    check("plain DM message reaches the bot", dm_msg["channel_id"] == dm_id)
    s, _ = req("GET", f"/channels/{dm_id}/application-command-index", token=tx)
    check("outsider cannot read the DM index", s == 403)
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": dm_id,
                                          "data": {"id": ping["id"], "name": "ping", "options": []}}, tx)
    check("outsider cannot run commands in the DM", s == 403)

    # Autocomplete: the invoker's focused option gets choices back.
    s, _ = req("POST", "/interactions", {"type": 4, "application_id": aid, "channel_id": ch, "nonce": "5005",
                                          "data": {"id": echo["id"], "name": "echo", "options": [
                                              {"type": 3, "name": "text", "value": "he", "focused": True}]}}, tm)
    check("autocomplete request accepted", s == 204)
    ac = await wait_for(ws_bot, "INTERACTION_CREATE", lambda d: d.get("type") == 4)
    check("bot sees the focused option", ac["data"]["options"][0]["focused"] is True)
    s, _ = req("POST", f"/interactions/{ac['id']}/{ac['token']}/callback", {"type": 8, "data": {"choices": [
        {"name": "hello", "value": "hello"}, {"name": "help", "value": "help"}]}})
    check("autocomplete choices accepted", s == 204)
    res = await wait_for(ws_mem, "INTERACTION_AUTOCOMPLETE_RESULT")
    check("choices reach the invoker with the nonce", res["nonce"] == "5005" and [c["name"] for c in res["choices"]] == ["hello", "help"])
    s, again = req("POST", f"/interactions/{ac['id']}/{ac['token']}/callback", {"type": 8, "data": {"choices": []}})
    check("second autocomplete answer refused", s == 400 and again["code"] == 40060)
    s, _ = req("POST", "/interactions", {"type": 4, "application_id": aid, "channel_id": ch, "nonce": "5006",
                                          "data": {"id": echo["id"], "name": "echo", "options": [
                                              {"type": 3, "name": "text", "value": "x", "focused": True}]}}, tm)
    ac2 = await wait_for(ws_bot, "INTERACTION_CREATE", lambda d: d.get("type") == 4)
    s, _ = req("POST", f"/interactions/{ac2['id']}/{ac2['token']}/callback", {"type": 4, "data": {"content": "no"}})
    check("autocomplete only accepts type 8", s == 400)
    s, _ = req("POST", f"/interactions/{ac2['id']}/{ac2['token']}/callback", {"type": 8, "data": {"choices": [{"name": "", "value": "x"}]}})
    check("empty choice name refused", s == 400)
    s, _ = req("POST", f"/interactions/{ac2['id']}/{ac2['token']}/callback", {"type": 8, "data": {"choices": [{"name": "ok", "value": "ok"}]}})
    check("valid choices accepted after a refused one", s == 204)

    # User installs: an outsider who is not in the guild installs the app for their own DM.
    s, dm2 = req("POST", "/users/@me/channels", {"recipient_id": oid}, tx)
    dm2_id = dm2["id"]
    s, index = req("GET", f"/channels/{dm2_id}/application-command-index", token=tx)
    check("no commands before install", index["application_commands"] == [])
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": dm2_id,
                                          "data": {"id": ping["id"], "name": "ping", "options": []}}, tx)
    check("uninstalled user cannot run the command", s == 404)
    check("user installs the app", req("PUT", f"/users/@me/applications/{aid}", token=tx)[0] == 204)
    s, index = req("GET", f"/channels/{dm2_id}/application-command-index", token=tx)
    check("installed commands appear in the user's DM", sorted(c["name"] for c in index["application_commands"]) == ["echo", "ping"])
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": dm2_id, "nonce": "202",
                                          "data": {"id": ping["id"], "name": "ping", "options": []}}, tx)
    check("installed user runs the command", s == 204)
    inst = await wait_for(ws_bot, "INTERACTION_CREATE", lambda d: d.get("channel_id") == dm2_id)
    check("installed interaction names the user", inst["user"]["id"] == xid)
    check("user uninstalls the app", req("DELETE", f"/users/@me/applications/{aid}", token=tx)[0] == 204)
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": dm2_id,
                                          "data": {"id": ping["id"], "name": "ping", "options": []}}, tx)
    check("uninstalled user is refused again", s == 404)

    # Guild commands still need the bot in the guild.
    s, gcmd = req("POST", f"/applications/{aid}/guilds/{gid}/commands", {"name": "gping", "description": "Guild ping"}, btoken)
    req("DELETE", f"/applications/{aid}/guilds/{gid}", token=to)
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": ch, "nonce": "303",
                                          "data": {"id": gcmd["id"], "name": "gping", "options": []}}, tm)
    check("guild command refused once the bot leaves", s == 404)

    for w in (ws_bot, ws_mem, ws_out):
        await w.close()
    print("\nFAILURES:" if failures else "\nAll checks passed.", failures or "")
    sys.exit(1 if failures else 0)


asyncio.run(main())
