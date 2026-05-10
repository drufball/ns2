---
targets:
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
  - crates/cli/src/**/*.rs
verified: 2026-05-10T12:13:04Z
---

# Issue Lifecycle Spec

## Overview

Issues are the primary user-facing work unit. Each issue moves through a simple lifecycle
driven by the agent session it spawns. The DB is the single source of truth; the server
holds no authoritative issue state in memory.

## States and Transitions

```
open → in_progress → completed
                   ↘ failed
                   ↘ waiting
     ↑ (reopen)
```

- **`open`** — issue created, not yet started. The default state after `ns2 issue new`.
- **`in_progress`** — `PATCH /issues/:id/status` with `in_progress` has been called; a session is active.
- **`completed`** — the agent called `stop(complete)`. Terminal.
- **`waiting`** — the agent called `stop(waiting)` or the session ended without calling
  `stop`. The issue is paused for human input. Terminal (the session is still associated
  and its history is preserved; reopen to continue).
- **`failed`** — the agent session hit an error, or the server restarted while the
  session was active (orphan recovery). Terminal unless explicitly reopened.
- **`cancelled`** — manually cancelled. Terminal.

`failed`, `completed`, and `waiting` issues can be moved back to `open` via
`ns2 issue reopen`. The behavior differs by prior state — see the Reopen section below.

## Stop-Tool-Driven Issue Completion

When `PATCH /issues/:id/status` with `in_progress` is received, the server starts or resumes the
linked session via `spawn_harness_sync`. A single global **issue lifecycle subscriber**
(`spawn_issue_lifecycle_subscriber` in `server/lib.rs`) subscribes to the `EventBus` and
drives all issues to their terminal states — there is no per-session watcher task.

**Event flow:**

1. Agent calls `stop(status, [comment])` during a turn → harness captures a `StopSignal`.
2. After `end_turn`, the harness emits `SessionEvent::Stopped { status, comment }` if a
   stop signal was received, then emits `SessionEvent::Done`.
3. The lifecycle subscriber holds a `stop_map` (keyed by `session_id`). On `Stopped`, it
   inserts the status and comment. On `Done`, it looks up the entry and calls
   `park_issue(id, park_status, comment)`:
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

**If the agent never calls `stop`:** the session ends as `Waiting` and the issue
transitions to `Waiting` with no comment added.

## Issue State → Session Actions

The lifecycle subscriber also reacts to issue `StatusChanged` events:

- **`InProgress`** — calls `handle_in_progress`, which spawns a new harness (or resumes
  an existing `Waiting` session). This is the bridge from status change to agent execution.
- **`Cancelled`** — kills the active session if one is linked: drops the `msg_senders`
  entry and marks the session `Cancelled` in the DB.

## Comment Protocol

Comments are stored in the `issues.comments` JSON array with `author`, `created_at`, and `body` fields. The `author` is `"user"` for human comments, the agent name for agent output, and `"system"` for server-generated notices such as orphan recovery messages. The `comment` flag on `ns2 issue complete` and `ns2 issue reopen` appends a user comment before the status transition, ensuring it is visible in history when an agent resumes.

## Opening Prompt

When a session is started for an issue, `issues::build_initial_message(issue)` formats the
issue's title, body, and any comment history into the opening agent prompt. Comments are
rendered with author, timestamp, and body under an `# Issue History` header. This is a
pure function in the `issues` crate with no side effects.

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

`ns2 issue reopen --id <id> [--comment <text>]` moves a `failed`, `completed`,
or `waiting` issue back to `open`. **Behavior differs by prior state:**

- **`failed` → reopen** — clears `session_id` so the next `in_progress` transition creates a fresh session.
  The failed session's history is not replayed (the harness is long dead).
- **`completed` → reopen** — keeps `session_id` so the next `in_progress` transition resumes the existing
  session with full history. This lets the agent continue from where it left off.
- **`waiting` → reopen** — keeps `session_id` so the next `in_progress` transition resumes the existing
  session with full history. This is the primary continuation path when an agent has
  paused for human input.

For all states:
- Existing comments are preserved — the history of what happened is retained.
- If `--comment <text>` is provided, a comment with `author = "user"` is appended
  **before** the status transition, so it is visible in history when the agent resumes.
- The `updated_at` timestamp is refreshed.
- Only `failed`, `completed`, and `waiting` issues can be reopened. Attempting to reopen
  an `open` or `in_progress` issue returns an error.

After reopening, the normal lifecycle applies.

## Resume (`IssueService::resume_issue`)

`resume_issue(id)` atomically transitions a `Waiting` issue (with a linked session) to
`InProgress` and emits a `StatusChanged` event. The lifecycle subscriber reacts to the
event by resuming the session. This is distinct from `start_issue` (which also handles
`open` issues and fresh session creation) and is used when a waiting issue is continued
directly without going through `open`.

## Validation Rules

`PATCH /issues/:id/status` with `in_progress` requires the issue to have an assignee whose agent file exists in `.ns2/agents/`. `ns2 issue complete` requires a `--comment` and the issue must not already be terminal. `ns2 issue reopen` requires `failed`, `completed`, or `waiting` state. Cancellation is allowed from `open`, `in_progress`, or `waiting` states.

## Connect Sections

- **session lifecycle:** `specs/session-lifecycle.spec.md` — session states, orphan sweep, SSE event stream
- **CLI commands:** `specs/cli-commands/issue.spec.md` — `ns2 issue` subcommand reference
- **data model:** `specs/data-model.spec.md` — schema for issues and comments
- **architecture:** `specs/architecture.spec.md` — crate dependency rules