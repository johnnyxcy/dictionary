ALTER TABLE lookups ADD COLUMN requested_direction TEXT NOT NULL DEFAULT 'en_zh';
CREATE TABLE app_state (
    key TEXT PRIMARY KEY,
    payload TEXT NOT NULL
);
PRAGMA user_version = 3;
