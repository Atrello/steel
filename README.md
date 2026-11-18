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

Phase 1 in progress. Smoke tests against a running server: `scripts/smoke_test.py`, `scripts/dm_smoke_test.py`, `scripts/presence_smoke_test.py`, `scripts/upload_smoke_test.py`, `scripts/resume_smoke_test.py`, `scripts/staged_upload_smoke_test.py`, `scripts/members_smoke_test.py`, `scripts/phase2_smoke_test.py`, `scripts/polls_smoke_test.py`. Implemented so far:

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
- Search: `GET /guilds/{id}/messages/search` and `GET /channels/{id}/messages/search` with `content`, `limit`, `offset`. Case-insensitive substring, newest first.

Not yet implemented: threads, forums, member list, emoji and stickers, profiles and settings, audit log, moderation beyond kick and ban, onboarding, roles and permissions, mention counts, voice-related member fields (mute, deafen).

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

To change the schema, add the next numbered file. Never edit a migration that has shipped; add a new one instead.

Version numbers are reserved per branch so parallel work does not collide: `0001`–`0003` are the baseline schema, `0004`–`0099` belong to the Steel core, `0100`–`0199` to text features, `0200`–`0299` to voice, and `0300`–`0399` to bots. Gaps are fine, and a lower version merged later still runs once. A database may hold migrations this binary does not ship, which startup tolerates.

Databases created by commits between `136f9a6` and `4f36470` were built with an earlier runner that had no migration record. Recreate those dev databases rather than upgrading them.
