---
targets:
  - crates/server/src/**/*.rs
  - crates/server/Cargo.toml
verified: 2026-05-10T18:53:22Z
---

# server crate

The server crate is the HTTP layer for ns2. It owns the axum router, the runtime state shared across request handlers, and the logic that spawns agent harnesses and streams their output to SSE clients.

## What it does

The server handles these families of HTTP endpoints: session management, issue management, SSE event streaming, external webhook ingestion, hook CRUD, and named-event CRUD. Route handlers are intentionally thin — they parse input, call a service or the database, and return a response. No business logic lives in a handler.

On startup, `run(config)` initializes the database, constructs `AppState`, spawns background tasks (hook evaluator, timer scheduler, issue lifecycle subscriber), runs the orphan sweep (see Session Lifecycle Spec), and binds the TCP listener with graceful SIGTERM/Ctrl-C shutdown.

## Key modules

- **`lib.rs`** — router construction and the public `run()` entry point. Pure wiring, no business logic.
- **`state.rs`** — owns `AppState` and the `spawn_harness_sync` function. Single authority over all runtime maps.
- **`routes/`** — thin handlers for sessions, issues, webhooks, hooks, named events, and shared error types. Delegates to `state.rs` or the `issues` crate service.

## Background tasks

Three background tasks are spawned at server startup:

- **Hook evaluator** — subscribes to the `EventBus` and fires hooks whose `event_names` and optional `filter` match incoming `SystemEvent`s.
- **Timer scheduler** (`hooks::timer::spawn_timer_scheduler`) — wakes every 30 seconds, queries all enabled timer events from `EventStore`, and emits `SystemEvent::TimerFired { event_id, event_name, fired_at }` for any whose 5-field cron schedule falls within the rolling window. Hooks listening for `timer.<name>` are matched by the evaluator.
- **Issue lifecycle subscriber** (`spawn_issue_lifecycle_subscriber`) — subscribes to the `EventBus` to drive issue status transitions from session events.

## AppState and channel ownership

`AppState` holds the database handle, the issue service, the event bus, the hook store, the event store, and two ephemeral maps:

- `sessions` — maps `session_id → broadcast::Sender<SessionEvent>`, used by SSE streaming to push live events.
- `msg_senders` — maps `session_id → mpsc::Sender<String>`, used to deliver messages to a running harness.

No module other than `state.rs` may insert into or remove from these maps. All harness spawning goes through `spawn_harness_sync`, which creates both channels, registers them atomically, and cleans them up when the harness exits.

## SSE event stream

`GET /events` is the Server-Sent Events endpoint. Query parameters:

- `session_id` — filter to events from a specific session
- `issue_id` — filter to events related to a specific issue
- `types` — comma-separated broad event type names (`session`, `issue`, `external`, `timer`, `mcp`). If absent, all event types are emitted.
- `last_turns` — when `session_id` is set, limit historical replay to the last N turns. `0` skips all history.
- `event_type` — fine-grained event type filter (e.g. `"mcp.channel_notification"`). When set, only events matching this exact type are passed.
- `channel_id` — when set, only `McpChannelNotification` events with a matching `channel_id` are passed. Used by `ns2 mcp` to subscribe to a personal channel.

The `event_type` and `channel_id` filters are used together by `ns2 mcp` to efficiently receive only the notifications intended for a specific developer channel.

## POST /webhooks/:event_id

External services (e.g. GitHub, CI systems) can trigger events by posting to this endpoint.

**Request flow:**

1. Look up the named Event by `event_id` from `EventStore`; return 404 if not found, not a Webhook-kind event, or disabled.
2. **HMAC verification** (only when the event has a `secret` configured): reads the `X-Hub-Signature-256` header (expected format: `sha256=<hex>`), computes HMAC-SHA256 of the raw request body using the secret, and compares with constant-time `verify_slice`. Returns 401 on missing, malformed, or mismatched signature. Skips the check entirely when no secret is set.
3. Parse the request body as JSON; return 400 on invalid JSON.
4. Emit `SystemEvent::External { event_id, event_name, payload }` to the event bus.
5. Return `200 OK` with body `{"ok": true}`.

Hooks that include `external.<name>` in their `event_names` list are then matched by the hook evaluator via the normal filter path.

## Named-event routes

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/named-events` | Create a named event (webhook or timer) |
| `GET` | `/named-events` | List all named events |
| `GET` | `/named-events/:id` | Get a named event by ID |
| `DELETE` | `/named-events/:id` | Delete a named event |

`POST /named-events` validates timer cron schedules before persisting. If the `schedule` field cannot be parsed, the server returns `400 Bad Request` with `{ "error": "invalid cron schedule: ..." }`. Returns `201 Created` on success.

## Hook validation

`POST /hooks` accepts an `event_names` array instead of a `source` field.

## Invariants

- **Route handlers must be thin.** Parse input, call a service or the DB, return a response.
- **`state.rs` is the single owner of broadcast channels and the session registry.** Nothing else touches the channel maps.
- **`spawn_harness_sync` is the only spawn path.** A `spawning` guard in `AppState` prevents double-spawn under concurrent requests.
- **Webhook HMAC is constant-time.** Signature comparison uses `hmac::Mac::verify_slice`, not string equality, to prevent timing attacks.

## Connect Sections

- **session lifecycle:** `specs/session-lifecycle.spec.md` — session states, orphan sweep, SSE event stream
- **issue lifecycle:** `specs/issue-lifecycle.spec.md` — issue states and transitions
- **architecture:** `specs/architecture.spec.md` — crate dependency rules