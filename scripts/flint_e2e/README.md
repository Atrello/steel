# Flint to Steel headless check

Drives Flint's `discord-api` and `discord-gateway` crates against a running `steeld`:
registers two users, connects both gateways to READY, creates a guild and invite, accepts
the invite, sends a message each way, uploads a file through the signed upload flow, and
checks the other user receives MESSAGE_CREATE and the downloaded bytes match.

Setup:

1. Check out `Atrello/flint` on `claude/project-thread-ldm0ad` next to the steel checkout's parent, so the path `../../../flint` resolves from this directory.
2. Start steeld on a fresh database:

   ```sh
   STEEL_BIND=127.0.0.1:8099 STEEL_DATA_DIR=/tmp/steel-e2e \
   STEEL_DATABASE_URL="sqlite:///tmp/steel-e2e/steel.db?mode=rwc" \
   cargo run -p steeld
   ```

3. From this directory: `cargo run` (uses the toolchain pinned in `rust-toolchain.toml`, 1.99.1).

The server address is hardcoded to `http://127.0.0.1:8099`. The run exits 0 when every check passes.
