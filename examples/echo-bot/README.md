# Echo bot

A small example bot for Steel. It shows application commands, a private (ephemeral) reply, a button that edits its message, and the gateway flow.

## Try it

1. Sign in to a Steel server as a user and create an application (the bot account is created with it):

   ```sh
   curl -s -X POST "$STEEL_URL/api/v10/applications" \
     -H "authorization: $USER_TOKEN" -H "content-type: application/json" \
     -d '{"name":"Echo Bot"}'
   ```

   The response has `id` (the application ID) and `token` (the bot token, shown once).

2. Add the bot to a guild you own. Only the guild owner can do this:

   ```sh
   curl -s -X PUT "$STEEL_URL/api/v10/applications/$APPLICATION_ID/guilds/$GUILD_ID" \
     -H "authorization: $USER_TOKEN"
   ```

3. Run the bot:

   ```sh
   pip install websockets
   STEEL_URL=http://127.0.0.1:8080 APPLICATION_ID=... BOT_TOKEN=... python3 examples/echo-bot/echo_bot.py
   ```

4. In Flint, open that guild and type `/` in a channel. `/ping`, `/echo` and `/hello` should appear.

`BOT_TOKEN` may be the raw token or `Bot <token>`. Keep the token secret: anyone with it can act as the bot.
