---
targets:
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
  - crates/cli/src/**/*.rs
verified: 2026-05-06T18:51:59Z
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
              ↘ waiting
         ↑ (reopen)
```

- **`open`** — issue created, not yet started. The default state after `ns2 issue new`.
- **`running`** — `ns2 issue start` has been called; a session is active.
- **`completed`** — the agent called `stop(complete)`. Terminal.
- **`waiting`** — the agent called `stop(waiting)` or the session ended without calling
  `stop`. The issue is paused for human input. Terminal (the session is still associated
  and its history is preserved; reopen to continue).
- **`failed`** — the agent session hit an error, or the server restarted while the
  session was active (orphan recovery). Terminal unless explicitly reopened.

`failed`, `completed`, and `waiting` issues can be moved back to `open` via
`ns2 issue reopen`. The behavior differs by prior state — see the Reopen section below.

## Stop-Tool-Driven Issue Completion

When `ns2 issue start` spawns a harness, the server also spawns an `issue_watcher` task
that subscribes to the session's event bus. The watcher drives the issue to its terminal
state using the `Stopped` SSE event emitted by the harness just before `Done`.

**Event flow:**

1. Agent calls `stop(status, [comment])` during a turn → harness captures a `StopSignal`.
2. After `end_turn`, the harness emits `SessionEvent::Stopped { status, comment }` if a
   stop signal was received, then emits `SessionEvent::Done`.
3. The `issue_watcher` holds the most recent `Stopped` event in memory. On `Done`, it
   calls `park_issue(id, park_status, comment)`:
   - `stop(complete)` → `park_status = Completed`
   - `stop(waiting)` or no stop call → `park_status = Waiting`

**`park_issue` behaviour:**
- Accepts only `Completed` or `Waiting` as target status (rejects anything else).
- If a non-empty `comment` is provided, appends it to the issue's `comments` array
  (author = explicit author arg, or `issue.assignee`, or `"agent"` as fallback) and
  emits a `CommentAdded` event *before* the status transition.
- Updates `issue.status` and emits a `StatusChanged` event.

4. On `Error { message }` — posts `message` as a comment (author = `"system"`), then
   marks the issue `failed`.

The `Stopped` event for a different session is ignored (the watcher filters by
`session_id`).

**If the agent never calls `stop`:** the session ends as `Waiting` and the issue
transitions to `Waiting` with no comment added.

## Comment Protocol

Comments are stored in the `issues.comments` JSON array with `author`, `created_at`, and `body` fields. The `author` is `"user"` for human comments, the agent name for agent output, and `"system"` for server-generated notices such as orphan recovery messages. The `comment` flag on `ns2 issue complete` and `ns2 issue reopen` appends a user comment before the status transition, ensuring it is visible in history when an agent resumes.

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

`ns2 issue reopen --id <id> [--comment <text>] [--start]` moves a `failed`, `completed`,
or `waiting` issue back to `open`. **Behavior differs by prior state:**

- **`failed` → reopen** — clears `session_id` so `issue start` creates a fresh session.
  The failed session's history is not replayed (the harness is long dead).
- **`completed` → reopen** — keeps `session_id` so `issue start` resumes the existing
  session with full history. This lets the agent continue from where it left off.
- **`waiting` → reopen** — keeps `session_id` so `issue start` resumes the existing
  session with full history. This is the primary continuation path when an agent has
  paused for human input.

For all states:
- Existing comments are preserved — the history of what happened is retained.
- If `--comment <text>` is provided, a comment with `author = "user"` is appended
  **before** the status transition, so it is visible in history when the agent resumes.
- If `--start` is provided, `issue start` is called immediately after reopening.
- The `updated_at` timestamp is refreshed.
- Only `failed`, `completed`, and `waiting` issues can be reopened. Attempting to reopen
  an `open` or `running` issue returns an error.

After reopening, the normal lifecycle applies.

## Validation Rules

`ns2 issue start` requires the issue to be in `open` state and to have an assignee whose agent file exists in `.ns2/agents/`. `ns2 issue complete` requires a `--comment` and the issue must not already be terminal. `ns2 issue reopen` requires `failed`, `completed`, or `waiting` state. `ns2 issue new --start` requires `--assignee`. Cancellation is allowed from `open`, `running`, or `waiting` states.

## Connect Sections

- **session lifecycle:** `specs/session-lifecycle.spec.md` — session states, orphan sweep, SSE event stream
- **CLI commands:** `specs/cli-commands/issue.spec.md` — `ns2 issue` subcommand reference
- **data model:** `specs/data-model.spec.md` — schema for issues and comments
- **architecture:** `specs/architecture.spec.md` — crate dependency rules