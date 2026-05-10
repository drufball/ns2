---
targets:
  - crates/cli/src/commands/session.rs
verified: 2026-05-10T13:58:21Z
---

# ns2 session

Sessions are the internal agent runs that power issues. In normal use you don't create them directly — `ns2 issue start` creates one automatically and links it to the issue. Session commands exist for inspection, debugging, and advanced scripting.

## Lifecycle

A session moves through these states in order:

- **created** — session exists but no message sent yet; agent not started
- **running** — agent is active and processing messages
- **waiting** / **failed** / **cancelled** — terminal states

## When to use session commands directly

The most common reason to drop down to session commands is to watch what an agent is doing in real time. `ns2 session tail --id <uuid>` streams the session's output: model text, tool calls, tool results, and a final `[done]` or `[error]` marker. It blocks until the session finishes and exits non-zero on failure.

`ns2 session wait --id <uuid>` is the quiet alternative to tail — it polls silently and exits once the session reaches a terminal state. Use it in scripts where you don't want the full output stream.

`ns2 session send --id <uuid> --message "..."` injects a follow-up message into a running session. The agent picks it up on its next turn. This only works while the session is in `created` or `running` state.

## Creating sessions directly

`ns2 session new` is occasionally useful for running one-off agent tasks outside the issue workflow. Pass `--agent` to choose an agent type, `--message` to start immediately, and `--wait` to block until the session completes (prints only the final turn's content). Without `--message`, the session waits in `created` state for your first `session send`.

## Listing and stopping

`ns2 session list` shows recent sessions with their ID, name, status, and creation time. Filter by `--status` or look up a specific session with `--id`. Use `ns2 session stop` to cancel a session that's stuck or no longer needed.