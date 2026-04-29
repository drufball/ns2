---
targets:
  - Cargo.toml
  - crates/*/Cargo.toml
verified: 2026-04-29T17:14:15Z
---

# Architecture Spec

## Design Principles

Flat set of crates, each owning one layer. Dependencies are a directed acyclic graph — any crate can be understood, tested, and replaced without reading the others.

**Do not add dependencies to a crate's `Cargo.toml` to work around an architectural boundary.** Adding a dep to the wrong crate is a violation, not a shortcut.

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

    harness --> db & anthropic & tools & agents & workspace

    issues --> db

    server --> issues & db & anthropic & tools & harness

    tui --> types
    cli --> agents & specs & workspace & server & types
```

Arrows point from dependent to dependency.

## Crates

**`types`** — shared domain types: `Session`, `Turn`, `ContentBlock`, `Issue`, SSE events, tool shapes. No behavior.

**`db`** — SQLite access via sqlx. Owns schema, migrations, and every query.
_Doesn't own: SQL written anywhere else._

**`anthropic`** — HTTP client for the Anthropic Messages API. Streaming SSE parsing, request/response types.

**`tools`** — `Tool` trait plus `bash`, `read`, `write`, `edit` implementations.
_Doesn't own: session or turn state._

**`workspace`** — git worktree management and `git_root()` discovery.

**`agents`** — reads/writes `.ns2/agents/*.md` agent definition files.

**`specs`** — reads/writes `.spec.md` files; staleness checking.

**`harness`** — agent turn loop. Context window construction, system prompt loading, tool dispatch, worktree resolution. Emits events to a broadcast channel; one instance per active session.
_Doesn't own: issue lifecycle or state transitions — that's `issues`. No HTTP._

**`issues`** — pure issue domain service. Owns the state machine (open → running → completed/failed/cancelled), `start_issue`, `complete_issue`, `reopen_issue`, `orphan_sweep`. Exposes `IssueService` and `StartIssueOutcome`.
_Doesn't own: HTTP routing, harness spawning, or session maps — those belong in `server`._
**Dependencies:** `types`, `db`.

**`server`** — axum HTTP server. Routes, SSE fan-out, `ServerConfig`, session maps, harness spawning. Constructs the Anthropic client, standard tools, and `spawn_harness_sync`. Delegates issue lifecycle to `IssueService` but owns all harness lifecycle.
_Doesn't own: issue business logic — delegate to `issues`._

**`tui`** — ratatui terminal UI. Connects to the server via SSE. Thin client: all state comes from the server.

**`cli`** — the `ns2` binary. Wires crates; contains no logic of its own. Depends directly on `server` to start the in-process server, and on `types` for shared domain types.
_Doesn't own: Anthropic client init or harness instantiation — that's `server`'s job._