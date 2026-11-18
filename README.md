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

Phase 1 in progress. Smoke tests against a running server: `scripts/smoke_test.py`, `scripts/dm_smoke_test.py`, `scripts/presence_smoke_test.py`, `scripts/upload_smoke_test.py`, `scripts/resume_smoke_test.py`, `scripts/staged_upload_smoke_test.py`, `scripts/members_smoke_test.py`, `scripts/phase2_smoke_test.py`, `scripts/bots_smoke_test.py`. Implemented so far:

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
- Search: `GET /guilds/{id}/messages/search` and `GET /channels/{id}/messages/search` with `content`, `limit`, `offset`. Case-insensitive substring, newest first.

Not yet implemented: threads, forums, polls, member list, emoji and stickers, profiles and settings, audit log, moderation beyond kick and ban, onboarding, roles and permissions, mention counts, voice-related member fields (mute, deafen).

## Run locally

```sh
cargo run -p steeld
# STEEL_DATABASE_URL defaults to sqlite://steel.db?mode=rwc
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

To change the schema, add the next numbered file (for example `0004_add_thing.sql`). Never edit a migration that has shipped; add a new one instead.

Databases created by commits between `136f9a6` and `4f36470` were built with an earlier runner that had no migration record. Recreate those dev databases rather than upgrading them.

## Bots and slash commands

- Bot accounts: `POST /api/v10/applications` creates an application and its bot user and returns the bot token once. `POST /applications/{id}/bot/reset` issues a new token. Authenticate as `Authorization: Bot <token>` or with the gateway Identify token.
- The guild owner adds a bot with `PUT /applications/{id}/guilds/{guild}`.
- Application commands: `/applications/{id}/commands` and `/applications/{id}/guilds/{guild}/commands` (list, create, overwrite, edit, delete). Chat-input commands only.
- Discovery: `GET /guilds/{id}/application-command-index` (also per channel), which Flint uses for the composer.
- Interactions: Flint submits `POST /interactions` (commands, components, modal submits). Steel sends the bot `INTERACTION_CREATE`. The bot answers with `POST /interactions/{id}/{token}/callback` (messages, deferred, update, modal) and then followups under `/webhooks/{app}/{token}`, all valid for 15 minutes.
- Bot messages can carry components and an ephemeral flag (64), which reaches only the invoking user.
- Example bot: `examples/echo-bot/`. Smoke test: `scripts/bots_smoke_test.py`. Flint-side check: `cargo run --bin bots` in `scripts/flint_e2e` (needs steeld on 127.0.0.1:8080).
- Not yet: autocomplete (Flint sends no autocomplete request), bot DMs, and user-installed apps.
