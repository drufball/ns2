---
targets:
  - crates/harness/src/**/*.rs
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
severity: warning
verified: 2026-05-10T11:07:23Z
---

# Agent Sessions Spec

## Overview

Sessions work like `tmux`: 

- They run in the background to completion across many agent turns and tool calls. 
- Users can attach to any session to replay history and monitor live activity. 
- Messages sent by the user (or their proxy) to a session are queued for the next available turn. 
- Agents can request input via a dedicated tool, which transitions the session to a `waiting` state.

## Interaction Model

Sessions are managed primarily through the CLI (`ns2 issue start`, `ns2 issue wait`, `ns2 session send`, etc.), which talks to the server over HTTP. Users can also send messages directly to running sessions via the REST API. The CLI exposes commands for listing sessions, tailing recent turns, and sending messages — the same operations an orchestrating agent would use.

## Lifecycle

```
created → running → [waiting → running → ... →] completed | failed | cancelled
```

- **created**: session record written, agent task not yet spawned
- **running**: agent turn loop active in background
- **waiting**: agent has flagged it needs input; loop is paused
- **completed/failed/cancelled**: terminal states; history retained
- **retry:** failed sessions resume from the last completed turn, not from the beginning

## State & Persistence

- **SQLite:** source of truth. Session metadata, status, full turn + message history, tool call results. A fresh harness can reconstruct the complete conversation from the DB with no in-memory dependency.
- **In-memory:** active tokio task handle per running session, SSE broadcast channel per session. These are ephemeral — dropped on harness exit or server restart.
- Server is stateless except for SSE channels — on restart, orphaned `running` sessions are swept to `failed` and can be reopened; `completed` sessions remain intact and accept new messages by spawning a fresh harness that loads history from SQLite.

## Concurrency

- Arbitrary number of sessions run concurrently as independent tokio tasks

## Subscribing to a Session

Any client (CLI, future TUI) can subscribe to a session's SSE stream. On subscribe, the server replays full history as SSE events (filtered by `last_turns` if provided), then streams live events as they arrive. Multiple subscribers can attach to the same session simultaneously — attach and detach are non-destructive.