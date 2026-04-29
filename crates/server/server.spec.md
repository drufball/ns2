# server crate — module responsibilities

## Module overview

```
server/src/
  lib.rs           — router construction, AppState initialization, run()
  state.rs         — AppState, session registry, broadcast channel management
  routes/
    mod.rs         — shared Error / Result types and IntoResponse impl
    session.rs     — session CRUD and SSE streaming handlers
    issue.rs       — issue route handlers (delegate to issues crate)
```

---

## lib.rs

Owns the public API surface of the crate:

- `ServerConfig` — server startup parameters (port, data dir, pid file, model).
- `run(config)` — entry point: initializes the database, client, tools, and `AppState`; calls `orphan_sweep`; binds the TCP listener; runs the axum server with graceful SIGTERM/Ctrl-C handling.
- `build_router(state)` — wires all route paths to their handler functions and attaches `AppState`.
- Re-exports `Error` and `Result` from `routes` so callers can use `server::Error` directly.

`lib.rs` contains no business logic. Its only job is wiring.

---

## state.rs

Single owner of runtime state shared across all request handlers.

### AppState

`AppState` holds:

- `db` — database handle.
- `issue_service` — pure domain service for issue lifecycle operations.
- `sessions` — broadcast-sender registry: maps `session_id → broadcast::Sender<SessionEvent>`. Used by SSE streaming to push live events to connected clients.
- `msg_senders` — mpsc-sender registry: maps `session_id → mpsc::Sender<String>`. Used to deliver messages to an already-running harness task.
- `spawning` — set of session ids for which a harness spawn is currently in flight (guards against double-spawn on concurrent requests).
- `client`, `tools`, `model` — dependencies forwarded to each harness task.

No other module may hold mutable access to `sessions`, `msg_senders`, or `spawning`. All mutations to these maps happen exclusively inside `state.rs`.

### spawn_harness_sync

The single function that starts a harness task:

1. Creates a `broadcast` channel (for SSE) and an `mpsc` channel (for inbound messages).
2. Registers both senders in the `AppState` maps under `session_id`.
3. Removes the session id from the `spawning` guard once the task is live.
4. Optionally attaches an `issue_watcher` task that listens on the broadcast channel and writes the final turn text as a comment when the session completes or fails.
5. Cleans up both maps when the harness task finishes.

---

## routes/mod.rs

Defines types shared across all route modules:

- `Error` — the server error enum (`Db`, `Io`, `NotFound`, `BadRequest`). Implements `IntoResponse` to map errors to HTTP status codes and JSON error bodies.
- `Result<T>` — type alias for `std::result::Result<T, Error>`.
- Includes the `From<issues::Error>` conversion so issue service errors bubble up cleanly.

---

## routes/session.rs

Thin HTTP handlers for session management and SSE streaming. Each handler:

1. Parses the request (path params, query params, JSON body).
2. Calls `state.db` or `state::spawn_harness_sync`.
3. Returns a response.

No business logic lives here.

### Handlers

| Method | Path | Handler |
|--------|------|---------|
| GET | /health | `health` |
| POST | /sessions | `create_session` |
| GET | /sessions | `list_sessions` |
| GET | /sessions/:id | `get_session` |
| GET | /sessions/:id/events | `session_events` |
| POST | /sessions/:id/messages | `send_message` |
| PATCH | /sessions/:id/status | `update_session_status` |
| GET | /sessions/:id/last_text | `session_last_text` |

`session_events` builds a historical SSE stream from DB turns and chains it with a live `BroadcastStream` when a harness is active.

`send_message` handles the spawn-on-demand logic: if the session has no live harness, it spawns one via `spawn_harness_sync`. A `spawning` guard prevents duplicate harness spawns under concurrent requests.

---

## routes/issue.rs

Thin HTTP handlers for issue management. Delegates all lifecycle transitions (start, complete, reopen) to `issues::IssueService`. Handlers only parse HTTP input, call the service, and return responses.

### Handlers

| Method | Path | Handler |
|--------|------|---------|
| POST | /issues | `create_issue` |
| GET | /issues | `list_issues` |
| GET | /issues/:id | `get_issue` |
| PATCH | /issues/:id | `edit_issue` |
| POST | /issues/:id/comments | `add_comment` |
| POST | /issues/:id/start | `start_issue` |
| POST | /issues/:id/complete | `complete_issue` |
| POST | /issues/:id/reopen | `reopen_issue` |

### Helpers

- `generate_issue_id()` — generates a 4-character alphanumeric id.
- `slugify(title)` — converts a title into a URL-safe branch slug.

---

## Invariants

- **Route handlers must be thin.** No business logic inline — parse input, call a service or the DB, return a response.
- **`state.rs` is the single owner of broadcast channels and the session registry.** All harness spawning goes through `spawn_harness_sync`; nothing else may insert into or remove from `sessions` or `msg_senders`.
- **No behavior changes.** This module structure is a pure structural refactor of the original monolithic `lib.rs`.
