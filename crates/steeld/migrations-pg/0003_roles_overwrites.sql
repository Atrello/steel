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
