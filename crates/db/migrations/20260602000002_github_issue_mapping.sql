CREATE TABLE IF NOT EXISTS github_issue_mapping (
    ns2_id TEXT PRIMARY KEY NOT NULL,
    github_number INTEGER NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
