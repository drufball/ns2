---
targets:
  - crates/cli/src/**/*.rs
  - crates/cli/Cargo.toml
verified: 2026-04-26T16:18:47Z
---

# CLI Commands Spec

This spec serves as feature reference and `--help` specification. The code in `main.rs` should always match.

---

### `ns2 --help`

```
ns2 is an issue-driven agent orchestration tool.

Concepts:
  agent    a named system prompt stored in .ns2/agents/; defines how the model behaves
  issue    a work item assigned to an agent; the primary way to get work done
  session  internal implementation detail — created automatically when an issue starts

Typical workflow:
  ns2 server start
  ns2 agent list
  id=$(ns2 issue new --title "..." --body "..." --assignee swe)
  ns2 issue start --id "$id"
  ns2 issue wait --id "$id"
  ns2 issue complete --id "$id" --comment "Done"

Usage: ns2 [OPTIONS] <COMMAND>

Commands:
  server   Localhost server. Must be running for all commands.
  session  Inspect agent sessions (implementation detail — use `issue` to get work done).
  agent    Create and list agents to use in sessions.
  spec     Create design docs and verify they are in sync.
  issue    Track and manage work items.
  help     Print this message or the help of the given subcommand(s)

Options:
      --server <SERVER>
          Base URL of the ns2 server.
          
          [default: http://localhost:9876]

  -h, --help
          Print help (see a summary with '-h')
```

---

### `ns2 server --help`

```
Hosts session state and agent loops on localhost — must be running before any other commands work.

Usage: ns2 server <COMMAND>

Commands:
  start  Start the ns2 server.
  stop   Stop a running server.
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 server start --help`

```
Start the ns2 server.

Usage: ns2 server start [OPTIONS]

Options:
      --port <PORT>
          Port to listen on. Change this if the default port is occupied.
          
          [default: 9876]

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 server stop --help`

```
Stop a running server.

PID file: ~/.ns2/<repo-name>/server-<port>.pid (default: ~/.ns2/<repo-name>/server-9876.pid).

Usage: ns2 server stop

Options:
  -h, --help
          Print help (see a summary with '-h')
```

---

### `ns2 session --help`

```
Sessions are the internal agent runs that power issues. You typically don't create sessions directly — use `ns2 issue start` instead, which creates a session automatically.

Use session commands for inspection: tail output, list recent runs, or stop a runaway session.

Lifecycle:
  created    session exists but no message sent yet; agent not started
  running    agent is active and processing messages
  completed  agent finished successfully
  failed     agent ended with an error (check tail output for details)
  cancelled  stopped manually via session stop

Usage: ns2 session <COMMAND>

Commands:
  list  List recent sessions.
  new   Start a new agent session and print its ID to stdout.
  tail  Stream a session's output to stdout.
  send  Queue a message to a session.
  stop  Cancel a running or created session.
  wait  Block until all specified sessions reach a terminal state.
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 session list --help`

```
List recent sessions.

Output (one row per session, newest first):
  id                                    name                  status      created_at
  550e8400-e29b-41d4-a716-446655440000  mytask                running     2026-04-23 18:36:25 UTC

Use the id field with --id in tail, send, and stop.

Usage: ns2 session list [OPTIONS]

Options:
      --status <STATUS>
          Show only sessions in this state. Values: created, running, completed, failed, cancelled.

      --id <ID>
          Show only the session with this UUID. Cannot be combined with --status.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 session new --help`

```
Start a new agent session. Session ID is printed to stdout (suitable for capture via `$(...)`). Human-readable confirmation to stderr. If `--message` is provided, the agent starts immediately. Without `--message`, the session remains in `created` state — useful when you want to set up the session before sending the first message.

Usage: ns2 session new [OPTIONS]

Options:
      --name <NAME>
          Optional human-readable label. Sessions are always identifiable by UUID via --id (printed to stdout on creation).

      --agent <AGENT>
          Which agent type should run the session. Run `ns2 agent list` to see available types. If omitted, no system prompt is used.

      --message <MESSAGE>
          The opening task or instruction for the agent. If omitted, the session waits for your first `session send`.

      --wait
          Block until session reaches terminal state. Emits session id to stdout, then only the final turn's content. Exits 0 on completed, non-zero on failed/cancelled. Requires --message.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 session tail --help`

```
Stream a session's output to stdout. Blocks until the session finishes, then exits 0 on success or non-zero on error.

Output format:
  [turn <uuid>]          new agent turn starting
  <text>                 model's text response, streamed
  [tool: name(input)]    tool call
  [result: content]      tool result
  [done]                 session completed successfully
  [error] <message>      session failed (also to stderr; exits non-zero)

Requires --id or --name.

Usage: ns2 session tail [OPTIONS]

Options:
      --id <ID>
          Identify session by UUID (preferred). The UUID is printed to stdout by `session new`.

      --name <NAME>
          Identify session by name (alternative to --id).

      --turns <TURNS>
          Only replay the last N turns of history before streaming live. 0 skips all history.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 session send --help`

```
Queue a message to a session.

Use this to give follow-up instructions, provide additional context, or correct an agent that's going down an incorrect path. The message is queued immediately; the agent picks it up on its next turn. Messages can only be sent to sessions that are in the `created` or `running` state.

Usage: ns2 session send [OPTIONS] --message <MESSAGE>

Options:
      --id <ID>
          Identify session by UUID (preferred).

      --name <NAME>
          Identify session by name (alternative to --id).

      --message <MESSAGE>
          The message to queue. Required.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 session stop --help`

```
Cancel a running or created session.

Use this to abort a session that's stuck, heading in the wrong direction, or no longer needed. Has no effect on sessions that are already `completed` or `cancelled`.

Usage: ns2 session stop [OPTIONS]

Options:
      --id <ID>
          Identify session by UUID (preferred).

      --name <NAME>
          Identify session by name (alternative to --id).

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 session wait --help`

```
Polls the listed sessions every second and exits once all of them are in 'completed', 'failed', or 'cancelled' state.

Exits 0 if all sessions completed or were cancelled; exits 1 if any session failed or does not exist.

Usage: ns2 session wait [OPTIONS]

Options:
      --id <IDS>...
          Session IDs to wait on. Repeat for multiple.

  -h, --help
          Print help (see a summary with '-h')
```

---

### `ns2 agent --help`

```
Agents define how a session behaves. Each agent is a Markdown file in .ns2/agents/ with three fields:

  name         the identifier used in `session new --agent <name>`
  description  a one-line summary shown in `agent list`; helps you pick the right agent for a task
  body         the system prompt — sent to the model at the start of every session of this type

When a session starts, the agent's body is loaded as the system prompt before the first user message. An agent with an empty body runs with no system prompt.

Usage: ns2 agent <COMMAND>

Commands:
  list  List all available agent types.
  new   Create a new agent type.
  edit  Update an existing agent's description or system prompt.
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 agent list --help`

```
List all available agent types.

Shows the name and description of each agent from `.ns2/agents/`. Run this to find valid values for `--agent` in `session new`. No flags.

Usage: ns2 agent list

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 agent new --help`

```
Create a new agent type at `.ns2/agents/<name>.md`. Standard usage provides name, description, and body via flags.

Always pass `--body` when running non-interactively — without it the command opens `$EDITOR` and blocks until the editor exits.

Usage: ns2 agent new [OPTIONS]

Options:
      --name <NAME>
          The agent type name. Becomes the filename and the value you pass to `session new --agent`. Required.

      --description <DESCRIPTION>
          A one-line summary shown in `agent list`. Helps you pick the right agent for a task.

      --body <BODY>
          The system prompt body. Required for non-interactive use — omitting opens `$EDITOR` and blocks.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 agent edit --help`

```
Update an existing agent's description or system prompt.

Modifies the specified fields in place; fields you don't pass are unchanged. At least one of `--description` or `--body` must be provided.

Usage: ns2 agent edit [OPTIONS]

Options:
      --name <NAME>
          The agent type to edit. Required.

      --description <DESCRIPTION>
          Replace the frontmatter description.

      --body <BODY>
          Replace the system prompt body entirely.

  -h, --help
          Print help (see a summary with '-h')
```

---

### `ns2 spec --help`

```
Specs are Markdown files that describe the intended behavior of a part of the codebase and declare
which source files implement it. They serve two purposes: human-readable design documentation for
understanding and guiding the implementation, and a staleness check that fails when the code changes without the spec being reviewed.

Each spec file has YAML frontmatter:
  targets   glob patterns for the source files this spec governs (relative to git root)
  verified  timestamp of the last review; unset means the spec has never been verified
  severity  error (default) or warning — controls whether sync exits non-zero when stale

Lifecycle:
  unverified  spec was just created or targets have never been reviewed
  stale       one or more target files changed after the verified timestamp
  clean       all target files are older than the verified timestamp

Use `spec sync` in CI to enforce that specs are always kept in sync with the code they describe.

Usage: ns2 spec <COMMAND>

Commands:
  new     Create a new spec file.
  sync    Check whether spec targets have been modified since last verified.
  verify  Mark a spec as verified at the current time.
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 spec new --help`

```
Create a new spec file.

Initializes the file with the given targets and no `verified` timestamp. The body is left empty for you to fill in. Errors if the file already exists.

Usage: ns2 spec new [OPTIONS] <PATH>

Arguments:
  <PATH>
          Where to create the spec. Relative to git root.

Options:
      --target <TARGETS>...
          A glob pattern for files this spec covers. Repeat for multiple targets.

      --severity <SEVERITY>
          How stale detection is reported (default: error). Use `warning` for specs that document aspirational design.
          
          [default: error]

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 spec sync --help`

```
Check whether spec targets have been modified since the spec was last verified.

Prints an error for each stale spec and exits non-zero if any error-severity spec is stale. Exits 0 with no output if everything is clean.

Use this in CI to catch unreviewed drift, or before starting work to understand which specs are out of date.

Usage: ns2 spec sync [OPTIONS] [PATH]

Arguments:
  [PATH]
          A specific `.spec.md` file or directory to check. If omitted, checks all `.spec.md` files recursively from the git root.

Options:
      --error-on-warnings
          Treat `warning`-severity specs as errors. Use in CI when you want a strict check.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 spec verify --help`

```
Mark a spec as verified at the current time.

Writes the current UTC timestamp into the `verified` frontmatter field. Run this after reviewing or updating a spec's targets to confirm the spec is in sync with the code. The body and targets are preserved.

Usage: ns2 spec verify <PATH>

Arguments:
  <PATH>
          The spec file to verify. Required — you must verify specs one at a time.

Options:
  -h, --help
          Print help (see a summary with '-h')
```
---

### `ns2 issue --help`

```
Issues are lightweight work items with a title, body, optional assignee agent, and status lifecycle.

Lifecycle:
  open       issue created, not yet assigned to a session
  running    an agent session is actively working on this issue
  completed  work finished and reviewed
  failed     session ended with an error

Typical workflow:
  id=$(ns2 issue new --title "..." --body "..." --assignee swe)
  ns2 issue start --id "$id"
  ns2 issue wait --id "$id"
  ns2 issue complete --id "$id" --comment "Done: ..."

Use `issue list` to see current issues; use `issue wait` to block until issues finish.

Usage: ns2 issue <COMMAND>

Commands:
  new       Create a new issue.
  edit      Edit an existing issue.
  comment   Post a comment to an issue.
  start     Create an agent session for this issue and start it.
  complete  Mark an issue as completed.
  reopen    Move a failed or completed issue back to open.
  list      List issues.
  wait      Block until all specified issues reach a terminal state.
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 issue new --help`

```
Create a new issue. Prints the issue ID to stdout. Title and body are required.

Usage: ns2 issue new [OPTIONS] --title <TITLE> --body <BODY>

Options:
      --title <TITLE>
          Short description of the issue. Required.

      --body <BODY>
          Full issue body. Required.

      --assignee <ASSIGNEE>
          Agent type that should handle this issue (e.g. swe, qa-tester).

      --parent <PARENT>
          ID of the parent issue.

      --blocked-on <BLOCKED_ON>...
          Issue IDs that must be completed before this one. Repeat for multiple.

      --start
          Immediately start the issue after creating it. Requires --assignee.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 issue edit --help`

```
Edit fields of an existing issue. Only the flags you provide are changed.

Usage: ns2 issue edit [OPTIONS] --id <ID>

Options:
      --id <ID>
          The issue ID to edit. Required.

      --title <TITLE>
          New title.

      --body <BODY>
          New body.

      --assignee <ASSIGNEE>
          New assignee agent type. Pass empty string to clear.

      --parent <PARENT>
          New parent issue ID. Pass empty string to clear.

      --blocked-on [<BLOCKED_ON>...]
          Replace the blocked-on list. Pass with no value to clear.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 issue comment --help`

```
Post a comment to an issue.

Usage: ns2 issue comment [OPTIONS] --id <ID> --body <BODY>

Options:
      --id <ID>
          The issue ID. Required.

      --body <BODY>
          The comment body. Required.

      --author <AUTHOR>
          Author name (defaults to 'user').
          
          [default: user]

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 issue start --help`

```
Creates a new session using the issue's assignee agent, sends the issue title and body as the opening message, and links the session to the issue. Sets the issue status to 'running'.

Usage: ns2 issue start --id <ID>

Options:
      --id <ID>
          The issue ID. Required.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 issue complete --help`

```
Marks an issue completed and adds a final summary comment. The --comment flag is required.

Usage: ns2 issue complete --id <ID> --comment <COMMENT>

Options:
      --id <ID>
          The issue ID. Required.

      --comment <COMMENT>
          A final summary of what was done. Required.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 issue list --help`

```
List issues, newest first. Use flags to filter.

Output columns: id, title, status, assignee, created_at

Usage: ns2 issue list [OPTIONS]

Options:
      --status <STATUS>
          Show only issues in this status. Values: open, running, completed, failed.

      --assignee <ASSIGNEE>
          Show only issues assigned to this agent type.

      --parent <PARENT>
          Show only issues with this parent issue ID.

      --blocked-on <BLOCKED_ON>
          Show only issues blocked on this issue ID.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 issue wait --help`

```
Polls the listed issues every second and exits once all of them are in 'completed' or 'failed' state. Exits 0 if all completed; exits non-zero if any failed.

Usage: ns2 issue wait [OPTIONS]

Options:
      --id <IDS>...
          Issue IDs to wait on. Repeat for multiple.

  -h, --help
          Print help (see a summary with '-h')
```

### `ns2 issue reopen --help`

```
Moves a failed or completed issue back to open so work can resume on the same thread.
Preserves all existing comments.

Behavior differs by prior state:
  failed    → clears session_id so a fresh session is created on next start
  completed → keeps session_id so the existing session history is resumed on next start

Only failed or completed issues can be reopened. Attempting to reopen an issue in any
other state is an error.

Usage: ns2 issue reopen [OPTIONS] --id <ID>

Options:
      --id <ID>
          The issue ID. Required.

      --comment <COMMENT>
          Append a comment to the issue thread before transitioning back to open.
          Author is 'user'. Gives context to the agent when it resumes.

      --start
          Immediately start the issue after reopening it.

  -h, --help
          Print help (see a summary with '-h')
```