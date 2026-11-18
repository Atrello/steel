# Steel

> Strike your own spark.

Steel is a self-hostable chat server. It speaks a Discord-shaped JSON wire format (REST plus a JSON WebSocket gateway) so the Flint desktop client can connect to it. Nothing connects to Discord.

## Layout

```
crates/steeld/          server binary
crates/flint-protocol/  shared wire types (Flint depends on this crate)
deploy/                 Dockerfile, docker-compose.yml, example env file
```

## Status

Phase 1 in progress. Smoke tests against a running server: `scripts/smoke_test.py`, `scripts/dm_smoke_test.py`, `scripts/presence_smoke_test.py`, `scripts/upload_smoke_test.py`, `scripts/resume_smoke_test.py`, `scripts/staged_upload_smoke_test.py`, `scripts/members_smoke_test.py`, `scripts/phase2_smoke_test.py`, `scripts/polls_smoke_test.py`, `scripts/bots_smoke_test.py`, `scripts/threads_smoke_test.py`, `scripts/member_list_smoke_test.py`, `scripts/emojis_smoke_test.py`, `scripts/bots_context_smoke_test.py`, `scripts/invite_page_smoke_test.py`. Implemented so far:

- Accounts: `POST /api/v10/auth/register`, `POST /api/v10/auth/login`, `GET /api/v10/users/@me`
- Argon2id password hashes; opaque bearer tokens stored only as SHA-256 hashes
- Guilds, channels, messages (send, list with pagination, edit, delete, replies), typing, invites.
- DMs and group DMs (`POST /users/@me/channels`), reusing an existing 1:1 DM.
- Friends and blocking: friend requests by username, accept, block, unblock, remove (`/users/@me/relationships`).
- Presence: online while a gateway session is connected; `op 3` sets idle, DND or invisible; `PRESENCE_UPDATE` goes to guild co-members, DM peers and friends.
- Uploads, two ways: the signed flow Flint uses (`POST /channels/{id}/attachments`, then `PUT /cdn/upload/{token}`, then reference `uploaded_filename` in the message), and multipart `POST /channels/{id}/messages`.
- Uploads: multipart `POST /channels/{id}/messages` with `payload_json` and `files[n]`, up to 10 files, 100 MiB each by default (`STEEL_MAX_UPLOAD_BYTES`). Files are served from `/cdn/attachments/{channel}/{attachment}/{filename}`. Attachment IDs are random tokens.
- Read state: `POST /channels/{id}/messages/{id}/ack` (moves forward only), `MESSAGE_ACK`, and `read_state` in READY.
- Gateway dispatches READY (with guilds), GUILD_CREATE, CHANNEL_CREATE, MESSAGE_CREATE/UPDATE/DELETE, TYPING_START, GUILD_MEMBER_ADD.
- Gateway Resume: sessions keep up to 1024 events for 5 minutes after disconnect. `op 6` replays the events after the client's last sequence number and sends `RESUMED`. A session that is unknown, expired, or behind the buffer gets Invalid Session (`op 9`), and the client identifies again.
- Gateway at `/gateway`: Hello (op 10), Identify (op 2) to READY, Heartbeat (op 1) to ACK (op 11). Bad tokens close with 4004.

- Leave a group DM (`DELETE /channels/{id}/recipients/@me`) and leave a guild (`DELETE /users/@me/guilds/{id}`; the owner cannot leave).
- Guild members: read and edit nickname (`GET`/`PATCH /guilds/{id}/members/{user}`), kick (`DELETE`), ban and unban (`PUT`/`DELETE /guilds/{id}/bans/{user}`). Owner moderates; banned users cannot join with an invite.
- Read state: `POST /guilds/{id}/ack` and bulk `POST /read-states/ack-bulk`.

- Reactions: add and remove your own (`PUT`/`DELETE .../reactions/{emoji}/@me`), remove another user's (guild owner), list who reacted, clear all (owner). Messages carry `reactions` with count and `me`. Events: `MESSAGE_REACTION_ADD`, `MESSAGE_REACTION_REMOVE`, `MESSAGE_REACTION_REMOVE_ALL`. Up to 20 distinct emoji per message.
- Pins: list (`GET .../messages/pins`, paged by `before`), pin and unpin (`PUT`/`DELETE .../messages/pins/{id}`). Guild owner pins in guilds; any DM participant pins in DMs. Up to 50 per channel. Event: `CHANNEL_PINS_UPDATE`.
- Polls: create with `poll` on a message (2 to 10 answers, 1 to 768 hours, optional multiselect); vote with `PUT .../polls/{id}/answers/@me`; the author ends it with `POST .../polls/{id}/expire`. Messages carry `poll` with counts and `me_voted`. Events: `MESSAGE_POLL_VOTE_ADD`, `MESSAGE_POLL_VOTE_REMOVE`.
- Threads: start from a message (`POST .../messages/{id}/threads`, one per message, id = message id) or standalone (`POST .../channels/{id}/threads`, public only). Listed in `GET /guilds/{id}/threads/active`; events `THREAD_CREATE`. Thread members, private threads, and archiving are not implemented yet.
- Member list: Flint's op 37 subscription (`channels: {channel_id: [[start, end]]}`) gets `GUILD_MEMBER_LIST_UPDATE` SYNC ops on the same socket. One `everyone` list sorted by username, all in the `offline` group. Non-members get nothing. Presence groups are not included yet.
- Forums: a forum is a channel of type 15. `POST /channels/{id}/threads` on a forum creates a post with a required `message.content` starter (the post id equals the starter message id). `GET /channels/{id}/threads/search` lists open posts newest first, with `first_messages`. Forum posts are threads, so `THREAD_CREATE` and the guild active list cover them. Tags and sort orders other than newest activity are not implemented yet.
- Custom emoji: `GET/POST /guilds/{id}/emojis` (PNG or GIF data URI, up to 256 KiB, 50 per guild, `MANAGE_GUILD`), `PATCH`/`DELETE /guilds/{id}/emojis/{id}`. Images are served from `/cdn/emojis/{id}.{ext}`. Event `GUILD_EMOJIS_UPDATE` carries the full list. Stickers are not implemented yet.
- Profiles: `PATCH /users/@me` edits the display name (32 characters), bio (190), pronouns (40) and accent color (24-bit RGB). Absent fields stay; `null` clears. `GET /users/{id}/profile` returns them, with `USER_UPDATE` to the user. Banners and connections are not stored yet.
- Settings: `GET`/`PATCH /users/@me/settings-proto/{kind}` (kinds 1 and 2). Steel stores the base64 protobuf opaquely. A write must send the current data version (field 1) as `required_data_version`, or it gets 409 with `out_of_date` and the current blob. Writes fire `USER_SETTINGS_PROTO_UPDATE` to the user's sessions. Not yet in READY.
- Audit log: `GET /guilds/{id}/audit-logs` (needs `VIEW_AUDIT_LOG`, bit 7), with `limit`, `before`, `user_id` and `action_type`. Entries are written for kicks, bans and unbans, channel create and delete, and emoji create, update and delete. Discord action numbers are used.
- Search: `GET /guilds/{id}/messages/search` and `GET /channels/{id}/messages/search` with `content`, `limit`, `offset`. Case-insensitive substring, newest first.

Not yet implemented:, emoji and stickers, profiles and settings, audit log, moderation beyond kick and ban, onboarding, roles and permissions, mention counts, voice-related member fields (mute, deafen).

## Run locally

```sh
cargo run -p steeld
# STEEL_DATABASE_URL defaults to sqlite://steel.db?mode=rwc; use postgres://... for Postgres
```

## Run with Docker

```sh
cd deploy
cp steel.env.example steel.env
docker compose up -d --build
```

## Test

```sh
cargo test
```

## Database migrations

The schema is versioned. Migrations live in `crates/steeld/migrations/` as `NNNN_name.sql`, are embedded in the binary, and run once at startup. Each applied migration is recorded in `_sqlx_migrations`, so an existing database upgrades in place.

To change the schema, add the next numbered file. Never edit a migration that has shipped; add a new one instead.

Version numbers are reserved per branch so parallel work does not collide: `0001`–`0003` are the baseline schema, `0004`–`0099` belong to the Steel core, `0100`–`0199` to text features, `0200`–`0299` to voice, and `0300`–`0399` to bots. Gaps are fine, and a lower version merged later still runs once. A database may hold migrations this binary does not ship, which startup tolerates.

Databases created by commits between `136f9a6` and `4f36470` were built with an earlier runner that had no migration record. Recreate those dev databases rather than upgrading them.

## Bots and slash commands

- Bot accounts: `POST /api/v10/applications` creates an application and its bot user and returns the bot token once. `POST /applications/{id}/bot/reset` issues a new token. Authenticate as `Authorization: Bot <token>` or with the gateway Identify token.
- The guild owner adds a bot with `PUT /applications/{id}/guilds/{guild}`.
- Application commands: `/applications/{id}/commands` and `/applications/{id}/guilds/{guild}/commands` (list, create, overwrite, edit, delete). Chat-input commands only.
- Discovery: `GET /guilds/{id}/application-command-index` (also per channel), which Flint uses for the composer.
- Interactions: Flint submits `POST /interactions` (commands, components, modal submits). Steel sends the bot `INTERACTION_CREATE`. The bot answers with `POST /interactions/{id}/{token}/callback` (messages, deferred, update, modal) and then followups under `/webhooks/{app}/{token}`, all valid for 15 minutes.
- Bot messages can carry components and an ephemeral flag (64), which reaches only the invoking user.
- Example bot: `examples/echo-bot/`. Smoke test: `scripts/bots_smoke_test.py`. Flint-side check: `cargo run --bin bots` in `scripts/flint_e2e` (needs steeld on 127.0.0.1:8080).
- Autocomplete: an option with `autocomplete: true` gets `INTERACTION_CREATE` (type 4) when a member types. The bot answers with `callback` type 8 and the choices go back to the invoker as `INTERACTION_AUTOCOMPLETE_RESULT`.
- Bot DMs: a user opens a DM with the bot (`POST /users/@me/channels`), sees its global commands in the DM index, and runs them there.
- User installs: `PUT /users/@me/applications/{app}` installs an application for yourself. Its global commands then work in your DMs and guilds, without the bot in the guild. `DELETE` removes it.
- Smoke tests: `scripts/bots_context_smoke_test.py` covers bot DMs, autocomplete and user installs.
- Flint's client already requests the command index for DMs with a bot. It has no autocomplete request and no way to see user installs yet, so those two need client work.
