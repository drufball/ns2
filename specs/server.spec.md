---
targets:
  - crates/server/src/**/*.rs
  - crates/server/Cargo.toml
verified: 2026-05-10T14:01:16Z
---

# server crate

The server crate is the HTTP layer for ns2. It owns the axum router, the runtime state shared across request handlers, and the logic that spawns agent harnesses and streams their output to SSE clients.

## What it does

The server handles four families of HTTP endpoints: session management, issue management, SSE event streaming, and external webhook ingestion. Route handlers are intentionally thin — they parse input, call a service or the database, and return a response. No business logic lives in a handler.

On startup, `run(config)` initializes the database, constructs `AppState`, spawns background tasks (hook evaluator, timer scheduler), runs the orphan sweep (see Session Lifecycle Spec), and binds the TCP listener with graceful SIGTERM/Ctrl-C shutdown.

## Key modules

- **`lib.rs`** — router construction and the public `run()` entry point. Pure wiring, no business logic.
- **`state.rs`** — owns `AppState` and the `spawn_harness_sync` function. Single authority over all runtime maps.
- **`routes/`** — thin handlers for sessions, issues, webhooks, hooks, and shared error types. Delegates to `state.rs` or the `issues` crate service.

## Background tasks

Three background tasks are spawned at server startup:

- **Hook evaluator** — subscribes to the `EventBus` and fires internal hooks whose `event_types` and `filter` match incoming `SystemEvent`s.
- **Global issue lifecycle subscriber** (`spawn_issue_lifecycle_subscriber`) — subscribes to the `EventBus` and drives issue state in response to session and issue events: `SessionEvent::Stopped`/`Done` park the linked issue to `Completed` or `Waiting`; `SessionEvent::Error` marks it `failed`; `IssueEvent::StatusChanged { to: InProgress }` spawns or resumes the harness; `IssueEvent::StatusChanged { to: Cancelled }` drops the msg sender to terminate the harness.
- **Timer scheduler** (`hooks::timer::spawn_timer_scheduler`) — wakes every 30 seconds, queries all enabled timer hooks, and emits `SystemEvent::TimerFired` for any whose 5-field cron schedule falls within the 60-second rolling window ending at `now`. The action is then executed in a spawned task identical to the evaluator path.

## Hook validation

`POST /hooks` validates timer hook schedules before persisting. If the `schedule` field cannot be parsed as a 5-field cron expression, the server returns `400 Bad Request` with `{ "error": "invalid cron schedule: ..." }`.


## AppState and channel ownership

`AppState` holds the database handle, the issue service, and two ephemeral maps:

- `sessions` — maps `session_id → broadcast::Sender<SessionEvent>`, used by SSE streaming to push live events.
- `msg_senders` — maps `session_id → mpsc::Sender<String>`, used to deliver messages to a running harness.

No module other than `state.rs` may insert into or remove from these maps. All harness spawning goes through `spawn_harness_sync`, which creates both channels, registers them atomically, and cleans them up when the harness exits.

## POST /webhooks/:hook_id

External services (e.g. GitHub, CI systems) can trigger hook actions by posting to this endpoint.

**Request flow:**

1. Look up the hook by `hook_id`; return 404 if not found, not an `External` hook, or disabled.
2. **HMAC verification** (only when the hook has a `secret` configured): reads the `X-Hub-Signature-256` header (expected format: `sha256=<hex>`), computes HMAC-SHA256 of the raw request body using the secret, and compares with constant-time `verify_slice`. Returns 401 on missing, malformed, or mismatched signature. Skips the check entirely when no secret is set.
3. Parse the request body as JSON; return 400 on invalid JSON.
4. Emit `SystemEvent::External { hook_id, payload }` to the event bus.
5. Return `200 OK` with body `{"ok": true}`.

## Hook evaluator and External events

`spawn_hook_evaluator` (in `lib.rs`) runs a background task subscribed to the event bus. When it receives a `SystemEvent::External` event, it takes an early branch before the normal `matches_event` loop:

1. Looks up the hook directly by `hook_id` from the event.
2. If found and enabled, spawns a task to run its action via `hooks::execute::run_action`.
3. Skips the general `matches_event` loop (which only handles Internal hooks) via `continue`.

This means External hooks are dispatched without a filter scan — the route has already validated the hook identity, so the evaluator just runs the action directly.

## Invariants

- **Route handlers must be thin.** Parse input, call a service or the DB, return a response.
- **`state.rs` is the single owner of broadcast channels and the session registry.** Nothing else touches the channel maps.
- **`spawn_harness_sync` is the only spawn path.** A `spawning` guard in `AppState` prevents double-spawn under concurrent requests.
- **Webhook HMAC is constant-time.** Signature comparison uses `hmac::Mac::verify_slice`, not string equality, to prevent timing attacks.

## Connect Sections

- **session lifecycle:** `crates/server/src/session-lifecycle.spec.md` — session states, orphan sweep, SSE event stream
- **issue lifecycle:** `crates/server/src/issue-lifecycle.spec.md` — issue states and transitions
- **architecture:** `crates/arch-tests/architecture.spec.md` — crate dependency rules