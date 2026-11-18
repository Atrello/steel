-- Per-user settings blobs, stored as the protobuf bytes the client sends (base64). Steel does not read
-- their fields beyond the data version (field 1), which the client uses to detect stale writes.
CREATE TABLE user_settings (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       INTEGER NOT NULL,
    proto      TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, kind)
);
