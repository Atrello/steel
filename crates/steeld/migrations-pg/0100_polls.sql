CREATE TABLE polls (
    message_id  TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    question    TEXT NOT NULL,
    expires_at  TEXT,
    multiselect INTEGER NOT NULL,
    finalized   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE poll_answers (
    message_id TEXT NOT NULL REFERENCES polls(message_id) ON DELETE CASCADE,
    answer_id  INTEGER NOT NULL,
    text       TEXT NOT NULL,
    emoji      TEXT,
    PRIMARY KEY (message_id, answer_id)
);

CREATE TABLE poll_votes (
    message_id TEXT NOT NULL,
    answer_id  INTEGER NOT NULL,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (message_id, answer_id, user_id),
    FOREIGN KEY (message_id, answer_id) REFERENCES poll_answers(message_id, answer_id) ON DELETE CASCADE
);
