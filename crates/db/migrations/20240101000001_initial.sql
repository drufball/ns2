CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'created',
    agent TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS turns (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    token_count INTEGER,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS content_blocks (
    id TEXT PRIMARY KEY NOT NULL,
    turn_id TEXT NOT NULL REFERENCES turns(id),
    block_index INTEGER NOT NULL,
    role TEXT NOT NULL,
    block_type TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
