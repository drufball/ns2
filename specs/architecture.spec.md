---
targets:
  - Cargo.toml
  - crates/*/Cargo.toml
verified: 2026-05-10T18:05:27Z
---

# Architecture Spec

## Design Principles

Flat set of crates, each owning one layer. Dependencies are a directed acyclic graph — any crate can be understood, tested, and replaced without reading the others.

**Do not add dependencies to a crate's `Cargo.toml` to work around an architectural boundary.** Adding a dep to the wrong crate is a violation, not a shortcut.

**Crates that own an external dependency boundary (database, HTTP) must not expose their concrete implementation types.** Use `pub(crate)` for types like `SqliteDb`, `SqliteHookStore`, etc. and expose only traits and factory functions. Upper-layer crates must couple to abstractions, not implementations. A factory function (e.g. `db::connect() -> (Arc<dyn Db>, Arc<dyn HookStore>, Arc<dyn EventStore>)`) is the correct seam.

When adding a substantial new unit of responsibilities or business logic, consider whether a new crate should be created instead of adding it to an existing one.

## Dependency Graph

```mermaid
graph TD
    types
    workspace

    db --> types
    anthropic --> types
    tools --> types
    agents --> workspace
    specs --> workspace

    events --> types

    harness --> db & anthropic & tools & agents & workspace & events

    issues --> db & events

    hooks --> events & db & issues & types

    server --> issues & db & anthropic & tools & harness & events & hooks

    tui --> types
    cli --> agents & specs & workspace & server & types
```

Arrows point from dependent to dependency.

> **Known violations (tracked):** `hooks` depends on `issues` — reaches across layer boundaries. Tracked in GH#98.

## Crates

**`types`** — shared domain types: `Session`, `Turn`, `ContentBlock`, `Issue`, tool shapes. No behavior.

**`db`** — SQLite access via sqlx. Owns schema, migrations, and every query.
_Doesn't own: SQL written anywhere else._

**`anthropic`** — HTTP client for the Anthropic Messages API. Streaming SSE parsing, request/response types.

**`tools`** — `Tool` trait plus `bash`, `read`, `write`, `edit` implementations.
_Doesn't own: session or turn state._

**`workspace`** — git worktree management and `git_root()` discovery.

**`agents`** — reads/writes `.ns2/agents/*.md` agent definition files.

**`specs`** — reads/writes `.spec.md` files; staleness checking.

**`events`** — global event bus. Defines `SystemEvent` (the top-level envelope), `SessionEvent` (turn-level harness events), and `IssueEvent` (issue lifecycle events). `SystemEvent::External { event_id, event_name, payload }` carries the named Event's id and name when a webhook is received; `SystemEvent::TimerFired { event_id, event_name, fired_at }` carries them for timer firings. `EventBus` is a cheaply-cloneable `tokio::broadcast` wrapper; `send` is fire-and-forget.

**`harness`** — agent turn loop. Context window construction, system prompt loading, tool dispatch, worktree resolution. Publishes `SystemEvent::Session` events to the `EventBus`; one instance per active session.
_Doesn't own: issue lifecycle or state transitions — that's `issues`. No HTTP._

**`issues`** — pure issue domain service. Owns the state machine (open → in_progress → completed/failed/waiting), `start_issue`, `resume_issue`, `complete_issue`, `reopen_issue`, `orphan_sweep`. Exposes `build_initial_message` (pure function that formats an issue's title, body, and comment history into the opening agent prompt). Publishes `SystemEvent::Issue` events to the `EventBus`. Exposes `IssueService` and `StartIssueOutcome`.
_Doesn't own: HTTP routing, harness spawning, or session maps — those belong in `server`._

**`hooks`** — hook filter evaluation, action dispatch, and timer scheduling. Defines `HookAction` (SendMessage/CreateIssue/RunShell) and `HookFilter` (field conditions). Hooks subscribe to events by name via `Hook.event_names: Vec<String>` (e.g. `"issue.created"`, `"external.ci-complete"`, `"timer.heartbeat"`, or `"*"` to match all). Modules: `evaluate` (event matching against `event_names`), `execute` (action dispatch), `template` (minijinja rendering), `cron` (5-field cron → next fire time via the `cron` crate), and `timer` (`process_timer_events` + `spawn_timer_scheduler` background loop that reads timer events from `EventStore`). Also exposes `generate_event_id()` for creating short random IDs for named events.
_Known violation tracked in GH#98 (direct `issues` dep)._

**`server`** — axum HTTP server. Routes, `ServerConfig`, session maps, harness spawning. Holds an `EventBus` and an `EventStore` in `AppState` shared by all routes and background tasks. Exposes `GET /events` as an SSE endpoint that replays session history from DB then streams live `SystemEvent`s with optional `session_id`, `issue_id`, and `types` filters. Exposes `POST /webhooks/:event_id` for external webhook ingestion: looks up the named Event from `EventStore`, validates the HMAC-SHA256 signature (if a secret is set), and publishes `SystemEvent::External`. Named-event CRUD is at `POST /named-events`, `GET /named-events`, `GET /named-events/:id`, `DELETE /named-events/:id`. Hook CRUD is at `/hooks`. Constructs the Anthropic client, standard tools, and `spawn_harness_sync`. Delegates issue lifecycle to `IssueService` but owns all harness lifecycle. On startup, spawns three background tasks: the hook evaluator, the timer scheduler (`hooks::timer::spawn_timer_scheduler` seeded with `EventStore`), and the global issue lifecycle subscriber (`spawn_issue_lifecycle_subscriber`).
_Doesn't own: issue business logic — delegate to `issues`._

**`tui`** — ratatui terminal UI. Connects to the server via SSE. Thin client: all state comes from the server.

**`cli`** — the `ns2` binary. Wires crates; contains no logic of its own. Depends directly on `server` to start the in-process server, and on `types` for shared domain types. Uses `reqwest` directly for HTTP calls to the local ns2 server (health checks, issue/session/hook CRUD, SSE streaming).
_Doesn't own: Anthropic client init or harness instantiation — that's `server`'s job._