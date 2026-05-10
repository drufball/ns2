CREATE TABLE IF NOT EXISTS events (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    kind_type   TEXT NOT NULL,   -- 'webhook' or 'timer'
    kind        TEXT NOT NULL,   -- JSON blob
    description TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
