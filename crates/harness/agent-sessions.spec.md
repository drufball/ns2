---
targets:
  - crates/harness/src/**/*.rs
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
severity: warning
verified: 2026-04-26T17:28:05Z
---

# Agent Sessions Spec

## Overview

Sessions work like `tmux`: 

- They run in the background to completion across many agent turns and tool calls. 
- Users can attach to any session to replay history and monitor live activity. 
- Messages sent by the user (or their proxy) to a session are queued for the next available turn. 
- Agents can request input via a dedicated tool, which transitions the session to a `waiting` state.

## Orchestrator Agent

Agent sessions support manual, tmux-style interaction of listing and attaching to sessions, but the default interaction will be through a top-level orchestrating agent. This agent has access to all of the same tools as a user: listing sessions, tailing recent turns, sending messages, etc.

Most of the time, the user will work with the orchestrator and it will handle summarising & managing sessions.

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

- Arbitrary number of sessions run concurrently
- Meta-agent session is always present (created on first launch, persisted across restarts)
- Each session is an independent tokio task

## TUI Interaction

- Attach/detach is non-destructive — session continues regardless
- On attach: server replays full history as SSE events, then streams live
- Multiple TUI clients can attach to the same session simultaneously
- Default TUI view is the meta-agent session