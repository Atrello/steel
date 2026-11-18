-- A thread is a channel (type 11) whose parent_id is the text channel it was started in. Threads started
-- from a message reuse that message's id. Archiving is recorded here, though Steel does not archive yet.
CREATE TABLE threads (
    channel_id           TEXT PRIMARY KEY REFERENCES channels(id) ON DELETE CASCADE,
    owner_id             TEXT REFERENCES users(id) ON DELETE SET NULL,
    archived             INTEGER NOT NULL DEFAULT 0,
    auto_archive_minutes INTEGER NOT NULL,
    archive_timestamp    TEXT NOT NULL,
    created_at           TEXT NOT NULL
);
