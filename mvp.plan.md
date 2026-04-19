# MVP Plan: Hello World Scaffold

## Goal

End state: `ns2 session new --message "hello"` kicks off a real agent session, and `ns2 session tail` streams Claude's response to stdout. No tools — just send and receive.

The server is started and stopped manually for the MVP (`ns2 server start` / `ns2 server stop`). CLI commands error clearly if the server isn't running.

Each milestone is a working, integrated system — just with more real implementations replacing stubs. Verification loop (`cargo clippy -- -D warnings && cargo test`) must pass at every milestone.

## Conventions

**Error types:** each crate owns its own `Error` type via `thiserror`, exposed as `<crate>::Error`. Nothing shared in `types`.

---

## Milestone 1 — Binary says hello

**What works:** `ns2 session list` prints hardcoded output. Nothing real, but the workspace compiles, lints, and tests pass.

- [ ] Root `Cargo.toml` workspace with all crates listed as members
- [ ] Stub `lib.rs` for each crate: `types`, `db`, `anthropic`, `harness`, `server`, `tui`
- [ ] `cli` crate: `ns2 session list` prints a hardcoded table row to stdout
- [ ] Verification loop passes

---

## Milestone 2 — Real sessions, no agent

**What works:** `ns2 server start` launches the server, `ns2 session new` creates a session in SQLite, `ns2 session list` reads from db. Session sits in `created` state forever — no harness yet. CLI commands print a clear error if the server isn't running.

New crates: `types`, `db`, `server` (basic routes, no SSE)

- [ ] `types`: `Session`, `SessionStatus`, `Turn`, `ContentBlock` with serde
- [ ] `db`: sqlx migrations, `SessionDb` trait + sqlite impl, `#[sqlx::test]` tests
- [ ] `server`: axum with `POST /sessions`, `GET /sessions`, `GET /sessions/:id`, `GET /health`; in-memory `AppState` with db pool; data dir is `~/.ns2/<repo-name>/` (derived from repo root dirname); SQLite db lives there
- [ ] `cli`: `ns2 server start` launches the server as a foreground process; `ns2 server stop` sends SIGTERM via PID file; `ns2 session new [--name] [--agent] [--message]` and `ns2 session list [--status]` hit real server routes and print a clear error if the server is unreachable

---

## Milestone 3 — SSE streaming + tail command

**What works:** `ns2 session tail` connects to the server and streams session events to stdout. Server emits fake events for running sessions. Fully verifiable from the CLI.

> **Verify with:** `ns2 session tail --id <id>` after creating a session. Also verifiable with `curl`.

New crates: none

- [ ] `types`: SSE event envelope types
- [ ] `db`: `TurnDb`, `ContentBlockDb` traits + sqlite impls
- [ ] `server`: `GET /sessions/:id/events` SSE route; on connect, replay db history then stream live; server emits one fake text event per second for any `running` session (hardcoded stand-in)
- [ ] `cli`: `ns2 session tail [--id|--name] [--turns]` connects to SSE and prints events to stdout

---

## Milestone 4 — Harness with stub responses

**What works:** `ns2 session new --message "hello"` transitions the session through `running → completed` and `ns2 session tail` shows a fake hardcoded assistant response. Full lifecycle, no real AI.

New crates: `harness`

- [ ] `harness`: `run(config)` spawns a tokio task; reads queued message; emits hardcoded `ContentBlock::Text` via broadcast channel; marks session `completed` in db
- [ ] `harness`: defines `AnthropicClient` trait (for mockability); stub impl returns a fixed response
- [ ] `server`: on `POST /sessions`, spawn harness task; wire broadcast sender into `AppState`; wire mpsc sender for user messages
- [ ] `server`: `POST /sessions/:id/messages` sends to harness mpsc queue
- [ ] `cli`: `ns2 session send [--id|--name] --message`

---

## Milestone 5 — Real Anthropic calls

**What works:** Full end-to-end. Claude responds with text and `ns2 session tail` streams the real output. No tools.

New crates: `anthropic`

- [ ] `anthropic`: `reqwest` HTTP client, SSE streaming parser, `BlockAssembler` for deltas → complete blocks
- [ ] `anthropic`: implements the `AnthropicClient` trait from `harness`
- [ ] `harness`: swap stub for real client; system prompt passed as a bare string (agent file loading deferred)
- [ ] `cli`: `ns2 session stop [--id|--name]`
- [ ] End-to-end smoke test: start server in-process, create session, await `completed`, assert non-empty text block in db; uses a mock HTTP server (wiremock) standing in for the Anthropic API — no real API calls in CI

---

## Out of scope

These are intentionally excluded from MVP scope.

- **TUI** (`tui` crate + `ns2 session attach`): ratatui thin client over the existing SSE route — the server needs no changes
- **Server auto-spawn**: CLI probes `GET /health` on launch; spawns server as background process + PID file if no response
