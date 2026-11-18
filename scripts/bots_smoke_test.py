# Bot, application command and interaction smoke test for a running steeld.
# Usage: python3 scripts/bots_smoke_test.py http://127.0.0.1:8080
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


def req(method, path, body=None, token=None, prefix=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = token
    r = urllib.request.Request((prefix or API) + path, data=data, method=method, headers=headers)
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
    owner, member, outsider = reg("botown"), reg("botmem"), reg("botout")
    to, tm, tx = owner["token"], member["token"], outsider["token"]
    mid, xid = member["user"]["id"], outsider["user"]["id"]

    g = req("POST", "/guilds", {"name": "Bots"}, to)[1]
    gid, ch = g["id"], g["channels"][0]["id"]
    inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]
    req("POST", f"/invites/{inv}", token=tm)

    # Application and bot account.
    s, app = req("POST", "/applications", {"name": "Echo Bot", "description": "test"}, to)
    check("create application", s == 201 and app.get("token"))
    aid, btoken, bot_id = app["id"], "Bot " + app["token"], app["bot"]["id"]
    check("bot user flagged as bot", app["bot"]["bot"] is True)
    s, me = req("GET", "/users/@me", token=btoken)
    check("bot token authenticates", s == 200 and me["bot"] is True)
    s, _ = req("POST", f"/applications/{aid}/bot/reset", token=to)
    check("owner can reset token", s == 200)
    s, _ = req("GET", "/users/@me", token=btoken)
    check("old bot token is revoked", s == 401)
    btoken = "Bot " + req("POST", f"/applications/{aid}/bot/reset", token=to)[1]["token"]
    s, _ = req("GET", f"/applications/{aid}", token=tm)
    check("non-owner cannot manage application", s == 403)

    # Only the guild owner adds the bot.
    check("member cannot add bot", req("PUT", f"/applications/{aid}/guilds/{gid}", token=tm)[0] == 403)
    check("owner adds bot to guild", req("PUT", f"/applications/{aid}/guilds/{gid}", token=to)[0] == 204)

    # Gateway: owner and member watch, bot identifies with its token.
    ws_bot = await gateway(btoken)
    ws_own = await gateway(to)
    ws_mem = await gateway(tm)

    # Commands: a global one and a guild one.
    s, cmd = req("POST", f"/applications/{aid}/commands", {"name": "ping", "description": "Ping the bot"}, btoken)
    check("bot registers global command", s == 201 and cmd["name"] == "ping")
    s, gcmd = req("POST", f"/applications/{aid}/guilds/{gid}/commands",
                  {"name": "echo", "description": "Echo text", "options": [
                      {"type": 3, "name": "text", "description": "Text to echo", "required": True}]}, btoken)
    check("bot registers guild command", s == 201)
    check("bad command name rejected", req("POST", f"/applications/{aid}/commands", {"name": "Bad Name", "description": "x"}, btoken)[0] == 400)
    check("duplicate command name rejected", req("POST", f"/applications/{aid}/commands", {"name": "ping", "description": "x"}, btoken)[0] == 400)

    s, index = req("GET", f"/guilds/{gid}/application-command-index", token=tm)
    names = sorted(c["name"] for c in index["application_commands"])
    check("member sees both commands in index", names == ["echo", "ping"])
    check("index lists the application", any(a["id"] == aid for a in index["applications"]))
    s, idx_out = req("GET", f"/guilds/{gid}/application-command-index", token=tx)
    check("outsider cannot read index", s == 403)

    # Invocation: member runs /ping.
    nonce = "n-" + RUN
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": ch, "nonce": nonce,
                                          "data": {"id": cmd["id"], "name": "ping", "version": cmd["version"], "options": []}}, tm)
    check("member invokes /ping", s == 204)
    created = await wait_for(ws_bot, "INTERACTION_CREATE")
    check("bot receives INTERACTION_CREATE with token", created["type"] == 2 and created["token"])
    check("member gets INTERACTION_SUCCESS with nonce", (await wait_for(ws_mem, "INTERACTION_SUCCESS"))["nonce"] == nonce)
    ix, itoken = created["id"], created["token"]

    # Outsider cannot invoke.
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": ch,
                                          "data": {"id": cmd["id"], "name": "ping", "options": []}}, tx)
    check("outsider cannot invoke", s == 403)

    # Bot answers with a message carrying a button.
    button = [{"type": 1, "components": [{"type": 2, "style": 1, "label": "Press", "custom_id": "press"}]}]
    s, _ = req("POST", f"/interactions/{ix}/{itoken}/callback",
               {"type": 4, "data": {"content": "pong", "components": button}}, prefix=API)
    check("callback accepted", s == 204)
    msg_ev = await wait_for(ws_mem, "MESSAGE_CREATE", lambda d: d.get("content") == "pong")
    check("MESSAGE_CREATE author is bot", msg_ev["author"]["bot"] is True)
    check("components reach the member", msg_ev["components"][0]["components"][0]["custom_id"] == "press")
    mid_msg = msg_ev["id"]
    s, again = req("POST", f"/interactions/{ix}/{itoken}/callback", {"type": 4, "data": {"content": "x"}})
    check("second callback is refused", s == 400 and again["code"] == 40060)

    # Button press: member presses; bot receives a component interaction.
    s, _ = req("POST", "/interactions", {"type": 3, "application_id": aid, "channel_id": ch, "message_id": mid_msg,
                                          "data": {"custom_id": "press", "component_type": 2}}, tm)
    check("member presses button", s == 204)
    press = await wait_for(ws_bot, "INTERACTION_CREATE", lambda d: d.get("type") == 3)
    check("component interaction carries custom_id", press["data"]["custom_id"] == "press")
    s, _ = req("POST", f"/interactions/{press['id']}/{press['token']}/callback",
               {"type": 7, "data": {"content": "pressed"}})
    check("UPDATE_MESSAGE accepted", s == 204)
    upd = await wait_for(ws_mem, "MESSAGE_UPDATE", lambda d: d.get("id") == mid_msg)
    check("MESSAGE_UPDATE carries new content", upd["content"] == "pressed")

    # Ephemeral: visible to the invoker only.
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": ch,
                                          "data": {"id": cmd["id"], "name": "ping", "options": []}}, tm)
    created2 = await wait_for(ws_bot, "INTERACTION_CREATE", lambda d: d.get("type") == 2)
    s, _ = req("POST", f"/interactions/{created2['id']}/{created2['token']}/callback",
               {"type": 4, "data": {"content": "secret", "flags": 64}})
    eph = await wait_for(ws_mem, "MESSAGE_CREATE", lambda d: d.get("content") == "secret")
    check("ephemeral message reaches invoker", eph["flags"] == 64)
    await asyncio.sleep(0.2)
    s, hist_owner = req("GET", f"/channels/{ch}/messages?limit=20", token=to)
    check("ephemeral message hidden from others", all(m.get("content") != "secret" for m in hist_owner))
    s, hist_mem = req("GET", f"/channels/{ch}/messages?limit=20", token=tm)
    check("ephemeral message visible to invoker", any(m.get("content") == "secret" for m in hist_mem))

    # Followup and deferred response.
    s, fu = req("POST", f"/webhooks/{aid}/{created2['token']}", {"content": "followup"})
    check("followup posts a message", s == 200 and fu["content"] == "followup")
    s, _ = req("POST", "/interactions", {"type": 2, "application_id": aid, "channel_id": ch,
                                          "data": {"id": cmd["id"], "name": "ping", "options": []}}, tm)
    created3 = await wait_for(ws_bot, "INTERACTION_CREATE", lambda d: d.get("type") == 2)
    req("POST", f"/interactions/{created3['id']}/{created3['token']}/callback", {"type": 5})
    s, orig = req("PATCH", f"/webhooks/{aid}/{created3['token']}/messages/@original", {"content": "late"})
    check("deferred response creates original message", s == 200 and orig["content"] == "late")
    s, orig2 = req("PATCH", f"/webhooks/{aid}/{created3['token']}/messages/@original", {"content": "later"})
    check("@original edits the same message", s == 200 and orig2["id"] == orig["id"] and orig2["content"] == "later")

    # Modal: bot opens a popup from a component press, member submits it.
    s, _ = req("POST", "/interactions", {"type": 3, "application_id": aid, "channel_id": ch, "message_id": mid_msg,
                                          "nonce": "m-" + RUN, "data": {"custom_id": "press", "component_type": 2}}, tm)
    press2 = await wait_for(ws_bot, "INTERACTION_CREATE", lambda d: d.get("type") == 3)
    s, _ = req("POST", f"/interactions/{press2['id']}/{press2['token']}/callback", {"type": 9, "data": {
        "custom_id": "form", "title": "Your name", "components": [{"type": 1, "components": [
            {"type": 4, "custom_id": "name", "style": 1, "label": "Name"}]}]}})
    check("modal callback accepted", s == 204)
    modal = await wait_for(ws_mem, "INTERACTION_MODAL_CREATE")
    check("modal reaches the invoker", modal["custom_id"] == "form" and modal["nonce"] == "m-" + RUN)
    s, _ = req("POST", "/interactions", {"type": 5, "application_id": aid, "channel_id": ch, "data": {
        "id": modal["id"], "custom_id": "form", "components": [{"type": 1, "components": [
            {"type": 4, "custom_id": "name", "value": "Ada"}]}]}}, tm)
    check("modal submission accepted", s == 204)
    sub = await wait_for(ws_bot, "INTERACTION_CREATE", lambda d: d.get("type") == 5)
    check("bot receives modal submission", sub["data"]["custom_id"] == "form")
    s, _ = req("POST", "/interactions", {"type": 5, "application_id": aid, "channel_id": ch, "data": {
        "id": modal["id"], "custom_id": "form", "components": []}}, tm)
    check("modal cannot be submitted twice", s == 404)

    # Command management and index updates.
    check("bot deletes guild command", req("DELETE", f"/applications/{aid}/guilds/{gid}/commands/{gcmd['id']}", token=btoken)[0] == 204)
    s, index = req("GET", f"/guilds/{gid}/application-command-index", token=tm)
    check("deleted command leaves index", [c["name"] for c in index["application_commands"]] == ["ping"])

    # Removing the bot from the guild stops its commands appearing.
    check("owner removes bot", req("DELETE", f"/applications/{aid}/guilds/{gid}", token=to)[0] == 204)
    s, index = req("GET", f"/guilds/{gid}/application-command-index", token=tm)
    check("commands disappear once the bot leaves", index["application_commands"] == [])

    for w in (ws_bot, ws_own, ws_mem):
        await w.close()
    print("\nFAILURES:" if failures else "\nAll checks passed.", failures or "")
    sys.exit(1 if failures else 0)


asyncio.run(main())
