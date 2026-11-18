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

Phase 1 in progress. Smoke tests: `scripts/smoke_test.py` and `scripts/dm_smoke_test.py` against a running server. Implemented so far:

- Accounts: `POST /api/v10/auth/register`, `POST /api/v10/auth/login`, `GET /api/v10/users/@me`
- Argon2id password hashes; opaque bearer tokens stored only as SHA-256 hashes
- Guilds, channels, messages (send, list with pagination, edit, delete, replies), typing, invites.
- DMs and group DMs (`POST /users/@me/channels`), reusing an existing 1:1 DM.
- Friends and blocking: friend requests by username, accept, block, unblock, remove (`/users/@me/relationships`).
- Gateway dispatches READY (with guilds), GUILD_CREATE, CHANNEL_CREATE, MESSAGE_CREATE/UPDATE/DELETE, TYPING_START, GUILD_MEMBER_ADD.
- Gateway at `/gateway`: Hello (op 10), Identify (op 2) to READY, Heartbeat (op 1) to ACK (op 11). Bad tokens close with 4004.

Not yet implemented: Resume, roles and permissions, uploads and CDN, presence, read state.

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
