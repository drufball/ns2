CREATE TABLE IF NOT EXISTS github_issue_mapping (
    ns2_id TEXT NOT NULL PRIMARY KEY,
    github_number INTEGER NOT NULL UNIQUE,
    created_at DATETIME NOT NULL DEFAULT (datetime('now')),
    updated_at DATETIME NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_github_issue_mapping_github_number ON github_issue_mapping(github_number);
