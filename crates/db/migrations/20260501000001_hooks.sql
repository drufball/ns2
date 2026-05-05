CREATE TABLE IF NOT EXISTS hooks (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    source_type TEXT NOT NULL,
    source      TEXT NOT NULL,
    filter      TEXT,
    action_type TEXT NOT NULL,
    action      TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_by  TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS hook_executions (
    id            TEXT PRIMARY KEY,
    hook_id       TEXT NOT NULL,
    triggered_at  TEXT NOT NULL,
    event_payload TEXT NOT NULL,
    status        TEXT NOT NULL,
    result        TEXT,
    completed_at  TEXT
);
