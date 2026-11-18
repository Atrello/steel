-- Editable profile fields. A row exists only once a user has set one of them.
CREATE TABLE user_profiles (
    user_id      TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    global_name  TEXT,
    bio          TEXT,
    pronouns     TEXT,
    accent_color INTEGER,
    updated_at   TEXT NOT NULL
);
