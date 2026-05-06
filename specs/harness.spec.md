---
targets:
  - crates/harness/src/**/*.rs
verified: 2026-05-06T18:50:24Z
---

# harness crate

The harness crate runs agent sessions. It owns the turn loop, tool dispatch, hook execution, system prompt construction, and context window management. Everything that happens between "session starts" and "session ends" lives here.

## What it does

Given a session, the harness drives the agent forward: builds the context window, calls the Anthropic API, streams events to SSE subscribers, dispatches tool calls, persists each completed turn to SQLite, and loops until the model signals it's done (or something fails).

## Module overview

| Module | Responsibility |
|---|---|
| `loop_.rs` | The main turn loop and tool dispatch — orchestrates everything else |
| `prompt.rs` | Assembles the system prompt from an agent definition and config files |
| `context.rs` | Token counting and sliding-window construction (reserved for future use) |
| `history.rs` | Persists turns and content blocks to SQLite; emits `SessionEvent`s |
| `hooks.rs` | Executes pre-tool, post-tool, and stop hooks |
| `retry.rs` | Handles Anthropic 429 rate-limit retries with backoff |
| `cwd.rs` | Resolves the session's working directory via git worktree lookup |
| `lib.rs` | Re-exports the public API; hosts the integration test module |

Each leaf module (`prompt`, `history`, `hooks`, `retry`, `cwd`) depends only on external crates — they have no knowledge of each other or of the turn loop. `loop_.rs` is the sole orchestrator.

## How it fits in

The harness is spawned by the server (`crates/server`) as a tokio task per session. It receives user messages via an in-memory channel, streams `SessionEvent`s back via a broadcast channel, and uses `crates/db` as its durable store. It has no knowledge of HTTP — that belongs to the server.