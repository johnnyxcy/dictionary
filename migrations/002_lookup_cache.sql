ALTER TABLE lookups ADD COLUMN cache_key TEXT NOT NULL DEFAULT '';
CREATE INDEX lookups_cache ON lookups(direction, cache_key);
PRAGMA user_version = 2;
