---
targets:
  - crates/cli/src/**/*.rs
verified: 2026-05-10T11:09:22Z
---

# CLI Architecture

The `cli` crate is the `ns2` binary. Its source is organized into four layers: an entry point, a
client module, a rendering module, and a commands directory. Each layer has a single,
well-defined responsibility and strict rules about what it may import.

## Entry point: `main.rs`

`main.rs` owns the top-level clap struct and all `#[derive(Subcommand)]` enums that define the
CLI surface. After parsing, it dispatches into the appropriate `commands::*` function and
returns. It also contains `load_dotenv` (reads `.env` before any other work) and the test
module for integration-style tests that exercise the full command tree via `Cli::try_parse_from`.

`main.rs` does not contain HTTP calls, output formatting, or business logic.

## HTTP client: `client.rs`

`client.rs` is the **only place in the crate where `reqwest` is used directly**. It contains:

- `handle_connection_error` — maps a `reqwest::Error` to a stderr message and exits.
- `print_error_response` — reads a non-2xx response body, prints the `error` field if present,
  and exits.
- `stream_events` — opens an SSE stream to a session events endpoint and prints each event by
  calling into `render::print_session_event`.
- `resolve_session_id` — resolves `--id` / `--name` flags to a UUID by optionally querying the
  sessions list endpoint.

No other module may call `reqwest` directly. If a command needs HTTP it must do so via the
helpers in this module or by constructing a `reqwest::Client` within `commands/*.rs` only for
simple request/response calls (POST/GET/PATCH with JSON bodies and no streaming).

## Output formatting: `render.rs`

`render.rs` is the **only place where output formatting logic lives**. It contains pure
functions that take domain types and return formatted strings or print to stdout/stderr. It does
not make HTTP calls and does not call `process::exit`.

Responsibilities:

- Issue table rows (`format_issue_row`, `print_issue_row`).
- Session event formatting (`format_session_event`, `print_session_event`).
- SSE frame parsing (`parse_sse_frames`).
- Spec sync error/warning messages (`format_sync_error`, `format_sync_warning`).
- Spinner helpers (`SPINNER_FRAMES`, `spinner_char`), string truncation (`truncate_str`).
- Issue tree rendering (`IssueTreeNode`, `issue_status_symbol`, `render_tree_line`,
  `render_issue_tree`).
- Session wait progress lines (`session_status_symbol`, `render_session_line`).

No other module may contain output formatting logic. If a command needs to print something it
must either call a function in this module or use a trivially simple `println!`/`eprintln!`
for a one-line confirmation message.

## Commands: `commands/`

Each file in `commands/` corresponds to one top-level CLI noun. Command functions call
`ServerClient` methods (or construct `reqwest::Client` inline for simple requests) and
`render::*` functions. They contain no raw formatting logic and no SSE streaming code.

| File | Noun | Responsibility |
|------|------|----------------|
| `agent.rs` | `ns2 agent` | `run_list`, `run_new`, `run_edit` — reads/writes `.ns2/agents/` via the `agents` crate. |
| `issue.rs` | `ns2 issue` | Full issue lifecycle: new, edit, comment, set-status, complete, reopen, list, wait, watch, subscribe. Owns `issue_is_terminal`, `all_nodes_terminal`, and `run_subscribe` (shared by both `issue subscribe` and the `--subscribe` flag on `issue new`). |
| `server.rs` | `ns2 server` | `data_dir_and_pid` (shared helper), `run_start` (delegates to the `server` crate), `run_stop` (reads PID file and signals the process). |
| `session.rs` | `ns2 session` | Full session lifecycle: list, new, tail, send, stop, wait. Owns `session_is_terminal`. |
| `spec.rs` | `ns2 spec` | `run_new`, `run_sync`, `run_verify`. Owns `verify_spec_paths` / `VerifyResult` (returned instead of calling `process::exit` so tests can assert on the result). |
| `worktree.rs` | `ns2 worktree` | `run_list`, `run_create`, `run_delete` — delegates to the `workspace` crate. |

## Dependency rules

```
main.rs
  └── commands/*   (dispatch only)
        ├── client   (HTTP helpers)
        └── render   (formatting helpers)
render
  └── types        (domain types — no HTTP, no formatting side-effects)
client
  ├── render       (print_session_event for SSE streaming)
  └── reqwest      (the only module allowed to use reqwest directly)
```

`render.rs` and `client.rs` do not depend on each other except that `client::stream_events`
calls `render::print_session_event` and `render::parse_sse_frames`.