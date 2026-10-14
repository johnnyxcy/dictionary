ALTER TABLE attempts ADD COLUMN answer_search_key TEXT NOT NULL DEFAULT '';
PRAGMA user_version = 5;
