# ns2

ns2 is a session-based agent orchestration tool built in Rust. It provides a local HTTP server that manages agent sessions and issues, a CLI for driving those primitives, and a streaming event bus so clients can watch what agents are doing in real time.

## Crate layout

The workspace is a flat collection of single-responsibility crates. See **[specs/architecture.spec.md](specs/architecture.spec.md)** for the full dependency graph and design rules. The short version:

| Crate | Responsibility |
|---|---|
| `types` | Shared domain types (`Session`, `Turn`, `Issue`, tool shapes). No behavior. |
| `db` | SQLite access via sqlx — schema, migrations, and every query. |
| `anthropic` | HTTP client for the Anthropic Messages API, including streaming SSE parsing. |
| `tools` | `Tool` trait plus `bash`, `read`, `write`, `edit` implementations. |
| `workspace` | Git worktree management and `git_root()` discovery. |
| `agents` | Reads/writes `.ns2/agents/*.md` agent definition files. |
| `specs` | Reads/writes `.spec.md` files; staleness checking. |
| `events` | Global event bus (`SystemEvent`, `SessionEvent`, `IssueEvent`). |
| `harness` | Agent turn loop — context window, system prompt, tool dispatch, worktree resolution. |
| `issues` | Issue domain service and state machine (`open → in_progress → completed/failed`). |
| `hooks` | Hook types, filter evaluation, action dispatch, and timer scheduling. |
| `server` | Axum HTTP server — routes, session maps, harness spawning. |
| `tui` | Ratatui terminal UI that connects to the server via SSE. |
| `cli` | The `ns2` binary. Wires crates; contains no logic of its own. |

## Build

```bash
# Prerequisites: Rust stable, sqlx-cli (optional, for manual migrations)
cargo build --release
```

The `ns2` binary lands at `target/release/ns2`. Add it to your `PATH` or run it via `cargo run --bin ns2 --`.

## Running the server

ns2 runs as a local server that the CLI talks to over HTTP (default port 9876).

```bash
export ANTHROPIC_API_KEY=sk-ant-...

# Start the server in a git repo (it needs a git root to find agent files)
cd /path/to/your/project
ns2 server start
```

The server stores all state in a SQLite database at `~/.ns2/ns2.db` and serves the REST + SSE API at `http://localhost:9876`.

## Usage example

### List registered agents

```bash
ns2 agent list
```

### Issue lifecycle

Create an issue and assign it to an agent:

```bash
# Create an issue (prints a short 4-character ID, e.g. "a1b2")
ISSUE=$(ns2 issue new --title "Add a greeting" \
                      --body "Create hello.txt containing 'Hello World'" \
                      --assignee swe)

# Inspect it
ns2 issue list --status open

# Start the agent session
ns2 issue edit --id "$ISSUE" --status in_progress

# Block until the agent finishes
ns2 issue wait --id "$ISSUE"

# Check the result
ns2 issue list --status completed

# Add a review comment and mark it done
ns2 issue comment --id "$ISSUE" --body "Looks good!" --author reviewer
ns2 issue complete --id "$ISSUE" --comment "Verified."
```

**Compact form** — create, start, and block in one command:

```bash
ISSUE=$(ns2 issue new --title "Add a greeting" \
                      --body "Create hello.txt containing 'Hello World'" \
                      --assignee swe \
                      --status in_progress \
                      --wait)
ns2 issue complete --id "$ISSUE" --comment "Done."
```

State machine: `open → in_progress → completed/failed`. `waiting` is a non-terminal pause state (agent yielded; resumes on next `in_progress`). Terminal issues can be moved back to `open` with `ns2 issue reopen`.

### Session lifecycle (direct)

```bash
# Create a one-shot session and stream the response
SESSION=$(ns2 session new --message "hello")
ns2 session tail --id "$SESSION"

# Or block inline until Claude replies
ns2 session new --message "hello" --wait
```

## Specs

The `specs/` directory has long-form detail on every subsystem:

- [Architecture & dependency rules](specs/architecture.spec.md)
- [CLI commands](specs/cli-commands.spec.md)
- [Issue lifecycle](specs/issue-lifecycle.spec.md)
- [Session lifecycle](specs/session-lifecycle.spec.md)
- [Server](specs/server.spec.md)
- [Agent harness](specs/harness.spec.md)
- [Data model](specs/data-model.spec.md)
