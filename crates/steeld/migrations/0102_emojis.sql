-- Custom guild emoji. Image files live under STEEL_DATA_DIR/emojis/{guild_id}/{id}.{ext}.
CREATE TABLE guild_emojis (
    id         TEXT PRIMARY KEY,
    guild_id   TEXT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    animated   INTEGER NOT NULL DEFAULT 0,
    ext        TEXT NOT NULL,
    creator_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    UNIQUE (guild_id, name)
);
