# Deploying Steel

This guide is for running your own Steel server. Steel is one binary, `steeld`, that serves
the REST API, the gateway and uploaded files. It stores everything in SQLite by default.

## Requirements

- Docker with the Compose plugin, or a Rust toolchain (see the top-level README) for a bare build.
- A domain name pointed at the host, and a reverse proxy that terminates TLS (Caddy, nginx,
  Traefik or similar). Steel itself speaks plain HTTP.

## Quick start with Docker

```sh
cd deploy
cp steel.env.example steel.env
# Edit steel.env: set STEEL_PUBLIC_URL to the address clients will use, for example
# https://chat.example.com. Leaving the placeholder in place gives wrong links.
docker compose up -d --build
```

Steel listens on port 8080 inside the container, and Compose publishes it on the host at
`8080`. Point your reverse proxy at `127.0.0.1:8080`.

Check that it is up:

```sh
curl -s http://127.0.0.1:8080/api/v10/gateway
```

Then open Flint, choose the server address, and register the first account.

## Configuration

All settings are environment variables. `deploy/steel.env.example` lists them with comments.

| Variable | Default | Purpose |
| --- | --- | --- |
| `STEEL_BIND` | `0.0.0.0:8080` in the image | Address and port `steeld` listens on. |
| `STEEL_DATABASE_URL` | `sqlite:////data/steel.db?mode=rwc` | Database location. Only SQLite is supported right now. |
| `STEEL_DATA_DIR` | `/data` | Where uploaded files are stored. |
| `STEEL_PUBLIC_URL` | empty | The public address clients use. Used for absolute CDN and upload links. An empty value gives relative links, which many clients cannot follow. |
| `STEEL_MAX_UPLOAD_BYTES` | `104857600` (100 MiB) | Per-file upload limit. |
| `RUST_LOG` | `steeld=info` | Log level, using the `tracing` filter syntax. |

Keep `steel.env` out of version control. It is listed in the example as the file to copy from,
and it may hold other secrets you add later.

## Reverse proxy and TLS

Steel builds upload and CDN links from `STEEL_PUBLIC_URL`, and it reads `X-Forwarded-Proto` and
`X-Forwarded-Host` when they are present. Forward both headers from your proxy so links use
`https` and your domain.

Caddy example:

```
chat.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

The gateway is a WebSocket on `/gateway`. Caddy proxies WebSockets without extra configuration.
With nginx, add the usual `Upgrade` and `Connection` headers on that location.

## Data and backups

Everything lives in the `steel-data` Docker volume: the SQLite database (`steel.db`, with its
`-wal` and `-shm` files while running) and uploaded files. Compose prefixes the volume with the project name, so run `docker volume ls` to find the exact name (usually `deploy_steel-data`).

To back up, copy the volume with the server stopped, or use SQLite's online backup:

```sh
docker compose stop steel
docker run --rm -v deploy_steel-data:/data -v "$PWD":/backup debian:bookworm-slim \
  tar czf /backup/steel-data-$(date +%F).tar.gz -C /data .
docker compose start steel
```

Restoring means stopping the server, extracting the archive into the volume, and starting it
again.

## Upgrading

```sh
git pull
docker compose up -d --build
```

Migrations run at startup. Each one runs once and is recorded in `_sqlx_migrations`, so an
existing database upgrades in place. Take a backup before upgrading.

Databases created by commits between `136f9a6` and `4f36470` used an earlier migration runner
and cannot be upgraded. Recreate them rather than upgrading.

## Running without Docker

```sh
cargo build --release -p steeld
STEEL_BIND=127.0.0.1:8080 \
STEEL_DATA_DIR=/var/lib/steel \
STEEL_DATABASE_URL="sqlite:///var/lib/steel/steel.db?mode=rwc" \
STEEL_PUBLIC_URL=https://chat.example.com \
./target/release/steeld
```

Create the data directory first, and make sure the user running `steeld` can write to it.

## Limits and known gaps

- Postgres is planned but not available yet. Running on SQLite is the only supported option.
- The Docker image runs as UID 1000. The data volume must be writable by that user, which the
  image sets up for the default volume.
- Bots and slash commands are available, but autocomplete and user-installed apps are not.
