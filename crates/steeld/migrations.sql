CREATE TABLE IF NOT EXISTS users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    email         TEXT UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    created_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tokens (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS guilds (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    owner_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS guild_members (
    guild_id  TEXT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    joined_at TEXT NOT NULL,
    nick      TEXT,
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE IF NOT EXISTS roles (
    id          TEXT PRIMARY KEY,
    guild_id    TEXT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    permissions TEXT NOT NULL,
    color       INTEGER NOT NULL DEFAULT 0,
    hoist       INTEGER NOT NULL DEFAULT 0,
    mentionable INTEGER NOT NULL DEFAULT 0,
    position    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS roles_guild_idx ON roles (guild_id);

CREATE TABLE IF NOT EXISTS member_roles (
    guild_id TEXT NOT NULL,
    user_id  TEXT NOT NULL,
    role_id  TEXT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (guild_id, user_id, role_id),
    FOREIGN KEY (guild_id, user_id) REFERENCES guild_members(guild_id, user_id) ON DELETE CASCADE
);

-- Channel permission overwrites. target_type 0 is a role, 1 a member. Allow and deny are
-- decimal permission bitfields stored as text.
CREATE TABLE IF NOT EXISTS overwrites (
    channel_id  TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    target_id   TEXT NOT NULL,
    target_type INTEGER NOT NULL,
    allow       TEXT NOT NULL,
    deny        TEXT NOT NULL,
    PRIMARY KEY (channel_id, target_id)
);

CREATE TABLE IF NOT EXISTS guild_bans (
    guild_id   TEXT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE IF NOT EXISTS channels (
    id         TEXT PRIMARY KEY,
    guild_id   TEXT REFERENCES guilds(id) ON DELETE CASCADE,
    kind       INTEGER NOT NULL,
    name       TEXT,
    topic      TEXT,
    position   INTEGER NOT NULL DEFAULT 0,
    parent_id  TEXT REFERENCES channels(id) ON DELETE SET NULL,
    owner_id   TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS channel_recipients (
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (channel_id, user_id)
);

CREATE TABLE IF NOT EXISTS relationships (
    user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    target_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind      INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, target_id)
);

CREATE TABLE IF NOT EXISTS messages (
    id          TEXT PRIMARY KEY,
    channel_id  TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    author_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    content     TEXT NOT NULL,
    reply_to_id TEXT,
    created_at  TEXT NOT NULL,
    edited_at   TEXT
);
CREATE INDEX IF NOT EXISTS messages_channel_idx ON messages (channel_id, id);

CREATE TABLE IF NOT EXISTS read_states (
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id      TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    last_message_id TEXT,
    PRIMARY KEY (user_id, channel_id)
);

CREATE TABLE IF NOT EXISTS attachments (
    id           TEXT PRIMARY KEY,
    message_id   TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    channel_id   TEXT NOT NULL,
    filename     TEXT NOT NULL,
    content_type TEXT,
    size         INTEGER NOT NULL,
    path         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS attachments_message_idx ON attachments (message_id);

CREATE TABLE IF NOT EXISTS pending_uploads (
    token        TEXT PRIMARY KEY,
    channel_id   TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    filename     TEXT NOT NULL,
    content_type TEXT,
    size         INTEGER NOT NULL,
    path         TEXT,
    uploaded     INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS invites (
    code       TEXT PRIMARY KEY,
    guild_id   TEXT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    inviter_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS reactions (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    emoji      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (message_id, user_id, emoji)
);

CREATE TABLE IF NOT EXISTS pins (
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    pinned_at  TEXT NOT NULL,
    pinned_by  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (channel_id, message_id)
);
