"""Echo bot: a tiny example bot for Steel.

It registers three slash commands, answers them, and shows a button and a modal.

  /ping            replies "pong"
  /echo text:...   repeats the text back (ephemeral, only you see it); autocompletes words
  /hello           posts a message with a button; pressing it edits the message

Setup (see README.md): create an application and bot token, add the bot to a guild you own,
then run this script with STEEL_URL, BOT_TOKEN and APPLICATION_ID set.

Requires: pip install websockets
"""
import asyncio
import json
import os
import urllib.request
import urllib.error

import websockets

STEEL_URL = os.environ.get("STEEL_URL", "http://127.0.0.1:8080").rstrip("/")
BOT_TOKEN = os.environ["BOT_TOKEN"]
APPLICATION_ID = os.environ["APPLICATION_ID"]
API = STEEL_URL + "/api/v10"
AUTH = BOT_TOKEN if BOT_TOKEN.startswith("Bot ") else "Bot " + BOT_TOKEN

COMMANDS = [
    {"name": "ping", "description": "Check that the bot is alive"},
    {
        "name": "echo",
        "description": "Repeat text back to you, privately",
        "options": [
            {"type": 3, "name": "text", "description": "What to repeat", "required": True, "autocomplete": True}
        ],
    },
    {"name": "hello", "description": "Post a message with a button"},
]
BUTTON_ROW = [{"type": 1, "components": [{"type": 2, "style": 1, "label": "Count me", "custom_id": "count:0"}]}]


def rest(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(
        API + path, data=data, method=method,
        headers={"content-type": "application/json", "authorization": AUTH},
    )
    try:
        with urllib.request.urlopen(request) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode(errors="replace")


def callback(interaction, kind, data=None):
    body = {"type": kind}
    if data is not None:
        body["data"] = data
    return rest("POST", f"/interactions/{interaction['id']}/{interaction['token']}/callback", body)


def option(interaction, name):
    for item in interaction["data"].get("options", []):
        if item.get("name") == name:
            return item.get("value")
    return None


SUGGESTIONS = ["hello", "hello again", "help", "echo", "ping"]


async def handle(interaction):
    kind = interaction["type"]
    name = interaction["data"].get("name")
    if kind == 4:
        # Autocomplete: suggest the words that start with what the user has typed so far.
        typed = next((o.get("value", "") for o in interaction["data"].get("options", []) if o.get("focused")), "")
        choices = [{"name": w, "value": w} for w in SUGGESTIONS if w.startswith(typed)][:25]
        await asyncio.to_thread(callback, interaction, 8, {"choices": choices})
        return
    if kind == 2 and name == "ping":
        await asyncio.to_thread(callback, interaction, 4, {"content": "pong"})
    elif kind == 2 and name == "echo":
        text = option(interaction, "text") or ""
        await asyncio.to_thread(callback, interaction, 4, {"content": text, "flags": 64})
    elif kind == 2 and name == "hello":
        await asyncio.to_thread(callback, interaction, 4, {"content": "Hello! Press the button.", "components": BUTTON_ROW})
    elif kind == 3:
        # Button press: bump the counter in the custom_id and edit the message in place.
        count = int(interaction["data"]["custom_id"].split(":", 1)[1]) + 1
        rows = [{"type": 1, "components": [{"type": 2, "style": 1, "label": "Count me", "custom_id": f"count:{count}"}]}]
        await asyncio.to_thread(callback, interaction, 7, {"content": f"Pressed {count} times.", "components": rows})


async def main():
    for command in COMMANDS:
        status, _ = await asyncio.to_thread(rest, "POST", f"/applications/{APPLICATION_ID}/commands", command)
        print("register", command["name"], status)

    gateway = STEEL_URL.replace("http", "ws", 1) + "/gateway"
    async with websockets.connect(gateway) as ws:
        await ws.recv()  # HELLO
        await ws.send(json.dumps({"op": 2, "d": {"token": AUTH}}))
        ready = json.loads(await ws.recv())
        print("connected as", ready["d"]["user"]["username"])
        async for raw in ws:
            event = json.loads(raw)
            if event.get("t") == "INTERACTION_CREATE":
                await handle(event["d"])


if __name__ == "__main__":
    asyncio.run(main())
