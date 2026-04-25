---
targets:
  - crates/server/src/**/*.rs
  - crates/harness/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
verified: 2026-04-25T20:27:39Z
---

# Session Lifecycle Spec

## Overview

Sessions are the internal agent run units that power issues. Every session is fully
reconstructable from SQLite — the server holds no authoritative state beyond the active
SSE broadcast channels and the in-progress harness tasks that feed them.

## States and Transitions

```
created → running → completed
                 ↘ failed
                 ↘ cancelled
```

- **`created`** — session record exists; no harness is running. Occurs when a session
  is created with no initial message, or when the server restarts and the in-memory
  harness map is empty.
- **`running`** — a harness task is active and processing turns.
- **`completed`** — the harness finished with `stop_reason: end_turn` (or equivalent).
  Terminal; the session's turn history is fully persisted.
- **`failed`** — the harness encountered an unrecoverable error, the API key was missing,
  or the server restarted while the session was `running` (orphan recovery, see below).
  Terminal unless explicitly reopened.
- **`cancelled`** — manually cancelled by the user or orchestrator. Terminal.

There is no `waiting` state in the current implementation (reserved for a future input-
request tool).

## Persistence Model

SQLite is the single source of truth. Everything the harness produces is persisted
before it is used to build the next turn's context:

- `sessions` table — id, name, agent type, status, branch, timestamps
- `turns` table — one row per completed turn, with token count
- `content_blocks` table — one row per content block within a turn (text, tool_use,
  tool_result, thinking), with role, ordering index, and JSON-encoded content

Because all history is in SQLite, a fresh harness can reconstruct the full conversation
by loading every content block for the session, ordered by turn and block index, and
presenting them to the API as the prior `messages` array. No in-memory state beyond the
SSE broadcast channel and the message-queue sender is needed.

## In-Memory State (the only exception)

Two maps live in the server's `AppState`:

- `sessions: Mutex<HashMap<Uuid, broadcast::Sender<SessionEvent>>>` — the SSE broadcast
  channel for each live session. Dropped when the harness exits; SSE clients that
  reconnect after a restart receive history-only streams.
- `msg_senders: Mutex<HashMap<Uuid, mpsc::Sender<String>>>` — the message queue channel
  for each running harness. Dropped on harness exit.

These maps are intentionally ephemeral. Nothing in the DB depends on them.

## Resume After Restart (`Completed` Sessions Accept New Messages)

A session in `completed` state has no live harness. When `POST /sessions/:id/messages`
is called on such a session:

1. A new harness is spawned.
2. The harness loads the full turn history from SQLite via `db.list_turns_with_blocks`.
3. The full history is presented to the Anthropic API as the `messages` array so the
   model has complete context.
4. The session transitions back to `running`.
5. SSE listeners that were connected before the restart are gone; new SSE subscribers
   can connect and receive the new turn's events live.

The `created` and `running` states already handle this path. The `completed` state adds
the same spawn-on-demand behaviour so multi-turn sessions work seamlessly across restarts.

A `failed` or `cancelled` session must first be explicitly reopened (see Issue Lifecycle
Spec, `ns2 issue reopen`) before accepting new messages.

## Server-Restart Orphan Recovery

On `serve()` startup, before accepting any requests, the server performs an orphan sweep:

1. Query the DB for all sessions with status `running`.
2. For each such session, the harness map is empty (cold start), so the session is
   orphaned.
3. Transition the session to `failed` with `updated_at = now`.
4. If the session is linked to an issue (identified via `issues.session_id`), mark that
   issue `failed` and append a comment:
   ```
   session lost on server restart
   ```
   The comment author is set to `"system"`.

The orphan sweep happens synchronously (with the DB) before the HTTP listener accepts
connections, so clients never observe a half-recovered state.

## SSE Event Stream

`GET /sessions/:id/events` replays history then streams live:

1. Load all turns + blocks from SQLite in order.
2. Emit `TurnStarted`, `ContentBlockStarted`, `ContentBlockDelta` (one per stored text
   delta), `ContentBlockDone`, `TurnDone` events for each persisted turn.
3. If the session is terminal, emit `SessionDone` and close the stream.
4. If the session is live, subscribe to the broadcast channel and forward events as they
   arrive.

The `?last_turns=N` query parameter limits history replay to the final N turns (0 = skip
history entirely, absent = all turns).

## Concurrency Invariants

- Exactly one harness runs per session at a time. The `spawning` set in `AppState` is
  the mutex-guarded "in flight" guard that prevents two concurrent `send_message` calls
  from both reaching the spawn path.
- The broadcast sender and message sender are inserted into their maps atomically (under
  a combined lock) before the spawning task yields, so late-arriving concurrent requests
  always find the sender in the map.

## Connect Sections

- **harness:** `crates/harness/agent-harness.spec.md` — turn loop, context window
  construction, tool dispatch
- **data model:** `crates/db/data-model.spec.md` — schema, migrations, query interface
- **architecture:** `crates/arch-tests/architecture.spec.md` — crate dependency rules
- **issue lifecycle:** `crates/server/src/issue-lifecycle.spec.md` — issue states,
  orphan recovery from the issue perspective