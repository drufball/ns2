---
targets:
  - crates/server/src/**/*.rs
  - crates/server/Cargo.toml
verified: 2026-04-29T17:13:14Z
---

# server crate

The server crate is the HTTP layer for ns2. It owns the axum router, the runtime state shared across request handlers, and the logic that spawns agent harnesses and streams their output to SSE clients.

## What it does

The server handles three families of HTTP endpoints: session management, issue management, and SSE event streaming. Route handlers are intentionally thin — they parse input, call a service or the database, and return a response. No business logic lives in a handler.

On startup, `run(config)` initializes the database, constructs `AppState`, runs the orphan sweep (see Session Lifecycle Spec), and binds the TCP listener with graceful SIGTERM/Ctrl-C shutdown.

## Key modules

- **`lib.rs`** — router construction and the public `run()` entry point. Pure wiring, no business logic.
- **`state.rs`** — owns `AppState` and the `spawn_harness_sync` function. Single authority over all runtime maps.
- **`routes/`** — thin handlers for sessions, issues, and shared error types. Delegates to `state.rs` or the `issues` crate service.

## AppState and channel ownership

`AppState` holds the database handle, the issue service, and two ephemeral maps:

- `sessions` — maps `session_id → broadcast::Sender<SessionEvent>`, used by SSE streaming to push live events.
- `msg_senders` — maps `session_id → mpsc::Sender<String>`, used to deliver messages to a running harness.

No module other than `state.rs` may insert into or remove from these maps. All harness spawning goes through `spawn_harness_sync`, which creates both channels, registers them atomically, and cleans them up when the harness exits.

## Invariants

- **Route handlers must be thin.** Parse input, call a service or the DB, return a response.
- **`state.rs` is the single owner of broadcast channels and the session registry.** Nothing else touches the channel maps.
- **`spawn_harness_sync` is the only spawn path.** A `spawning` guard in `AppState` prevents double-spawn under concurrent requests.

## Connect Sections

- **session lifecycle:** `crates/server/src/session-lifecycle.spec.md` — session states, orphan sweep, SSE event stream
- **issue lifecycle:** `crates/server/src/issue-lifecycle.spec.md` — issue states and transitions
- **architecture:** `crates/arch-tests/architecture.spec.md` — crate dependency rules