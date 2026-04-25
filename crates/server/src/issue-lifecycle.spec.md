---
targets:
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
  - crates/cli/src/**/*.rs
verified: 2026-04-25T10:02:12Z
---

# Issue Lifecycle Spec

## Overview

Issues are the primary user-facing work unit. Each issue moves through a simple lifecycle
driven by the agent session it spawns. The DB is the single source of truth; the server
holds no authoritative issue state in memory.

## States and Transitions

```
open → running → completed
              ↘ failed
         ↑ (reopen)
```

- **`open`** — issue created, not yet started. The default state after `ns2 issue new`.
- **`running`** — `ns2 issue start` has been called; a session is active.
- **`completed`** — the agent session finished successfully, or the user called
  `ns2 issue complete`. Terminal.
- **`failed`** — the agent session hit an error, or the server restarted while the
  session was active (orphan recovery). Terminal unless explicitly reopened.

Both `failed` and `completed` issues can be moved back to `open` via `ns2 issue reopen`.
The behavior differs by prior state — see the Reopen section below.

## Auto-Complete: Issue Watcher Task

When `ns2 issue start` spawns a harness, the server also spawns an `issue_watcher` task
that subscribes to the session's broadcast channel. The watcher:

1. Accumulates the current turn's text content as `ContentBlockDelta { TextDelta }`
   events arrive, building a `current_turn_text` buffer.
2. On `TurnDone` — saves `current_turn_text` into `last_turn_text`; resets
   `current_turn_text` to empty.
3. On `SessionDone` — posts `last_turn_text` as a comment on the issue (author =
   `issue.assignee` or `"agent"` if no assignee), then marks the issue `completed`.
4. On `Error { message }` — posts `message` as a comment (author = `"system"`), then
   marks the issue `failed`.

The comment is written to the DB before the status transition, so a reader that polls
on status will always see the comment once the issue is terminal.

## Comment Protocol

Comments are stored in the `issues.comments` JSON array. Each comment has:
- `author` — string; `"user"` for human comments, agent name for agent comments,
  `"system"` for server-generated notices (orphan recovery, error messages)
- `created_at` — UTC timestamp
- `body` — text content

`ns2 issue comment --id <id> --body <text> [--author <name>]` adds a comment manually.
`ns2 issue complete --id <id> --comment <text>` adds a final user comment and transitions
the issue to `completed`.
`ns2 issue reopen --id <id> --comment <text>` adds a user comment before transitioning
back to `open`.

## Orphan Recovery

On server start, the orphan sweep (see Session Lifecycle Spec) identifies issues whose
linked session was `running` at the time of restart. For each such issue:

1. The issue is transitioned to `failed`.
2. A comment is appended:
   ```
   session lost on server restart
   ```
   Author: `"system"`.

After recovery, the issue is in a clean terminal state and can be inspected, commented
on, or reopened.

## Reopen (`ns2 issue reopen`)

`ns2 issue reopen --id <id> [--comment <text>] [--start]` moves a `failed` or
`completed` issue back to `open`. **Behavior differs by prior state:**

- **`failed` → reopen** — clears `session_id` so `issue start` creates a fresh session.
  The failed session's history is not replayed (the harness is long dead).
- **`completed` → reopen** — keeps `session_id` so `issue start` resumes the existing
  session with full history. This lets the agent continue from where it left off.

For both states:
- Existing comments are preserved — the history of what happened is retained.
- If `--comment <text>` is provided, a comment with `author = "user"` is appended
  **before** the status transition, so it is visible in history when the agent resumes.
- If `--start` is provided, `issue start` is called immediately after reopening.
- The `updated_at` timestamp is refreshed.
- Only `failed` and `completed` issues can be reopened. Attempting to reopen an `open`
  or `running` issue returns an error.

After reopening, the normal `open → running → completed` lifecycle applies.

## Validation Rules

- **`ns2 issue start`** requires:
  - Issue must be in `open` state (not `running`, `completed`, or `failed`).
  - Issue must have an assignee; returns an error if `assignee` is `None`.
  - The assignee agent must exist in `.ns2/agents/`.
- **`ns2 issue complete`** requires:
  - Issue must not already be in a terminal state (`completed` or `failed`).
  - `--comment` flag is required.
- **`ns2 issue reopen`** requires:
  - Issue must be in `failed` or `completed` state.
- **`ns2 issue new --start`** requires:
  - `--assignee` must be provided (start needs an agent to run).

## Connect Sections

- **session lifecycle:** `crates/server/src/session-lifecycle.spec.md` — session states,
  orphan sweep, SSE event stream
- **CLI commands:** `crates/cli/cli-commands.spec.md` — `ns2 issue` subcommand reference
- **data model:** `crates/db/data-model.spec.md` — schema for issues and comments
- **architecture:** `crates/arch-tests/architecture.spec.md` — crate dependency rules