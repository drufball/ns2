---
targets:
  - crates/harness/src/**/*.rs
  - crates/tools/src/**/*.rs
verified: 2026-05-10T21:00:00Z
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

## Turn loop and final status

The harness runs `run_tool_dispatch_loop` in a loop until `end_turn` is received from the model. At that point it:

1. Checks the stop channel for a `StopSignal` sent by the `StopTool` during the preceding tool calls.
2. Runs Stop hooks; if any exit non-zero, injects their stdout as a new user message and re-enters the loop.
3. Determines the final `SessionStatus`:
   - Always `SessionStatus::Waiting`, regardless of whether `stop(complete)`, `stop(waiting)`, or no `stop` was called.
4. If a stop signal was received, emits `SessionEvent::Stopped { status, comment }` on the broadcast channel. The `status` field preserves the agent's intent (`Complete` or `Waiting`) so the global issue lifecycle subscriber can act on it (e.g. mark the linked issue `Completed`). The session itself always ends as `Waiting`.
5. Writes `SessionStatus::Waiting` to the DB and emits `SessionEvent::Done`.

## StopTool

The `stop` tool is auto-injected by the harness at startup (not configurable via `HarnessConfig`). It is always available to the agent. Its schema:

```json
{
  "status": "complete" | "waiting",   // required
  "comment": "<string>"               // optional
}
```

- **`complete`** — task is done; the session ends as `Waiting` and the linked issue becomes `Completed` (driven by the `SessionEvent::Stopped` the global issue lifecycle subscriber receives).
- **`waiting`** — agent needs human input; the session ends as `Waiting` and the linked issue becomes `Waiting`.

When the agent calls `stop`, the tool sends a `StopSignal` over an internal `mpsc` channel. The harness reads it after `end_turn` via `try_recv`. If the agent calls `stop` multiple times in a turn, only the last signal is used.

## SSE events emitted

| Event | When |
|---|---|
| `TurnStarted` | Start of each assistant turn |
| `ContentBlockStarted/Delta/Done` | Streaming content |
| `ToolUseStart/Done` | Each tool call dispatched |
| `TurnDone` | End of each assistant turn |
| `Stopped { status, comment }` | After `end_turn`, only when `stop` was called |
| `Done` | Always, after final status is written |
| `Error { message }` | On unrecoverable failure |

## How it fits in

The harness is spawned by the server (`crates/server`) as a tokio task per session. It receives user messages via an in-memory channel, streams `SessionEvent`s back via a broadcast channel, and uses `crates/db` as its durable store. It has no knowledge of HTTP — that belongs to the server.