-- Audit log entries. Changes and options are JSON text, shaped as Discord's audit log entries.
CREATE TABLE audit_log (
    id          TEXT PRIMARY KEY,
    guild_id    TEXT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    user_id     TEXT,
    target_id   TEXT,
    action_type INTEGER NOT NULL,
    reason      TEXT,
    changes     TEXT NOT NULL,
    options     TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX audit_log_guild_idx ON audit_log (guild_id, action_type);
