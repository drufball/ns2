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
