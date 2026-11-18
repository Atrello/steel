-- Server onboarding. Prompts and options are stored as one JSON document per guild, in the shape
-- Flint decodes. Each member's chosen options and seen markers live in user_onboarding.
CREATE TABLE guild_onboarding (
    guild_id   TEXT PRIMARY KEY REFERENCES guilds(id) ON DELETE CASCADE,
    enabled    INTEGER NOT NULL DEFAULT 0,
    prompts    TEXT NOT NULL DEFAULT '[]',
    updated_at TEXT NOT NULL
);

CREATE TABLE user_onboarding (
    user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    guild_id      TEXT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    responses     TEXT NOT NULL DEFAULT '[]',
    prompts_seen  TEXT NOT NULL DEFAULT '{}',
    options_seen  TEXT NOT NULL DEFAULT '{}',
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (user_id, guild_id)
);
