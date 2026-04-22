# CLI Commands Spec

## Overview

All commands follow a `ns2 <noun> <verb>` structure. Running `ns2` with no arguments attaches to the orchestrator session.

---

## `ns2 session`

### `ns2 session list`

List all sessions. Output columns: `id`, `name`, `agent type`, `status`, `last activity`.

Default: last 24h, up to 10 sessions. Sort order: `failed` → `waiting` → `running` → `completed` → `cancelled`, then by last update within each group.

Flags (all optional):
- `--status <status>` — filter by status (`running`, `waiting`, `completed`, `failed`, `cancelled`)
- `--agent <type>` — filter by agent type
- `--since <duration>` — filter to sessions active within a duration (default: `24h`). Format: `XXd:YYh:ZZm` with any field optional (e.g. `1d`, `2h:30m`, `1d:12h`).
- `--limit <n>` — max sessions to return (default: 10)

### `ns2 session new`

Create a new agent session. Prints the session ID on success.

Flags:
- `--name <name>` — human-readable label for the session
- `--agent <type>` — agent type; defaults to `coding`
- `--message <message>` — opening message to kick off the session. If omitted, session is created in `waiting` state.

### `ns2 session attach`

Attach the TUI to a session. Defaults to the orchestrator session if no flags provided.

Flags:
- `--id <id>` — attach by session ID
- `--name <name>` — attach by session name

### `ns2 session tail`

Print the most recent content blocks from a session to stdout without attaching the TUI. Useful for the orchestrator agent to check on sessions programmatically.

Flags:
- `--id <id>` — identify by session ID
- `--name <name>` — identify by session name
- `--turns <n>` — number of recent turns to show (default: 5)

### `ns2 session stop`

Cancel a running or waiting session.

Flags:
- `--id <id>` — identify by session ID
- `--name <name>` — identify by session name

### `ns2 session send`

Queue a message to a session without attaching.

Flags:
- `--id <id>` — identify by session ID
- `--name <name>` — identify by session name
- `--message <message>` — message to queue

---

## `ns2 agent`

### `ns2 agent list`

List all available agent types from `.ns2/agents/`, showing name and description from frontmatter. No flags.

### `ns2 agent new`

Create a new agent system prompt file in `.ns2/agents/` and open it in `$EDITOR`.

Flags:
- `--name <name>` — agent type name (becomes the filename and frontmatter `name`)
- `--description <description>` — short description written into frontmatter
- `--body <body>` — initial system prompt body; if omitted, file opens empty in `$EDITOR`

### `ns2 agent edit`

Updates the provided fields. All flags optional — if none provided, errors.

Flags:
- `--name <name>` — agent type to edit
- `--description <description>` — update the frontmatter description
- `--body <body>` — replace the prompt body directly.

---

## `ns2 spec`

Spec files (`.spec.md`) are design documents that declare which source files they govern. A spec file has YAML frontmatter with two fields:

- `targets` — a list of glob patterns (relative to the git root) for files this spec covers
- `verified` — an ISO 8601 UTC timestamp recording when the spec was last confirmed to match its targets

Files without valid frontmatter (e.g. the raw architecture or harness spec files) are silently ignored by all `ns2 spec` commands.

### `ns2 spec new`

Create a new spec file at the given path with the provided targets. The file is initialized with a `targets` list and no `verified` timestamp (unverified). The body is left empty.

Args:
- `<path>` — path where the spec file should be created (e.g. `crates/myfeature/design.spec.md`)

Flags:
- `--target <glob>` — glob pattern for files this spec covers; can be repeated

Prints `Created spec at <path>` on success. Errors if the file already exists.

### `ns2 spec sync`

Check whether any files matched by spec targets have been modified since the spec was last verified. Compares each target file's modification time against the `verified` timestamp. If `verified` is absent, every matched file is considered stale.

If any stale files are found, prints an error listing each affected spec path and its offending files, then exits non-zero. If all specs are clean, exits 0 with no output.

Args (optional):
- `<path>` — path to a specific `.spec.md` file; if omitted, checks all `.spec.md` files found recursively from the git root

Spec files without valid frontmatter (missing `targets`) are silently skipped.

### `ns2 spec verify`

Mark a spec as verified at the current time, writing the current UTC timestamp into the `verified` frontmatter field. The rest of the file (body and targets) is preserved.

Args:
- `<path>` — path to the spec file to verify (required — cannot verify all specs at once)

Prints `Verified <path>` on success.

---

## `ns2 workspace`

### `ns2 workspace list`

List all worktrees, showing branch, path, and the status of the current session on that branch.

Flags:
- `--status <status>` — filter by current session status (`running`, `waiting`, `completed`, `failed`, `cancelled`)

### `ns2 workspace clean`

Delete a worktree manually, typically after the branch has been merged.

Flags:
- `--branch <branch>` — branch whose worktree to remove
- `--force` — remove even if the branch hasn't been merged
