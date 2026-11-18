-- Stickers sent with a message, in order. The sticker row may later be deleted; hydration skips those.
CREATE TABLE message_stickers (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    position   INTEGER NOT NULL,
    sticker_id TEXT NOT NULL,
    PRIMARY KEY (message_id, position)
);
