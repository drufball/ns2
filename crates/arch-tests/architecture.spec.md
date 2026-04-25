---
targets:
  - Cargo.toml
  - crates/*/Cargo.toml
verified: 2026-04-25T11:19:49Z
---

# Architecture Spec

## Overview

The workspace is a flat set of crates, each owning one layer of the system. Dependencies are a directed, acyclic graph. Any crate should be able to be understood, tested, and replaced without reading the others.

**Deep modules:** each crate exposes a narrow, well-defined public interface and hides its implementation complexity.

**Do not add dependencies to a crate's `Cargo.toml` to work around an architectural boundary.** Adding a dependency to the wrong crate is an architectural violation, not a pragmatic shortcut. When in doubt, request input on how to proceed.

## Crates

**`types`** — shared domain types with no dependencies: `Session`, `Turn`, `ContentBlock`, `SessionStatus`, SSE event envelopes, tool input/output shapes. Everything else depends on this; it depends on nothing.

**`db`** — all SQLite access via sqlx. Owns the schema, migrations, and every query. Nothing outside this crate writes SQL. Exposes an async repository interface that the rest of the system talks to.

**`anthropic`** — HTTP client for the Anthropic Messages API. Owns streaming SSE parsing, assembles raw deltas into complete `ContentBlock`s, and surfaces a clean async interface. All knowledge of the wire format lives here.

**`tools`** — defines the `Tool` trait and implements the standard tools: `bash`, `read`, `write`, `edit`. Each tool is self-contained. The harness depends on this crate; tools have no knowledge of sessions or turns.

**`workspace`** — git worktree management. Creates a worktree for a branch on demand; worktrees persist until explicitly removed via CLI (typically after merge). Purely a git operations crate — no knowledge of sessions. Also exposes `git_root()` — a utility that returns the repository root, used by `cli` for locating config files and data directories. All git interactions belong here.

**`agents`** — agent definition files. Reads and writes `.ns2/agents/*.md` files, which define agent types via YAML frontmatter (`name`, `description`) and a system prompt body. Exposes a pure in-memory `AgentDef` type, parsing/formatting helpers, and directory-based list/load/write operations. Depends only on `workspace` for `git_root()` discovery. No knowledge of sessions, turns, or HTTP.

**`specs`** — spec file management. Reads and writes `.spec.md` files anywhere in the repo, which declare which source files a spec governs via YAML frontmatter (`targets`: glob patterns, `verified`: UTC timestamp). Exposes a pure in-memory `SpecDef` type, parsing/formatting helpers, recursive discovery (`list_specs`), and staleness checking (`stale_files`). Depends only on `workspace` for `git_root()` discovery. No knowledge of sessions, turns, or HTTP.

**`harness`** — the agent turn loop. Depends on `anthropic`, `tools`, `db`, and `agents`. Owns context window construction, system prompt loading (reads the agent definition for `session.agent` via the `agents` crate), and tool dispatch. Emits events to a tokio broadcast channel — it has no knowledge of HTTP or subscribers. One instance runs per active session as a tokio task.

**`server`** — axum HTTP server. Exposes routes for session management, issue management, and SSE streaming: creating session records, listing sessions, queuing user messages; creating, listing, editing, and completing issues; linking issues to agent sessions via `issue start`. Enforces branch-level concurrency: rejects new sessions on a branch that already has a `running` session (checked against `db` at creation time). Spawns harness tasks for new sessions but doesn't reach into the turn loop — the harness runs independently once started. Subscribes to harness broadcast channels and fans events out to SSE clients.

**`tui`** — ratatui terminal UI. Connects to the server via SSE and renders sessions. Thin client: all state comes from the server, nothing is computed locally.

**`cli`** — the `ns2` binary. Depends on `workspace` for git root discovery, `agents` for agent management commands, and `specs` for spec file commands. On launch, checks if a server is already running (via a PID file or a probe to localhost). If not, starts one in the background. Then launches the TUI connected to the orchestrator session. Wires crates together; contains no logic of its own.