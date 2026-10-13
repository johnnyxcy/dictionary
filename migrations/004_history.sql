CREATE INDEX attempts_history_order ON attempts(created_at DESC, id DESC);
CREATE INDEX attempts_history_status ON attempts(state, created_at DESC, id DESC);
PRAGMA user_version = 4;
