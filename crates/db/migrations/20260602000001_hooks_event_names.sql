-- Step 2: Replace HookSource with event_names on hooks.
-- We recreate the hooks table without the NOT NULL constraint on source_type/source
-- so new rows can be inserted without providing a source_type.
-- Old rows are preserved with their existing source_type values.

-- Create a new hooks table without NOT NULL on source_type/source
CREATE TABLE IF NOT EXISTS hooks_v2 (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    event_names TEXT NOT NULL DEFAULT '[]',
    source_type TEXT,
    source      TEXT,
    filter      TEXT,
    action_type TEXT NOT NULL,
    action      TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_by  TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- Copy existing data (backfill event_names as empty array)
INSERT INTO hooks_v2 (id, name, event_names, source_type, source, filter, action_type, action, enabled, created_by, created_at, updated_at)
SELECT id, name, '[]', source_type, source, filter, action_type, action, enabled, created_by, created_at, updated_at
FROM hooks;

-- Drop old table and rename
DROP TABLE hooks;
ALTER TABLE hooks_v2 RENAME TO hooks;

-- Create a new named_events table for Webhook and Timer event sources.
-- These replace the old HookSource::External and HookSource::Timer variants.
CREATE TABLE IF NOT EXISTS named_events (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    kind_type  TEXT NOT NULL,
    kind       TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
