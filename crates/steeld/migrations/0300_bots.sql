-- Phase 4: bot accounts, application commands and interactions.
-- A bot is a user row owned by an application. Its tokens live in `tokens` like any other user.
CREATE TABLE IF NOT EXISTS applications (
    id          TEXT PRIMARY KEY,
    owner_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bot_user_id TEXT NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS application_commands (
    id             TEXT PRIMARY KEY,
    application_id TEXT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
    guild_id       TEXT REFERENCES guilds(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    description    TEXT NOT NULL,
    options        TEXT NOT NULL DEFAULT '[]',
    version        TEXT NOT NULL,
    created_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS application_commands_app_idx ON application_commands (application_id, guild_id);

CREATE TABLE IF NOT EXISTS interactions (
    id                  TEXT PRIMARY KEY,
    token               TEXT NOT NULL,
    application_id      TEXT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
    kind                INTEGER NOT NULL,
    channel_id          TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    guild_id            TEXT,
    user_id             TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    message_id          TEXT,
    nonce               TEXT,
    responded           INTEGER NOT NULL DEFAULT 0,
    response_message_id TEXT,
    created_at          TEXT NOT NULL
);

-- Modal popups opened by a bot in response to an interaction. Submitting one consumes the row.
CREATE TABLE IF NOT EXISTS modals (
    id             TEXT PRIMARY KEY,
    interaction_id TEXT NOT NULL REFERENCES interactions(id) ON DELETE CASCADE,
    custom_id      TEXT NOT NULL,
    title          TEXT NOT NULL,
    components     TEXT NOT NULL,
    created_at     TEXT NOT NULL
);

-- Per-message extras: flags (ephemeral), components and the invoking user of an ephemeral message.
CREATE TABLE IF NOT EXISTS message_extras (
    message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    flags      INTEGER NOT NULL DEFAULT 0,
    components TEXT NOT NULL DEFAULT '[]',
    invoker_id TEXT REFERENCES users(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS application_commands_name_idx ON application_commands (application_id, COALESCE(guild_id, ''), name);
