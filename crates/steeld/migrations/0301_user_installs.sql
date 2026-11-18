-- Phase 4: a user can install an application for themselves. Its global commands then work in
-- that user's DMs and guilds, without the bot being a member there.
CREATE TABLE IF NOT EXISTS user_installs (
    user_id        TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    application_id TEXT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
    created_at     TEXT NOT NULL,
    PRIMARY KEY (user_id, application_id)
);
