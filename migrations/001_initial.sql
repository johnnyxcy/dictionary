CREATE TABLE skill_snapshots (
    hash TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    source TEXT NOT NULL,
    content TEXT NOT NULL
);
CREATE TABLE lookups (
    id TEXT PRIMARY KEY,
    input TEXT NOT NULL,
    search_key TEXT NOT NULL,
    direction TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE attempts (
    id TEXT PRIMARY KEY,
    lookup_id TEXT NOT NULL REFERENCES lookups(id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending', 'running', 'succeeded', 'failed', 'cancelled', 'timed_out', 'interrupted')),
    created_at TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL,
    skill_hash TEXT NOT NULL REFERENCES skill_snapshots(hash),
    input_tokens INTEGER CHECK(input_tokens >= 0),
    cached_input_tokens INTEGER CHECK(cached_input_tokens >= 0),
    output_tokens INTEGER CHECK(output_tokens >= 0),
    reasoning_output_tokens INTEGER CHECK(reasoning_output_tokens >= 0),
    duration_ms INTEGER CHECK(duration_ms >= 0),
    payload TEXT NOT NULL,
    UNIQUE(lookup_id, attempt_number)
);
CREATE INDEX attempts_created ON attempts(created_at DESC);
CREATE INDEX attempts_configuration ON attempts(model, effort, created_at);
PRAGMA user_version = 1;
