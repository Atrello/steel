-- Guild stickers. Image files live under STEEL_DATA_DIR/stickers/{guild_id}/{id}.{ext}.
-- format_type follows Discord: 1 is PNG, 4 is GIF.
CREATE TABLE guild_stickers (
    id          TEXT PRIMARY KEY,
    guild_id    TEXT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    tags        TEXT NOT NULL,
    format_type INTEGER NOT NULL,
    ext         TEXT NOT NULL,
    creator_id  TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (guild_id, name)
);
