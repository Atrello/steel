-- Channels can sit under a category.
ALTER TABLE channels ADD COLUMN parent_id TEXT REFERENCES channels(id) ON DELETE SET NULL;
