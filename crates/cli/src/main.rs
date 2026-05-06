use clap::{Parser, Subcommand};

mod client;
mod commands;
mod render;

#[derive(Parser)]
#[command(name = "ns2")]
#[command(about = "An issue-driven agent orchestration tool.")]
#[command(long_about = "ns2 is an issue-driven agent orchestration tool.\n\nConcepts:\n  agent    a named system prompt stored in .ns2/agents/; defines how the model behaves\n  issue    a work item assigned to an agent; the primary way to get work done\n  session  internal implementation detail — created automatically when an issue starts\n\nTypical workflow:\n  ns2 server start\n  ns2 agent list\n  id=$(ns2 issue new --title \"...\" --body \"...\" --assignee swe)\n  ns2 issue start --id \"$id\"\n  ns2 issue wait --id \"$id\"\n  ns2 issue complete --id \"$id\" --comment \"Done\"")]
struct Cli {
    #[arg(long, default_value = "http://localhost:9876", help = "Base URL of the ns2 server.")]
    server: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Localhost server. Must be running for all commands.", long_about = "Hosts session state and agent loops on localhost — must be running before any other commands work.")]
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    #[command(about = "Inspect agent sessions (implementation detail — use `issue` to get work done).", long_about = "Sessions are the internal agent runs that power issues. You typically don't create sessions directly — use `ns2 issue start` instead, which creates a session automatically.\n\nUse session commands for inspection: tail output, list recent runs, or stop a runaway session.\n\nLifecycle:\n  created    session exists but no message sent yet; agent not started\n  running    agent is active and processing messages\n  completed  agent finished successfully\n  failed     agent ended with an error (check tail output for details)\n  cancelled  stopped manually via session stop")]
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    #[command(about = "Create and list agents to use in sessions.", long_about = "Agents define how a session behaves. Each agent is a Markdown file in .ns2/agents/ with three fields:\n\n  name         the identifier used in `session new --agent <name>`\n  description  a one-line summary shown in `agent list`; helps you pick the right agent for a task\n  body         the system prompt — sent to the model at the start of every session of this type\n\nWhen a session starts, the agent's body is loaded as the system prompt before the first user message. An agent with an empty body runs with no system prompt.")]
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    #[command(about = "Create design docs and verify they are in sync.", long_about = "Specs are Markdown files that describe the intended behavior of a part of the codebase and declare\nwhich source files implement it. They serve two purposes: human-readable design documentation for\nunderstanding and guiding the implementation, and a staleness check that fails when the code changes\nwithout the spec being reviewed.\n\nEach spec file has YAML frontmatter:\n  targets   glob patterns for the source files this spec governs (relative to git root)\n  verified  timestamp of the last review; unset means the spec has never been verified\n  severity  error (default) or warning — controls whether sync exits non-zero when stale\n\nLifecycle:\n  unverified  spec was just created or targets have never been reviewed\n  stale       one or more target files changed after the verified timestamp\n  clean       all target files are older than the verified timestamp\n\nUse `spec sync` in CI to enforce that specs are always kept in sync with the code they describe.")]
    Spec {
        #[command(subcommand)]
        action: SpecAction,
    },
    #[command(about = "Track and manage work items.", long_about = "Issues are lightweight work items with a title, body, optional assignee agent, and status lifecycle.\n\nLifecycle:\n  open       issue created, not yet assigned to a session\n  running    an agent session is actively working on this issue\n  completed  work finished and reviewed\n  failed     session ended with an error\n\nTypical workflow:\n  id=$(ns2 issue new --title \"...\" --body \"...\" --assignee swe)\n  ns2 issue start --id \"$id\"\n  ns2 issue wait --id \"$id\"\n  ns2 issue complete --id \"$id\" --comment \"Done: ...\"\n\nUse `issue list` to see current issues; use `issue wait` to block until issues finish.")]
    Issue {
        #[command(subcommand)]
        action: IssueAction,
    },
    #[command(about = "Manage event-driven hooks.", long_about = "Hooks react to system events (issue status changes, session completions, etc.) and fire actions like sending comments to issues.\n\nLifecycle:\n  enabled    hook is active and will fire when events match\n  disabled   hook exists but will not fire\n\nTypical workflow:\n  WATCHER=$(ns2 issue new --title \"Watcher\" --body \"\")\n  ns2 hook new --name notify --source internal --event-type issue.status_changed \\\n    --action send-message --target \"issue:$WATCHER\" \\\n    --body \"Issue {{ event.data.issue.id }} is now {{ event.data.to }}\"\n  ns2 hook list\n  ns2 hook logs --id <hook-id>")]
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    #[command(about = "Manage git worktrees for branches.", long_about = "Worktrees let multiple branches be checked out simultaneously into separate directories.\nEach worktree maps a branch to a directory under the configured worktree base path.\n\nThe base path is read from ns2.toml ([worktrees] path = ...) or defaults to\n~/.ns2/<repo-name>/worktrees/.\n\nSubcommands:\n  list    Print all worktrees under the base path\n  create  Create a worktree for a branch (idempotent)\n  delete  Remove a worktree and its branch")]
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
}

#[derive(Subcommand)]
enum HookAction {
    #[command(about = "Create a new hook.", long_about = "Create a new hook that reacts to system events.\n\nExamples:\n  ns2 hook new --name notify --source internal --event-type issue.status_changed \\\n    --action send-message --target issue:<id> --body \"Status: {{ event.data.to }}\"\n\n  ns2 hook new --name alert --source internal --event-type issue.created \\\n    --action send-message --target issue:<watcher-id> --body \"New issue created\"")]
    New {
        #[arg(long, help = "Hook name. Required.")]
        name: String,
        #[arg(long, help = "Source type: internal, external, or timer.")]
        source: String,
        #[arg(long = "event-type", num_args = 0.., help = "Event type(s) to listen for (for internal source). Repeatable. Use '*' for all.")]
        event_types: Vec<String>,
        #[arg(long = "filter-field", num_args = 0.., help = "Field condition in 'field=value' form. Repeatable (AND'd).")]
        filter_fields: Vec<String>,
        #[arg(long, help = "Action type: send-message, create-issue, or run-shell.")]
        action: String,
        #[arg(long, help = "Message target for send-message: issue:<id> or session:<id>.")]
        target: Option<String>,
        #[arg(long, help = "Message body (minijinja template) for send-message. Template context: event.")]
        body: Option<String>,
        #[arg(long, help = "Issue title for create-issue action.")]
        title: Option<String>,
        #[arg(long, help = "Assignee for create-issue action.")]
        assignee: Option<String>,
    },
    #[command(about = "List hooks.")]
    List {
        #[arg(long, help = "Only show enabled hooks.")]
        enabled: bool,
        #[arg(long, help = "Filter by source type: internal, external, timer.")]
        source_type: Option<String>,
    },
    #[command(about = "Show details of a hook.")]
    Show {
        #[arg(long, help = "The hook ID. Required.")]
        id: String,
    },
    #[command(about = "Enable a hook.")]
    Enable {
        #[arg(long, help = "The hook ID. Required.")]
        id: String,
    },
    #[command(about = "Disable a hook without deleting it.")]
    Disable {
        #[arg(long, help = "The hook ID. Required.")]
        id: String,
    },
    #[command(about = "Delete a hook permanently.")]
    Delete {
        #[arg(long, help = "The hook ID. Required.")]
        id: String,
    },
    #[command(about = "Show execution logs for a hook.")]
    Logs {
        #[arg(long, help = "The hook ID. Required.")]
        id: String,
        #[arg(long, default_value_t = 20, help = "Maximum number of executions to show.")]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    #[command(about = "List all available agent types.", long_about = "List all available agent types.\n\nShows the name and description of each agent from `.ns2/agents/`. Run this to find valid values for `--agent` in `session new`. No flags.")]
    List,
    #[command(about = "Create a new agent type.", long_about = "Create a new agent type at `.ns2/agents/<name>.md`. Standard usage provides name, description, and body via flags.\n\nAlways pass `--body` when running non-interactively — without it the command opens `$EDITOR` and blocks until the editor exits.")]
    New {
        #[arg(long, help = "The agent type name. Becomes the filename and the value you pass to `session new --agent`. Required.")]
        name: Option<String>,
        #[arg(long, help = "A one-line summary shown in `agent list`. Helps you pick the right agent for a task.")]
        description: Option<String>,
        #[arg(long, help = "The system prompt body. Required for non-interactive use — omitting opens `$EDITOR` and blocks.")]
        body: Option<String>,
    },
    #[command(about = "Update an existing agent's description or system prompt.", long_about = "Update an existing agent's description or system prompt.\n\nModifies the specified fields in place; fields you don't pass are unchanged. At least one of `--description` or `--body` must be provided.")]
    Edit {
        #[arg(long, help = "The agent type to edit. Required.")]
        name: Option<String>,
        #[arg(long, help = "Replace the frontmatter description.")]
        description: Option<String>,
        #[arg(long, help = "Replace the system prompt body entirely.")]
        body: Option<String>,
    },
}

#[derive(Subcommand)]
enum ServerAction {
    #[command(about = "Start the ns2 server.")]
    Start {
        #[arg(long, default_value_t = 9876, help = "Port to listen on. Change this if the default port is occupied.")]
        port: u16,
    },
    #[command(about = "Stop a running server.", long_about = "Stop a running server.\n\nPID file: ~/.ns2/<repo-name>/server-<port>.pid (default: ~/.ns2/<repo-name>/server-9876.pid).")]
    Stop {
        #[arg(long, default_value_t = 9876, help = "Port the server is listening on. Must match the --port used at start.")]
        port: u16,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    #[command(about = "List recent sessions.", long_about = "List recent sessions.\n\nOutput (one row per session, newest first):\n  id                                    name                  status      created_at\n  550e8400-e29b-41d4-a716-446655440000  mytask                running     2026-04-23 18:36:25 UTC\n\nUse the id field with --id in tail, send, and stop.")]
    List {
        #[arg(long, help = "Show only sessions in this state. Values: created, running, completed, failed, cancelled.")]
        status: Option<String>,
        #[arg(long, conflicts_with = "status", help = "Show only the session with this UUID. Cannot be combined with --status.")]
        id: Option<String>,
    },
    #[command(about = "Start a new agent session and print its ID to stdout.", long_about = "Start a new agent session. Session ID is printed to stdout (suitable for capture via `$(...)`). Human-readable confirmation to stderr. If `--message` is provided, the agent starts immediately. Without `--message`, the session remains in `created` state — useful when you want to set up the session before sending the first message.")]
    New {
        #[arg(long, help = "Optional human-readable label. Sessions are always identifiable by UUID via --id (printed to stdout on creation).")]
        name: Option<String>,
        #[arg(long, help = "Which agent type should run the session. Run `ns2 agent list` to see available types. If omitted, no system prompt is used.")]
        agent: Option<String>,
        #[arg(long, help = "The opening task or instruction for the agent. If omitted, the session waits for your first `session send`.")]
        message: Option<String>,
        #[arg(long, requires = "message", help = "Block until session reaches terminal state. Emits session id to stdout, then only the final turn's content. Exits 0 on completed, non-zero on failed/cancelled. Requires --message.")]
        wait: bool,
    },
    #[command(about = "Stream a session's output to stdout.", long_about = "Stream a session's output to stdout. Blocks until the session finishes, then exits 0 on success or non-zero on error.\n\nOutput format:\n  [turn <uuid>]          new agent turn starting\n  <text>                 model's text response, streamed\n  [tool: name(input)]    tool call\n  [result: content]      tool result\n  [done]                 session completed successfully\n  [error] <message>      session failed (also to stderr; exits non-zero)\n\nRequires --id or --name.")]
    Tail {
        #[arg(long, help = "Identify session by UUID (preferred). The UUID is printed to stdout by `session new`.")]
        id: Option<String>,
        #[arg(long, help = "Identify session by name (alternative to --id).")]
        name: Option<String>,
        #[arg(long, help = "Only replay the last N turns of history before streaming live. 0 skips all history.")]
        turns: Option<usize>,
        #[arg(long, help = "Exit after N seconds even if the session has not finished. Exits 0 if session completed naturally, 1 if timeout fired.")]
        timeout: Option<u64>,
    },
    #[command(about = "Queue a message to a session.", long_about = "Queue a message to a session.\n\nUse this to give follow-up instructions, provide additional context, or correct an agent that's going down an incorrect path. The message is queued immediately; the agent picks it up on its next turn. Messages can only be sent to sessions that are in the `created` or `running` state.")]
    Send {
        #[arg(long, help = "Identify session by UUID (preferred).")]
        id: Option<String>,
        #[arg(long, help = "Identify session by name (alternative to --id).")]
        name: Option<String>,
        #[arg(long, help = "The message to queue. Required.")]
        message: String,
    },
    #[command(about = "Cancel a running or created session.", long_about = "Cancel a running or created session.\n\nUse this to abort a session that's stuck, heading in the wrong direction, or no longer needed. Has no effect on sessions that are already `completed` or `cancelled`.")]
    Stop {
        #[arg(long, help = "Identify session by UUID (preferred).")]
        id: Option<String>,
        #[arg(long, help = "Identify session by name (alternative to --id).")]
        name: Option<String>,
    },
    #[command(about = "Block until all specified sessions reach a terminal state.", long_about = "Polls the listed sessions every second and exits once all of them are in 'completed', 'failed', or 'cancelled' state.\n\nExits 0 if all sessions completed or were cancelled; exits 1 if any session failed or does not exist.")]
    Wait {
        #[arg(long = "id", num_args = 1.., help = "Session UUIDs to wait on. Repeat for multiple.")]
        ids: Vec<String>,
        #[arg(long, help = "Exit after N seconds even if sessions have not finished. Exits 1 if timeout fired before completion.")]
        timeout: Option<u64>,
    },
}

#[derive(Subcommand)]
enum SpecAction {
    #[command(about = "Create a new spec file.", long_about = "Create a new spec file.\n\nInitializes the file with the given targets and no `verified` timestamp (so it shows as immediately stale). The body is left empty for you to fill in. Errors if the file already exists.")]
    New {
        #[arg(help = "Where to create the spec (e.g. `crates/myfeature/design.spec.md`). Relative to git root.")]
        path: String,
        #[arg(long = "target", num_args = 1.., help = "A glob pattern for files this spec covers. Repeat for multiple targets.")]
        targets: Vec<String>,
        #[arg(long, default_value = "error", help = "How stale detection is reported. Use `warning` for specs that document aspirational design.")]
        severity: String,
    },
    #[command(about = "Check whether spec targets have been modified since last verified.", long_about = "Check whether spec targets have been modified since the spec was last verified.\n\nPrints an error for each stale spec and exits non-zero if any error-severity spec is stale. Exits 0 with no output if everything is clean.\n\nUse this in CI to catch unreviewed drift, or before starting work to understand which specs are out of date.")]
    Sync {
        #[arg(help = "A specific `.spec.md` file or directory to check. If omitted, checks all `.spec.md` files recursively from the git root.")]
        path: Option<String>,
        #[arg(long, help = "Treat `warning`-severity specs as errors. Use in CI when you want a strict check.")]
        error_on_warnings: bool,
    },
    #[command(about = "Mark one or more specs as verified at the current time.", long_about = "Mark one or more specs as verified at the current time.\n\nWrites the current UTC timestamp into the `verified` frontmatter field of each spec. Run this after reviewing or updating a spec's targets to confirm the spec is in sync with the code. The body and targets are preserved.\n\nPass multiple paths to verify them all in one invocation. If any path fails, the others are still processed and the command exits 1.")]
    Verify {
        #[arg(
            required = true,
            help = "One or more spec files to verify. Pass multiple paths to verify them all in one invocation."
        )]
        paths: Vec<String>,
    },
}

#[derive(Subcommand)]
enum IssueAction {
    #[command(about = "Create a new issue.", long_about = "Create a new issue. Prints the issue ID to stdout. Title and body are required.")]
    New {
        #[arg(long, help = "Short description of the issue. Required.")]
        title: String,
        #[arg(long, help = "Full issue body. Required.")]
        body: String,
        #[arg(long, help = "Agent type that should handle this issue (e.g. swe, qa-tester).")]
        assignee: Option<String>,
        #[arg(long, help = "ID of the parent issue.")]
        parent: Option<String>,
        #[arg(long = "blocked-on", num_args = 1.., help = "Issue IDs that must be completed before this one. Repeat for multiple.")]
        blocked_on: Vec<String>,
        #[arg(long, help = "Immediately start the issue after creating it. Requires --assignee.")]
        start: bool,
        #[arg(long, help = "Git branch name to associate with this issue. Auto-generated from title if omitted.")]
        branch: Option<String>,
    },
    #[command(about = "Edit an existing issue.", long_about = "Edit fields of an existing issue. Only the flags you provide are changed.")]
    Edit {
        #[arg(long, help = "The issue ID to edit. Required.")]
        id: String,
        #[arg(long, help = "New title.")]
        title: Option<String>,
        #[arg(long, help = "New body.")]
        body: Option<String>,
        #[arg(long, help = "New assignee agent type. Pass empty string to clear.")]
        assignee: Option<String>,
        #[arg(long, help = "New parent issue ID. Pass empty string to clear.")]
        parent: Option<String>,
        #[arg(long = "blocked-on", num_args = 0.., help = "Replace the blocked-on list. Pass with no value to clear.")]
        blocked_on: Option<Vec<String>>,
        #[arg(long, help = "New git branch name for this issue.")]
        branch: Option<String>,
    },
    #[command(about = "Post a comment to an issue.")]
    Comment {
        #[arg(long, help = "The issue ID. Required.")]
        id: String,
        #[arg(long, help = "The comment body. Required.")]
        body: String,
        #[arg(long, default_value = "user", help = "Author name (defaults to 'user').")]
        author: String,
    },
    #[command(about = "Create an agent session for this issue and start it.", long_about = "Creates a new session using the issue's assignee agent, sends the issue title and body as the opening message, and links the session to the issue. Sets the issue status to 'running'.\n\nPrints a confirmation to stderr including the session UUID:\n  Started issue <id>. Session: <uuid>\n\nCapture the session UUID for later tailing:\n  session=$(ns2 issue start --id \"$id\" 2>&1 | awk '/Session:/{print $NF}')")]
    Start {
        #[arg(long, help = "The issue ID. Required.")]
        id: String,
    },
    #[command(about = "Mark an issue as completed.", long_about = "Marks an issue completed and adds a final summary comment. The --comment flag is required.")]
    Complete {
        #[arg(long, help = "The issue ID. Required.")]
        id: String,
        #[arg(long, help = "A final summary of what was done. Required.")]
        comment: String,
    },
    #[command(about = "Move a failed or completed issue back to open.", long_about = "Moves a failed or completed issue back to open so work can resume on the same thread. Preserves all existing comments.\n\n- failed → reopen → clears the session_id link so a fresh session will be created on next start.\n- completed → reopen → keeps the session_id so the existing session history is resumed on next start.\n\nOnly failed or completed issues can be reopened. Attempting to reopen an issue in any other state is an error.")]
    Reopen {
        #[arg(long, help = "The issue ID. Required.")]
        id: String,
        #[arg(long, help = "Append a comment to the issue thread before transitioning back to open. Author is 'user'.")]
        comment: Option<String>,
        #[arg(long, help = "Immediately start the issue after reopening it.")]
        start: bool,
    },
    #[command(about = "List issues.", long_about = "List issues, newest first. Use flags to filter.\n\nOutput columns: id, title, status, assignee, created_at")]
    List {
        #[arg(long, help = "Show only issues in this status. Values: open, running, completed, failed.")]
        status: Option<String>,
        #[arg(long, help = "Show only issues assigned to this agent type.")]
        assignee: Option<String>,
        #[arg(long, help = "Show only issues with this parent issue ID.")]
        parent: Option<String>,
        #[arg(long = "blocked-on", help = "Show only issues blocked on this issue ID.")]
        blocked_on: Option<String>,
    },
    #[command(about = "Block until all specified issues reach a terminal state.", long_about = "Polls the listed issues every second and exits once all of them are in 'completed' or 'failed' state. Exits 0 if all completed; exits non-zero if any failed.")]
    Wait {
        #[arg(long = "id", num_args = 1.., help = "Issue IDs to wait on. Repeat for multiple.")]
        ids: Vec<String>,
        #[arg(long, help = "Exit after N seconds even if issues have not finished. Exits 1 if timeout fired before completion.")]
        timeout: Option<u64>,
    },
    #[command(about = "Cancel a running or open issue.", long_about = "Cancel a running or open issue.\n\nSends a cancellation signal to the session associated with the issue, transitions the issue to 'cancelled' status, and terminates the harness loop cleanly.\n\nOnly open or running issues can be cancelled. Use `issue reopen` to resume work on a cancelled issue.")]
    Cancel {
        #[arg(long, help = "The issue ID. Required.")]
        id: String,
    },
    #[command(about = "Show details of a single issue.", long_about = "Print full details of a single issue: title, body, status, assignee, branch, and comments.\n\nWith --json, output is machine-readable JSON suitable for scripting:\n  STATUS=$(ns2 issue show --id \"$id\" --json | jq -r .status)")]
    Show {
        #[arg(long, help = "The issue ID. Required.")]
        id: String,
        #[arg(long, help = "Output as JSON instead of human-readable format.")]
        json: bool,
    },
    #[command(about = "Stream live events for an issue to stdout.", long_about = "Connects to the server's SSE event stream filtered to issue <id> and prints each event as it arrives.\n\nOutput format (one line per event):\n  [created]        open  – <title>\n  [status_changed] open → running\n  [comment_added]  <author>: \"<body>\"\n\nExits on Ctrl-C or when the server closes the stream.")]
    Watch {
        #[arg(long, help = "The issue ID to watch. Required.")]
        id: String,
    },
    #[command(about = "Subscribe to issue events and deliver notifications.", long_about = "Creates an internal hook that posts a comment notification to <deliver-to> whenever issue <id> has a status change or a new comment.\n\nThis is sugar for `ns2 hook new` with:\n  --source internal\n  --event-type issue.status_changed\n  --event-type issue.comment_added\n  --filter-field data.issue.id=<id>\n  --action send-message\n\nPrints the created hook ID to stdout.")]
    Subscribe {
        #[arg(long, help = "The issue ID to subscribe to. Required.")]
        id: String,
        #[arg(long = "deliver-to", help = "Notification target in the form 'issue:<id>' or 'session:<id>'. Required.")]
        deliver_to: String,
    },
}

#[derive(Subcommand)]
enum WorktreeAction {
    #[command(about = "List worktrees under the configured base path.", long_about = "List all git worktrees whose path is under the ns2 worktree base directory.\n\nPrints a table with columns: branch, path.\nPrints 'No worktrees found.' if none exist.")]
    List,
    #[command(about = "Create a worktree for a branch.", long_about = "Create a git worktree for the given branch under the worktree base directory.\n\nIdempotent: exits 0 without error if the worktree already exists.\n\nThe branch is created tracking origin/main if it does not yet exist locally.")]
    Create {
        #[arg(long, help = "The branch name to create a worktree for. Required.")]
        branch: String,
    },
    #[command(about = "Delete a worktree and its branch.", long_about = "Remove the worktree directory for a branch and delete the local branch.\n\nRequires the branch to be merged into main unless --force is passed.\nErrors with a clear message if no worktree exists for the given branch.")]
    Delete {
        #[arg(long, help = "The branch name whose worktree to delete. Required.")]
        branch: String,
        #[arg(long, help = "Delete even if the branch has unmerged commits.")]
        force: bool,
    },
}

// ────────────────────────────────────────────────────────────────────────────

fn load_dotenv() {
    let Some(root) = workspace::git_root_sync() else { return };
    let Ok(contents) = std::fs::read_to_string(root.join(".env")) else { return };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'');
            if std::env::var(key).is_err() {
                std::env::set_var(key, val);
            }
        }
    }
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() {
    load_dotenv();
    let cli = Cli::parse();

    match cli.command {
        Command::Server { action } => match action {
            ServerAction::Start { port } => {
                commands::server::run_start(port).await;
            }
            ServerAction::Stop { port } => {
                commands::server::run_stop(port);
            }
        },
        Command::Session { action } => match action {
            SessionAction::List { status, id } => {
                commands::session::run_list(&cli.server, status, id).await;
            }
            SessionAction::New { name, agent, message, wait } => {
                commands::session::run_new(&cli.server, name, agent, message, wait).await;
            }
            SessionAction::Tail { id, name, turns, timeout } => {
                commands::session::run_tail(&cli.server, id, name, turns, timeout).await;
            }
            SessionAction::Send { id, name, message } => {
                commands::session::run_send(&cli.server, id, name, message).await;
            }
            SessionAction::Stop { id, name } => {
                commands::session::run_stop(&cli.server, id, name).await;
            }
            SessionAction::Wait { ids, timeout } => {
                commands::session::run_wait(&cli.server, ids, timeout).await;
            }
        },
        Command::Agent { action } => match action {
            AgentAction::List => {
                commands::agent::run_list();
            }
            AgentAction::New { name, description, body } => {
                commands::agent::run_new(name, description, body);
            }
            AgentAction::Edit { name, description, body } => {
                commands::agent::run_edit(name, description, body);
            }
        },
        Command::Spec { action } => match action {
            SpecAction::New { path, targets, severity } => {
                commands::spec::run_new(path, targets, &severity);
            }
            SpecAction::Sync { path, error_on_warnings } => {
                commands::spec::run_sync(path, error_on_warnings);
            }
            SpecAction::Verify { paths } => {
                commands::spec::run_verify(&paths);
            }
        },
        Command::Issue { action } => match action {
            IssueAction::New { title, body, assignee, parent, blocked_on, start, branch } => {
                commands::issue::run_new(&cli.server, title, body, assignee, parent, blocked_on, start, branch).await;
            }
            IssueAction::Edit { id, title, body, assignee, parent, blocked_on, branch } => {
                commands::issue::run_edit(&cli.server, id, title, body, assignee, parent, blocked_on, branch).await;
            }
            IssueAction::Comment { id, body, author } => {
                commands::issue::run_comment(&cli.server, id, body, author).await;
            }
            IssueAction::Start { id } => {
                commands::issue::run_start(&cli.server, id).await;
            }
            IssueAction::Complete { id, comment } => {
                commands::issue::run_complete(&cli.server, id, comment).await;
            }
            IssueAction::Reopen { id, comment, start } => {
                commands::issue::run_reopen(&cli.server, id, comment, start).await;
            }
            IssueAction::List { status, assignee, parent, blocked_on } => {
                commands::issue::run_list(&cli.server, status, assignee, parent, blocked_on).await;
            }
            IssueAction::Wait { ids, timeout } => {
                commands::issue::run_wait(&cli.server, ids, timeout).await;
            }
            IssueAction::Show { id, json } => {
                commands::issue::run_show(&cli.server, id, json).await;
            }
            IssueAction::Cancel { id } => {
                commands::issue::run_cancel(&cli.server, id).await;
            }
            IssueAction::Watch { id } => {
                commands::issue::run_watch(&cli.server, id).await;
            }
            IssueAction::Subscribe { id, deliver_to } => {
                commands::issue::run_subscribe(&cli.server, id, deliver_to).await;
            }
        },
        Command::Hook { action } => match action {
            HookAction::New { name, source, event_types, filter_fields, action, target, body, title, assignee } => {
                commands::hook::run_new(&cli.server, name, source, event_types, filter_fields, action, target, body, title, assignee).await;
            }
            HookAction::List { enabled, source_type } => {
                commands::hook::run_list(&cli.server, enabled, source_type).await;
            }
            HookAction::Show { id } => {
                commands::hook::run_show(&cli.server, id).await;
            }
            HookAction::Enable { id } => {
                commands::hook::run_enable(&cli.server, id).await;
            }
            HookAction::Disable { id } => {
                commands::hook::run_disable(&cli.server, id).await;
            }
            HookAction::Delete { id } => {
                commands::hook::run_delete(&cli.server, id).await;
            }
            HookAction::Logs { id, limit } => {
                commands::hook::run_logs(&cli.server, id, limit).await;
            }
        },
        Command::Worktree { action } => match action {
            WorktreeAction::List => {
                commands::worktree::run_list().await;
            }
            WorktreeAction::Create { branch } => {
                commands::worktree::run_create(branch).await;
            }
            WorktreeAction::Delete { branch, force } => {
                commands::worktree::run_delete(branch, force).await;
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::*;
    use events::SessionEvent;
    use uuid::Uuid;
    use crate::render::{
        format_issue_row, format_issue_show, format_session_event, format_sync_error,
        format_sync_warning, issue_status_symbol, parse_sse_frames, render_issue_tree,
        render_session_line, render_tree_line, session_status_symbol, spinner_char, truncate_str,
        IssueTreeNode, SPINNER_FRAMES,
    };
    use crate::commands::spec::verify_spec_paths;
    use crate::commands::issue::{issue_is_terminal, all_nodes_terminal};
    use crate::commands::session::session_is_terminal;
    use crate::commands::server::data_dir_and_pid;

    #[test]
    fn issue_is_terminal_completed_is_true() {
        assert!(issue_is_terminal(&types::IssueStatus::Completed));
    }

    #[test]
    fn issue_is_terminal_failed_is_true() {
        assert!(issue_is_terminal(&types::IssueStatus::Failed));
    }

    #[test]
    fn issue_is_terminal_cancelled_is_true() {
        assert!(issue_is_terminal(&types::IssueStatus::Cancelled));
    }

    #[test]
    fn issue_is_terminal_open_is_false() {
        assert!(!issue_is_terminal(&types::IssueStatus::Open));
    }

    #[test]
    fn issue_is_terminal_running_is_false() {
        assert!(!issue_is_terminal(&types::IssueStatus::Running));
    }

    fn make_turn() -> Turn {
        Turn {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            token_count: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_tool_use_renders_name_and_input() {
        let turn = make_turn();
        let event = SessionEvent::ContentBlockDone {
            turn_id: turn.id,
            index: 0,
            block: ContentBlock::ToolUse {
                id: "abc".into(),
                name: "read".into(),
                input: serde_json::json!({"path": "/tmp/foo.txt"}),
            },
        };
        let out = format_session_event(&event).unwrap();
        assert!(out.contains("read"), "should contain tool name");
        assert!(out.contains("/tmp/foo.txt"), "should contain input");
    }

    #[test]
    fn test_tool_result_renders_content() {
        let turn = make_turn();
        let event = SessionEvent::ContentBlockDone {
            turn_id: turn.id,
            index: 1,
            block: ContentBlock::ToolResult {
                tool_use_id: "abc".into(),
                content: "file contents here".into(),
            },
        };
        let out = format_session_event(&event).unwrap();
        assert!(out.contains("file contents here"), "should contain result content");
    }

    #[test]
    fn test_input_json_delta_is_silent() {
        let event = SessionEvent::ContentBlockDelta {
            turn_id: Uuid::new_v4(),
            index: 0,
            delta: ContentBlockDelta::InputJsonDelta { partial_json: "{\"path\":".into() },
        };
        assert!(format_session_event(&event).is_none());
    }

    #[test]
    fn test_error_event_produces_output() {
        let event = SessionEvent::Error { message: "something went wrong".into() };
        let out = format_session_event(&event).unwrap();
        assert!(out.contains("something went wrong"));
    }

    #[test]
    fn parse_sse_frames_one_complete_frame() {
        let mut buf = String::new();
        let frames = parse_sse_frames(&mut buf, "data: hello\n\n");
        assert_eq!(frames, vec!["data: hello"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn parse_sse_frames_two_frames_concatenated() {
        let mut buf = String::new();
        let frames = parse_sse_frames(&mut buf, "data: first\n\ndata: second\n\n");
        assert_eq!(frames, vec!["data: first", "data: second"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn parse_sse_frames_partial_frame_stays_in_buffer() {
        let mut buf = String::new();
        let frames = parse_sse_frames(&mut buf, "data: partial");
        assert!(frames.is_empty());
        assert_eq!(buf, "data: partial");
    }

    #[test]
    fn parse_sse_frames_split_across_two_calls() {
        let mut buf = String::new();
        let frames1 = parse_sse_frames(&mut buf, "data: hel");
        assert!(frames1.is_empty());
        let frames2 = parse_sse_frames(&mut buf, "lo\n\n");
        assert_eq!(frames2, vec!["data: hello"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn parse_sse_frames_delimiter_not_consumed_into_next_frame() {
        let mut buf = String::new();
        let frames = parse_sse_frames(&mut buf, "data: a\n\ndata: b\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], "data: a");
        assert_eq!(frames[1], "data: b");
        assert!(buf.is_empty());
    }

    // Spec command parsing and behavior tests
    // These test functions and helpers that handle spec logic.
    // The actual CLI wiring (clap parse) is tested by integration tests / smoke tests.

    #[test]
    fn format_sync_error_includes_spec_and_files() {
        let stale = vec![
            std::path::PathBuf::from("crates/cli/src/main.rs"),
            std::path::PathBuf::from("crates/agents/src/lib.rs"),
        ];
        let out = format_sync_error("crates/cli/cli-commands.spec.md", &stale);
        assert!(out.contains("[error]"));
        assert!(out.contains("crates/cli/cli-commands.spec.md"));
        assert!(out.contains("crates/cli/src/main.rs"));
        assert!(out.contains("crates/agents/src/lib.rs"));
        // Each file is on its own indented line
        assert!(out.contains("  crates/cli/src/main.rs\n"));
        assert!(out.contains("  crates/agents/src/lib.rs\n"));
    }

    #[test]
    fn spec_sync_output_contains_spec_path_and_stale_file() {
        // This tests the formatting logic for the error output of `ns2 spec sync`.
        // We construct the message manually to verify the format.
        let spec_path = "crates/cli/cli-commands.spec.md";
        let stale = vec![std::path::PathBuf::from("crates/cli/src/main.rs")];
        let output = format_sync_error(spec_path, &stale);
        assert!(output.contains(spec_path), "output must include spec path");
        assert!(output.contains("crates/cli/src/main.rs"), "output must include stale file");
    }

    #[test]
    fn format_sync_warning_includes_warning_prefix() {
        let stale = vec![std::path::PathBuf::from("crates/cli/src/main.rs")];
        let out = format_sync_warning("crates/foo.spec.md", &stale);
        assert!(out.contains("[warning]"));
        assert!(out.contains("crates/foo.spec.md"));
        assert!(out.contains("crates/cli/src/main.rs"));
    }

    #[test]
    fn format_sync_error_includes_error_prefix() {
        let stale = vec![std::path::PathBuf::from("crates/cli/src/main.rs")];
        let out = format_sync_error("crates/foo.spec.md", &stale);
        assert!(out.contains("[error]"));
    }

    #[test]
    fn data_dir_and_pid_contains_port_and_repo_name() {
        let port: u16 = 19876;
        let tmp = std::env::temp_dir().join("ns2-test-home");
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("HOME", tmp.to_str().unwrap());

        let (data_dir, pid_file) = data_dir_and_pid(port);

        assert!(
            data_dir.to_string_lossy().contains(".ns2"),
            "data_dir should contain .ns2"
        );
        assert!(
            pid_file.to_string_lossy().contains("19876"),
            "pid_file should contain the port number"
        );
        assert!(
            pid_file.to_string_lossy().ends_with(".pid"),
            "pid_file should end with .pid"
        );
    }

    // --- CLI flag parsing tests ---

    use clap::Parser;

    #[test]
    fn session_tail_parses_timeout_flag() {
        let cli = Cli::try_parse_from(["ns2", "session", "tail", "--id", "abc", "--timeout", "10"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Tail { timeout, .. } } => {
                assert_eq!(timeout, Some(10));
            }
            _ => panic!("expected session tail command"),
        }
    }

    #[test]
    fn session_tail_no_timeout_is_none() {
        let cli = Cli::try_parse_from(["ns2", "session", "tail", "--id", "abc"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Tail { timeout, .. } } => {
                assert_eq!(timeout, None);
            }
            _ => panic!("expected session tail command"),
        }
    }

    #[test]
    fn session_wait_parses_timeout_flag() {
        let cli = Cli::try_parse_from(["ns2", "session", "wait", "--id", "abc", "--timeout", "30"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Wait { timeout, .. } } => {
                assert_eq!(timeout, Some(30));
            }
            _ => panic!("expected session wait command"),
        }
    }

    #[test]
    fn session_wait_no_timeout_is_none() {
        let cli = Cli::try_parse_from(["ns2", "session", "wait", "--id", "abc"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Wait { timeout, .. } } => {
                assert_eq!(timeout, None);
            }
            _ => panic!("expected session wait command"),
        }
    }

    #[test]
    fn issue_wait_parses_timeout_flag() {
        let cli = Cli::try_parse_from(["ns2", "issue", "wait", "--id", "abc", "--timeout", "60"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Wait { timeout, .. } } => {
                assert_eq!(timeout, Some(60));
            }
            _ => panic!("expected issue wait command"),
        }
    }

    #[test]
    fn issue_wait_no_timeout_is_none() {
        let cli = Cli::try_parse_from(["ns2", "issue", "wait", "--id", "abc"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Wait { timeout, .. } } => {
                assert_eq!(timeout, None);
            }
            _ => panic!("expected issue wait command"),
        }
    }

    #[test]
    fn session_tail_parses_turns_flag() {
        let cli = Cli::try_parse_from(["ns2", "session", "tail", "--id", "abc", "--turns", "5"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Tail { turns, .. } } => {
                assert_eq!(turns, Some(5));
            }
            _ => panic!("expected session tail command"),
        }
    }

    #[test]
    fn session_tail_turns_zero_is_valid() {
        let cli = Cli::try_parse_from(["ns2", "session", "tail", "--id", "abc", "--turns", "0"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Tail { turns, .. } } => {
                assert_eq!(turns, Some(0));
            }
            _ => panic!("expected session tail command"),
        }
    }

    #[test]
    fn session_tail_no_turns_flag_is_none() {
        let cli = Cli::try_parse_from(["ns2", "session", "tail", "--id", "abc"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Tail { turns, .. } } => {
                assert_eq!(turns, None);
            }
            _ => panic!("expected session tail command"),
        }
    }

    #[test]
    fn session_new_parses_wait_flag() {
        let cli = Cli::try_parse_from(["ns2", "session", "new", "--message", "do the thing", "--wait"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::New { wait, .. } } => {
                assert!(wait);
            }
            _ => panic!("expected session new command"),
        }
    }

    #[test]
    fn session_new_no_wait_flag_is_false() {
        let cli = Cli::try_parse_from(["ns2", "session", "new"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::New { wait, .. } } => {
                assert!(!wait);
            }
            _ => panic!("expected session new command"),
        }
    }

    #[test]
    fn session_list_parses_id_flag() {
        let cli = Cli::try_parse_from(["ns2", "session", "list", "--id", "550e8400-e29b-41d4-a716-446655440000"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::List { id, .. } } => {
                assert_eq!(id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
            }
            _ => panic!("expected session list command"),
        }
    }

    #[test]
    fn session_list_no_id_flag_is_none() {
        let cli = Cli::try_parse_from(["ns2", "session", "list"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::List { id, .. } } => {
                assert!(id.is_none());
            }
            _ => panic!("expected session list command"),
        }
    }

    // --- session_is_terminal tests ---

    #[test]
    fn session_is_terminal_completed_is_true() {
        assert!(session_is_terminal(&types::SessionStatus::Completed));
    }

    #[test]
    fn session_is_terminal_failed_is_true() {
        assert!(session_is_terminal(&types::SessionStatus::Failed));
    }

    #[test]
    fn session_is_terminal_cancelled_is_true() {
        assert!(session_is_terminal(&types::SessionStatus::Cancelled));
    }

    #[test]
    fn session_is_terminal_created_is_false() {
        assert!(!session_is_terminal(&types::SessionStatus::Created));
    }

    #[test]
    fn session_is_terminal_running_is_false() {
        assert!(!session_is_terminal(&types::SessionStatus::Running));
    }

    // --- session wait clap parse test ---

    #[test]
    fn session_wait_parses_multiple_ids() {
        let uuid1 = "550e8400-e29b-41d4-a716-446655440000";
        let uuid2 = "660f9511-f3ac-52e5-b827-557766551111";
        let cli = Cli::try_parse_from([
            "ns2", "session", "wait",
            "--id", uuid1,
            "--id", uuid2,
        ])
        .unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Wait { ids, .. } } => {
                assert_eq!(ids.len(), 2);
                assert!(ids.iter().any(|id| id == uuid1));
                assert!(ids.iter().any(|id| id == uuid2));
            }
            _ => panic!("expected session wait command"),
        }
    }

    // --- issue reopen CLI parse tests ---

    #[test]
    fn issue_reopen_parses_id_flag() {
        let cli = Cli::try_parse_from(["ns2", "issue", "reopen", "--id", "ab12"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Reopen { id, .. } } => {
                assert_eq!(id, "ab12");
            }
            _ => panic!("expected issue reopen command"),
        }
    }

    #[test]
    fn issue_reopen_missing_id_fails_to_parse() {
        let result = Cli::try_parse_from(["ns2", "issue", "reopen"]);
        assert!(result.is_err(), "reopen without --id should fail to parse");
    }

    #[test]
    fn issue_reopen_parses_comment_flag() {
        let cli = Cli::try_parse_from([
            "ns2", "issue", "reopen", "--id", "ab12", "--comment", "fix the test",
        ])
        .unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Reopen { id, comment, .. } } => {
                assert_eq!(id, "ab12");
                assert_eq!(comment.as_deref(), Some("fix the test"));
            }
            _ => panic!("expected issue reopen command"),
        }
    }

    #[test]
    fn issue_reopen_no_comment_is_none() {
        let cli = Cli::try_parse_from(["ns2", "issue", "reopen", "--id", "ab12"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Reopen { comment, .. } } => {
                assert!(comment.is_none());
            }
            _ => panic!("expected issue reopen command"),
        }
    }

    #[test]
    fn issue_reopen_parses_start_flag() {
        let cli = Cli::try_parse_from(["ns2", "issue", "reopen", "--id", "ab12", "--start"])
            .unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Reopen { start, .. } } => {
                assert!(start);
            }
            _ => panic!("expected issue reopen command"),
        }
    }

    #[test]
    fn issue_reopen_no_start_flag_is_false() {
        let cli = Cli::try_parse_from(["ns2", "issue", "reopen", "--id", "ab12"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Reopen { start, .. } } => {
                assert!(!start);
            }
            _ => panic!("expected issue reopen command"),
        }
    }

    #[test]
    fn issue_new_parses_branch_flag() {
        let cli = Cli::try_parse_from([
            "ns2", "issue", "new", "--title", "t", "--body", "b", "--branch", "feature/xyz",
        ])
        .unwrap();
        match cli.command {
            Command::Issue {
                action: IssueAction::New { branch, .. },
            } => {
                assert_eq!(branch.as_deref(), Some("feature/xyz"));
            }
            _ => panic!("expected issue new command"),
        }
    }

    #[test]
    fn issue_new_no_branch_is_none() {
        let cli = Cli::try_parse_from(["ns2", "issue", "new", "--title", "t", "--body", "b"])
            .unwrap();
        match cli.command {
            Command::Issue {
                action: IssueAction::New { branch, .. },
            } => {
                assert!(branch.is_none());
            }
            _ => panic!("expected issue new command"),
        }
    }

    #[test]
    fn issue_edit_parses_branch_flag() {
        let cli = Cli::try_parse_from([
            "ns2", "issue", "edit", "--id", "ab12", "--branch", "feat/my-branch",
        ])
        .unwrap();
        match cli.command {
            Command::Issue {
                action: IssueAction::Edit { branch, .. },
            } => {
                assert_eq!(branch.as_deref(), Some("feat/my-branch"));
            }
            _ => panic!("expected issue edit command"),
        }
    }

    #[test]
    fn issue_edit_no_branch_is_none() {
        let cli = Cli::try_parse_from(["ns2", "issue", "edit", "--id", "ab12", "--title", "new title"])
            .unwrap();
        match cli.command {
            Command::Issue {
                action: IssueAction::Edit { branch, .. },
            } => {
                assert!(branch.is_none());
            }
            _ => panic!("expected issue edit command"),
        }
    }

    #[test]
    fn issue_list_table_header_includes_branch() {
        let issue = Issue {
            id: "ab12".into(),
            title: "Test issue".into(),
            body: "body".into(),
            status: types::IssueStatus::Open,
            branch: "feat/my-feature".into(),
            assignee: Some("swe".into()),
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
                .unwrap()
                .into(),
            updated_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
                .unwrap()
                .into(),
        };
        let row = format_issue_row(&issue);
        assert!(row.contains("ab12"), "row should contain id");
        assert!(row.contains("Test issue"), "row should contain title");
        assert!(row.contains("open"), "row should contain status");
        assert!(row.contains("swe"), "row should contain assignee");
        assert!(row.contains("feat/my-feature"), "row should contain branch");
    }

    #[test]
    fn format_issue_row_truncates_long_title_and_branch() {
        let long_title = "A".repeat(40);
        let long_branch = "B".repeat(30);
        let issue = Issue {
            id: "xy99".into(),
            title: long_title,
            body: "body".into(),
            status: types::IssueStatus::Open,
            branch: long_branch,
            assignee: None,
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
                .unwrap()
                .into(),
            updated_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
                .unwrap()
                .into(),
        };
        let row = format_issue_row(&issue);
        // title field is limited to 30 chars, branch field to 25 chars
        let cols: Vec<&str> = row.splitn(7, "  ").collect();
        assert!(cols[1].trim().len() <= 30, "title column must be ≤30 chars");
        assert!(cols[4].trim().len() <= 25, "branch column must be ≤25 chars");
    }

    #[test]
    fn issue_new_parses_start_flag() {
        let cli = Cli::try_parse_from([
            "ns2", "issue", "new", "--title", "t", "--body", "b", "--assignee", "swe", "--start",
        ])
        .unwrap();
        match cli.command {
            Command::Issue {
                action: IssueAction::New { start, .. },
            } => {
                assert!(start);
            }
            _ => panic!("expected issue new command"),
        }
    }

    #[test]
    fn issue_new_no_start_flag_is_false() {
        let cli = Cli::try_parse_from(["ns2", "issue", "new", "--title", "t", "--body", "b"])
            .unwrap();
        match cli.command {
            Command::Issue {
                action: IssueAction::New { start, .. },
            } => {
                assert!(!start);
            }
            _ => panic!("expected issue new command"),
        }
    }

    // --- issue show CLI parse tests ---

    #[test]
    fn issue_show_parses_id_flag() {
        let cli = Cli::try_parse_from(["ns2", "issue", "show", "--id", "ab12"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Show { id, json } } => {
                assert_eq!(id, "ab12");
                assert!(!json);
            }
            _ => panic!("expected issue show command"),
        }
    }

    #[test]
    fn issue_show_parses_json_flag() {
        let cli = Cli::try_parse_from(["ns2", "issue", "show", "--id", "ab12", "--json"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Show { id, json } } => {
                assert_eq!(id, "ab12");
                assert!(json);
            }
            _ => panic!("expected issue show command"),
        }
    }

    #[test]
    fn issue_show_missing_id_fails_to_parse() {
        let result = Cli::try_parse_from(["ns2", "issue", "show"]);
        assert!(result.is_err(), "show without --id should fail to parse");
    }

    #[test]
    fn issue_cancel_parses_id_flag() {
        let cli = Cli::try_parse_from(["ns2", "issue", "cancel", "--id", "ab12"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Cancel { id } } => {
                assert_eq!(id, "ab12");
            }
            _ => panic!("expected issue cancel command"),
        }
    }

    #[test]
    fn issue_cancel_missing_id_fails_to_parse() {
        let result = Cli::try_parse_from(["ns2", "issue", "cancel"]);
        assert!(result.is_err(), "cancel without --id should fail to parse");
    }

    #[test]
    fn session_stop_parses_id_flag() {
        let cli = Cli::try_parse_from(["ns2", "session", "stop", "--id", "550e8400-e29b-41d4-a716-446655440000"]).unwrap();
        match cli.command {
            Command::Session { action: SessionAction::Stop { id, .. } } => {
                assert_eq!(id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
            }
            _ => panic!("expected session stop command"),
        }
    }

    // --- format_issue_show render tests ---

    fn make_show_issue() -> Issue {
        Issue {
            id: "ab12".into(),
            title: "My Title".into(),
            body: "My body text".into(),
            status: types::IssueStatus::Open,
            branch: "feat/my-branch".into(),
            assignee: Some("swe".into()),
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z").unwrap().into(),
            updated_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z").unwrap().into(),
        }
    }

    #[test]
    fn format_issue_show_contains_id_title_status_body() {
        let issue = make_show_issue();
        let out = format_issue_show(&issue);
        assert!(out.contains("ab12"), "should contain id");
        assert!(out.contains("My Title"), "should contain title");
        assert!(out.contains("open"), "should contain status");
        assert!(out.contains("My body text"), "should contain body");
    }

    #[test]
    fn format_issue_show_contains_assignee_and_branch() {
        let issue = make_show_issue();
        let out = format_issue_show(&issue);
        assert!(out.contains("swe"), "should contain assignee");
        assert!(out.contains("feat/my-branch"), "should contain branch");
    }

    #[test]
    fn format_issue_show_no_assignee_shows_dash() {
        let mut issue = make_show_issue();
        issue.assignee = None;
        let out = format_issue_show(&issue);
        assert!(out.contains("assignee:   -"), "no assignee should show dash");
    }

    #[test]
    fn format_issue_show_displays_comments() {
        let mut issue = make_show_issue();
        issue.comments = vec![types::IssueComment {
            author: "user".into(),
            body: "This is a comment".into(),
            created_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T11:00:00Z").unwrap().into(),
        }];
        let out = format_issue_show(&issue);
        assert!(out.contains("This is a comment"), "should contain comment body");
        assert!(out.contains("user"), "should contain comment author");
    }

    #[test]
    fn format_issue_show_no_comments_omits_comments_section() {
        let issue = make_show_issue();
        let out = format_issue_show(&issue);
        assert!(!out.contains("comments:"), "no comments should omit comments section");
    }

    #[test]
    fn worktree_create_parses_branch_flag() {
        let cli = Cli::try_parse_from([
            "ns2", "worktree", "create", "--branch", "feat/my-feature",
        ])
        .unwrap();
        match cli.command {
            Command::Worktree {
                action: WorktreeAction::Create { branch },
            } => {
                assert_eq!(branch, "feat/my-feature");
            }
            _ => panic!("expected worktree create command"),
        }
    }

    #[test]
    fn worktree_delete_parses_branch_and_force() {
        let cli = Cli::try_parse_from([
            "ns2", "worktree", "delete", "--branch", "feat/my-feature", "--force",
        ])
        .unwrap();
        match cli.command {
            Command::Worktree {
                action: WorktreeAction::Delete { branch, force },
            } => {
                assert_eq!(branch, "feat/my-feature");
                assert!(force);
            }
            _ => panic!("expected worktree delete command"),
        }
    }

    #[test]
    fn worktree_delete_no_force_defaults_false() {
        let cli = Cli::try_parse_from([
            "ns2", "worktree", "delete", "--branch", "feat/x",
        ])
        .unwrap();
        match cli.command {
            Command::Worktree {
                action: WorktreeAction::Delete { force, .. },
            } => {
                assert!(!force);
            }
            _ => panic!("expected worktree delete command"),
        }
    }

    #[test]
    fn worktree_list_parses() {
        let cli = Cli::try_parse_from(["ns2", "worktree", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Worktree { action: WorktreeAction::List }
        ));
    }

    // ─── Spinner / tree rendering tests ──────────────────────────────────────

    #[test]
    fn spinner_char_cycles_through_frames() {
        // First frame
        assert_eq!(spinner_char(0), SPINNER_FRAMES[0]);
        // Middle frame
        assert_eq!(spinner_char(5), SPINNER_FRAMES[5]);
        // Wraps around after all 10 frames
        assert_eq!(spinner_char(10), SPINNER_FRAMES[0]);
        assert_eq!(spinner_char(11), SPINNER_FRAMES[1]);
    }

    #[test]
    fn issue_status_symbol_running_returns_spinner() {
        let (sym, label) = issue_status_symbol(&types::IssueStatus::Running, 0);
        assert_eq!(sym, SPINNER_FRAMES[0].to_string());
        assert_eq!(label, "running");
    }

    #[test]
    fn issue_status_symbol_completed() {
        let (sym, label) = issue_status_symbol(&types::IssueStatus::Completed, 0);
        assert_eq!(sym, "✔");
        assert_eq!(label, "completed");
    }

    #[test]
    fn issue_status_symbol_failed() {
        let (sym, label) = issue_status_symbol(&types::IssueStatus::Failed, 0);
        assert_eq!(sym, "✗");
        assert_eq!(label, "failed");
    }

    #[test]
    fn issue_status_symbol_open() {
        let (sym, label) = issue_status_symbol(&types::IssueStatus::Open, 0);
        assert_eq!(sym, "●");
        assert_eq!(label, "open");
    }

    #[test]
    fn truncate_str_short_unchanged() {
        assert_eq!(truncate_str("hello", 30), "hello");
    }

    #[test]
    fn truncate_str_exact_limit_unchanged() {
        let s: String = "a".repeat(30);
        assert_eq!(truncate_str(&s, 30), s);
    }

    #[test]
    fn truncate_str_over_limit_truncated_with_ellipsis() {
        let s = "a".repeat(40);
        let result = truncate_str(&s, 30);
        // Must end with ellipsis and be ≤ 31 chars (30 + "…")
        assert!(result.ends_with('…'), "should end with ellipsis, got: {result}");
        let char_count = result.chars().count();
        // 30 chars + 1 ellipsis = 31
        assert!(char_count <= 31, "truncated string should be ≤31 chars, got: {char_count}");
    }

    fn make_tree_issue(id: &str, title: &str, status: types::IssueStatus) -> types::Issue {
        types::Issue {
            id: id.to_string(),
            title: title.to_string(),
            body: String::new(),
            status,
            branch: String::new(),
            assignee: None,
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    // --- spec verify CLI parse and behavior tests ---

    #[test]
    fn spec_verify_single_path_parses() {
        let cli =
            Cli::try_parse_from(["ns2", "spec", "verify", "crates/foo/foo.spec.md"]).unwrap();
        match cli.command {
            Command::Spec { action: SpecAction::Verify { paths } } => {
                assert_eq!(paths, vec!["crates/foo/foo.spec.md"]);
            }
            _ => panic!("expected spec verify command"),
        }
    }

    #[test]
    fn render_tree_line_root_no_snippet() {
        let node = IssueTreeNode {
            issue: make_tree_issue("ab12", "Fix the bug", types::IssueStatus::Running),
            snippet: None,
            children: vec![],
        };
        let line = render_tree_line(&node, "", 0, true);
        assert!(line.contains("[ab12]"), "must contain issue id");
        assert!(line.contains("Fix the bug"), "must contain title");
        assert!(line.contains("running"), "must contain status label");
    }

    #[test]
    fn render_tree_line_root_with_snippet() {
        let node = IssueTreeNode {
            issue: make_tree_issue("ab12", "Fix the bug", types::IssueStatus::Running),
            snippet: Some("Working on tests".to_string()),
            children: vec![],
        };
        let line = render_tree_line(&node, "", 0, true);
        assert!(line.contains("[ab12]"), "must contain issue id");
        assert!(line.contains("Fix the bug"), "must contain title");
        assert!(line.contains("Working on tests"), "must contain snippet");
        assert!(line.contains(": Working on tests"), "snippet must follow colon");
        assert!(line.contains("running"), "must contain status label");
    }

    #[test]
    fn render_tree_line_child_no_snippet_shown() {
        let node = IssueTreeNode {
            issue: make_tree_issue("cd34", "Sub task", types::IssueStatus::Open),
            snippet: Some("some content".to_string()),
            children: vec![],
        };
        // Children don't show snippet (is_root=false)
        let line = render_tree_line(&node, "├── ", 0, false);
        assert!(line.contains("[cd34]"), "must contain issue id");
        assert!(line.contains("Sub task"), "must contain title");
        assert!(line.contains("●"), "open symbol");
        assert!(line.contains("open"), "must contain status label");
        assert!(!line.contains("some content"), "child should not show snippet");
    }

    #[test]
    fn render_issue_tree_single_root_no_children() {
        let roots = vec![IssueTreeNode {
            issue: make_tree_issue("ab12", "Root issue", types::IssueStatus::Completed),
            snippet: None,
            children: vec![],
        }];
        let lines = render_issue_tree(&roots, 0);
        assert_eq!(lines.len(), 1, "single root with no children = 1 line");
        let line = &lines[0];
        // No prefix for single root
        assert!(line.starts_with('['), "single root should have no prefix connector");
        assert!(line.contains("[ab12]"));
        assert!(line.contains("✔"));
        assert!(line.contains("completed"));
    }

    #[test]
    fn render_issue_tree_root_with_children() {
        let child1 = IssueTreeNode {
            issue: make_tree_issue("cd34", "Child 1", types::IssueStatus::Running),
            snippet: None,
            children: vec![],
        };
        let child2 = IssueTreeNode {
            issue: make_tree_issue("ef56", "Child 2", types::IssueStatus::Open),
            snippet: None,
            children: vec![],
        };
        let roots = vec![IssueTreeNode {
            issue: make_tree_issue("ab12", "Root", types::IssueStatus::Running),
            snippet: None,
            children: vec![child1, child2],
        }];

        let lines = render_issue_tree(&roots, 3);
        assert_eq!(lines.len(), 3, "1 root + 2 children = 3 lines");

        // Root line: no prefix
        assert!(lines[0].starts_with('['), "root line should not have prefix");
        assert!(lines[0].contains("[ab12]"));

        // First child: ├──
        assert!(lines[1].contains("├──"), "non-last child should use ├──");
        assert!(lines[1].contains("[cd34]"));

        // Last child: └──
        assert!(lines[2].contains("└──"), "last child should use └──");
        assert!(lines[2].contains("[ef56]"));
    }

    #[test]
    fn render_issue_tree_multiple_roots() {
        let roots = vec![
            IssueTreeNode {
                issue: make_tree_issue("aa11", "Root A", types::IssueStatus::Running),
                snippet: None,
                children: vec![],
            },
            IssueTreeNode {
                issue: make_tree_issue("bb22", "Root B", types::IssueStatus::Completed),
                snippet: None,
                children: vec![],
            },
        ];
        let lines = render_issue_tree(&roots, 0);
        assert_eq!(lines.len(), 2);
        // Both roots get connector prefixes when there are multiple roots
        assert!(lines[0].contains("├──"), "first of multiple roots should use ├──");
        assert!(lines[1].contains("└──"), "last of multiple roots should use └──");
    }

    #[test]
    fn render_issue_tree_grandchildren() {
        let grandchild = IssueTreeNode {
            issue: make_tree_issue("gg99", "Grandchild", types::IssueStatus::Open),
            snippet: None,
            children: vec![],
        };
        let child = IssueTreeNode {
            issue: make_tree_issue("cc33", "Child", types::IssueStatus::Running),
            snippet: None,
            children: vec![grandchild],
        };
        let roots = vec![IssueTreeNode {
            issue: make_tree_issue("rr00", "Root", types::IssueStatus::Running),
            snippet: None,
            children: vec![child],
        }];

        let lines = render_issue_tree(&roots, 0);
        assert_eq!(lines.len(), 3, "root + child + grandchild = 3 lines");
        assert!(lines[0].contains("[rr00]"), "line 0 is root");
        assert!(lines[1].contains("[cc33]"), "line 1 is child");
        assert!(lines[2].contains("[gg99]"), "line 2 is grandchild");
        // Grandchild should have deeper indentation
        assert!(lines[2].len() > lines[1].len() - 1, "grandchild line should be indented more than child");
    }

    #[test]
    fn render_tree_line_spinner_cycles_for_running() {
        let node = IssueTreeNode {
            issue: make_tree_issue("ab12", "Task", types::IssueStatus::Running),
            snippet: None,
            children: vec![],
        };
        let line0 = render_tree_line(&node, "", 0, true);
        let line1 = render_tree_line(&node, "", 1, true);
        // Different ticks should produce different spinner chars (for most transitions)
        // Frame 0 is ⠋, frame 1 is ⠙
        assert!(line0.contains(SPINNER_FRAMES[0].to_string().as_str()));
        assert!(line1.contains(SPINNER_FRAMES[1].to_string().as_str()));
    }

    #[test]
    fn render_tree_line_completed_uses_checkmark() {
        let node = IssueTreeNode {
            issue: make_tree_issue("ab12", "Done task", types::IssueStatus::Completed),
            snippet: None,
            children: vec![],
        };
        let line = render_tree_line(&node, "", 99, true);
        assert!(line.contains("✔"), "completed should use checkmark regardless of tick");
        assert!(line.contains("completed"));
    }

    // ─── Session status symbol + render_session_line tests ───────────────────

    #[test]
    fn session_status_symbol_running_returns_spinner() {
        let (sym, label) = session_status_symbol(&types::SessionStatus::Running, 0);
        assert_eq!(sym, SPINNER_FRAMES[0].to_string());
        assert_eq!(label, "running");
        // Different ticks give different frames
        let (sym2, _) = session_status_symbol(&types::SessionStatus::Running, 3);
        assert_eq!(sym2, SPINNER_FRAMES[3].to_string());
    }

    #[test]
    fn session_status_symbol_completed() {
        let (sym, label) = session_status_symbol(&types::SessionStatus::Completed, 0);
        assert_eq!(sym, "✔");
        assert_eq!(label, "completed");
    }

    #[test]
    fn session_status_symbol_failed() {
        let (sym, label) = session_status_symbol(&types::SessionStatus::Failed, 0);
        assert_eq!(sym, "✗");
        assert_eq!(label, "failed");
    }

    #[test]
    fn session_status_symbol_cancelled() {
        let (sym, label) = session_status_symbol(&types::SessionStatus::Cancelled, 0);
        assert_eq!(sym, "●");
        assert_eq!(label, "cancelled");
    }

    #[test]
    fn render_session_line_running_with_snippet() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let line = render_session_line(id, "my-task", Some("reading files"), &types::SessionStatus::Running, 0);
        // ID prefix (first 8 chars)
        assert!(line.contains("[550e8400]"), "should show first 8 chars of UUID");
        assert!(line.contains("my-task"), "should show name");
        assert!(line.contains("reading files"), "should show snippet");
        assert!(line.contains(SPINNER_FRAMES[0].to_string().as_str()), "should show spinner");
        assert!(line.contains("running"));
    }

    #[test]
    fn render_session_line_completed() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let line = render_session_line(id, "done-task", None, &types::SessionStatus::Completed, 5);
        assert!(line.contains("[550e8400]"));
        assert!(line.contains("done-task"));
        assert!(line.contains("✔"));
        assert!(line.contains("completed"));
        // No snippet section when None
        assert!(!line.contains("None"));
    }

    #[test]
    fn render_session_line_empty_name_shows_dash() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let line = render_session_line(id, "", None, &types::SessionStatus::Completed, 0);
        assert!(line.contains("[550e8400] -:"), "empty name should render as dash");
    }

    // ─── P1: all_nodes_terminal must check entire tree, not just roots ────────

    #[test]
    fn all_nodes_terminal_returns_false_when_child_is_running() {
        // Root is completed but child is still running — should NOT be terminal.
        let running_child = IssueTreeNode {
            issue: make_tree_issue("ch01", "Child", types::IssueStatus::Running),
            snippet: None,
            children: vec![],
        };
        let roots = vec![IssueTreeNode {
            issue: make_tree_issue("rt01", "Root", types::IssueStatus::Completed),
            snippet: None,
            children: vec![running_child],
        }];
        assert!(
            !all_nodes_terminal(&roots),
            "should be false when any child is still running"
        );
    }

    #[test]
    fn all_nodes_terminal_returns_true_when_all_nodes_done() {
        let done_child = IssueTreeNode {
            issue: make_tree_issue("ch02", "Child", types::IssueStatus::Completed),
            snippet: None,
            children: vec![],
        };
        let roots = vec![IssueTreeNode {
            issue: make_tree_issue("rt02", "Root", types::IssueStatus::Completed),
            snippet: None,
            children: vec![done_child],
        }];
        assert!(
            all_nodes_terminal(&roots),
            "should be true when all nodes are terminal"
        );
    }

    #[test]
    fn all_nodes_terminal_returns_false_when_grandchild_is_running() {
        let running_grandchild = IssueTreeNode {
            issue: make_tree_issue("gc01", "Grandchild", types::IssueStatus::Running),
            snippet: None,
            children: vec![],
        };
        let done_child = IssueTreeNode {
            issue: make_tree_issue("ch03", "Child", types::IssueStatus::Completed),
            snippet: None,
            children: vec![running_grandchild],
        };
        let roots = vec![IssueTreeNode {
            issue: make_tree_issue("rt03", "Root", types::IssueStatus::Completed),
            snippet: None,
            children: vec![done_child],
        }];
        assert!(
            !all_nodes_terminal(&roots),
            "should be false when a grandchild is still running"
        );
    }

    // ─── P2a: Multi-root children indentation ────────────────────────────────

    #[test]
    fn multi_root_non_last_root_children_indented_with_pipe() {
        // When there are 2 roots and the first root (├──) has children,
        // those children should be indented with "│   " (continuing bar).
        let child = IssueTreeNode {
            issue: make_tree_issue("ch10", "Child of A", types::IssueStatus::Running),
            snippet: None,
            children: vec![],
        };
        let roots = vec![
            IssueTreeNode {
                issue: make_tree_issue("aa10", "Root A", types::IssueStatus::Running),
                snippet: None,
                children: vec![child],
            },
            IssueTreeNode {
                issue: make_tree_issue("bb10", "Root B", types::IssueStatus::Completed),
                snippet: None,
                children: vec![],
            },
        ];
        let lines = render_issue_tree(&roots, 0);
        // lines[0] = Root A (├── prefix)
        // lines[1] = Child of A — must start with "│   " because Root A is non-last
        // lines[2] = Root B (└── prefix)
        assert_eq!(lines.len(), 3);
        assert!(
            lines[1].starts_with("│   "),
            "child of non-last root must start with │   , got: {:?}",
            lines[1]
        );
    }

    #[test]
    fn multi_root_last_root_children_indented_with_spaces() {
        // When there are 2 roots and the last root (└──) has children,
        // those children should be indented with "    " (four spaces, no bar).
        let child = IssueTreeNode {
            issue: make_tree_issue("ch11", "Child of B", types::IssueStatus::Running),
            snippet: None,
            children: vec![],
        };
        let roots = vec![
            IssueTreeNode {
                issue: make_tree_issue("aa11", "Root A", types::IssueStatus::Completed),
                snippet: None,
                children: vec![],
            },
            IssueTreeNode {
                issue: make_tree_issue("bb11", "Root B", types::IssueStatus::Running),
                snippet: None,
                children: vec![child],
            },
        ];
        let lines = render_issue_tree(&roots, 0);
        // lines[0] = Root A (├── prefix)
        // lines[1] = Root B (└── prefix)
        // lines[2] = Child of B — must start with "    " (4 spaces) because Root B is last
        assert_eq!(lines.len(), 3);
        assert!(
            lines[2].starts_with("    "),
            "child of last root must start with 4 spaces, got: {:?}",
            lines[2]
        );
        assert!(
            !lines[2].starts_with("│"),
            "child of last root must NOT start with │, got: {:?}",
            lines[2]
        );
    }

    // ─── P2b: Snippet newline sanitization ───────────────────────────────────

    #[test]
    fn render_tree_line_snippet_with_newline_is_sanitized() {
        let node = IssueTreeNode {
            issue: make_tree_issue("ab99", "Task", types::IssueStatus::Running),
            snippet: Some("line one\nline two".to_string()),
            children: vec![],
        };
        let line = render_tree_line(&node, "", 0, true);
        // The output line must NOT contain a literal newline character
        assert!(
            !line.contains('\n'),
            "rendered tree line must not contain literal newline, got: {line:?}"
        );
        // The snippet content should appear with a space substituted for the newline
        assert!(
            line.contains("line one line two"),
            "newline in snippet should be replaced with space, got: {line:?}"
        );
    }

    #[test]
    fn render_tree_line_snippet_with_carriage_return_is_sanitized() {
        let node = IssueTreeNode {
            issue: make_tree_issue("ab98", "Task", types::IssueStatus::Running),
            snippet: Some("line one\r\nline two".to_string()),
            children: vec![],
        };
        let line = render_tree_line(&node, "", 0, true);
        assert!(
            !line.contains('\n'),
            "rendered tree line must not contain literal newline"
        );
        assert!(
            !line.contains('\r'),
            "rendered tree line must not contain carriage return"
        );
    }

    // ─── P3: session_status_symbol Created label ─────────────────────────────

    #[test]
    fn session_status_symbol_created_returns_created_label() {
        let (sym, label) = session_status_symbol(&types::SessionStatus::Created, 0);
        // Symbol should still be a spinner (animated)
        assert_eq!(sym, SPINNER_FRAMES[0].to_string(), "created status should use spinner");
        // Label must NOT be "running" — should be "created" or "waiting"
        assert_ne!(
            label, "running",
            "Created status label must not be 'running', should be 'created'"
        );
        assert_eq!(label, "created", "Created status should have label 'created'");
    }

    // ─── P4: render_session_line sanitizes \r in snippet ─────────────────────

    #[test]
    fn render_session_line_snippet_with_carriage_return_is_sanitized() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let line = render_session_line(
            id,
            "task",
            Some("first\r\nsecond"),
            &types::SessionStatus::Running,
            0,
        );
        assert!(
            !line.contains('\r'),
            "render_session_line must not embed carriage return in output, got: {line:?}"
        );
        assert!(
            !line.contains('\n'),
            "render_session_line must not embed literal newline in output"
        );
    }

    #[test]
    fn spec_verify_multiple_paths_parse() {
        let cli = Cli::try_parse_from([
            "ns2",
            "spec",
            "verify",
            "crates/foo/foo.spec.md",
            "crates/bar/bar.spec.md",
            "crates/baz/baz.spec.md",
        ])
        .unwrap();
        match cli.command {
            Command::Spec { action: SpecAction::Verify { paths } } => {
                assert_eq!(
                    paths,
                    vec![
                        "crates/foo/foo.spec.md",
                        "crates/bar/bar.spec.md",
                        "crates/baz/baz.spec.md",
                    ]
                );
            }
            _ => panic!("expected spec verify command"),
        }
    }

    #[test]
    fn spec_verify_no_paths_fails_to_parse() {
        let result = Cli::try_parse_from(["ns2", "spec", "verify"]);
        assert!(result.is_err(), "verify with no paths should fail to parse");
    }

    // Helper: write a minimal valid spec file into a temp directory and return its path.
    fn write_temp_spec(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            "---\ntargets:\n  - crates/foo/src/**/*.rs\n---\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn verify_spec_paths_single_success() {
        let tmp = tempfile::tempdir().unwrap();
        let git_root = tmp.path();
        write_temp_spec(git_root, "a.spec.md");

        let result = verify_spec_paths(git_root, &["a.spec.md".to_string()]);

        assert!(!result.any_failed, "should not fail for a valid spec");
        assert_eq!(result.stdout_lines, vec!["Verified a.spec.md"]);
        assert!(result.stderr_lines.is_empty(), "no stderr on success");

        // Confirm the file was actually updated with a verified timestamp.
        let updated = specs::load_spec(&git_root.join("a.spec.md")).unwrap();
        assert!(updated.verified.is_some(), "verified timestamp should be written");
    }

    #[test]
    fn verify_spec_paths_multiple_all_succeed() {
        let tmp = tempfile::tempdir().unwrap();
        let git_root = tmp.path();
        write_temp_spec(git_root, "a.spec.md");
        write_temp_spec(git_root, "b.spec.md");
        write_temp_spec(git_root, "c.spec.md");

        let paths: Vec<String> =
            ["a.spec.md", "b.spec.md", "c.spec.md"].iter().map(|s| (*s).to_string()).collect();
        let result = verify_spec_paths(git_root, &paths);

        assert!(!result.any_failed);
        assert_eq!(result.stdout_lines.len(), 3);
        assert!(result.stdout_lines.contains(&"Verified a.spec.md".to_string()));
        assert!(result.stdout_lines.contains(&"Verified b.spec.md".to_string()));
        assert!(result.stdout_lines.contains(&"Verified c.spec.md".to_string()));
        assert!(result.stderr_lines.is_empty());

        // All files have a verified timestamp.
        for name in &["a.spec.md", "b.spec.md", "c.spec.md"] {
            let def = specs::load_spec(&git_root.join(name)).unwrap();
            assert!(def.verified.is_some(), "{name} should have verified timestamp");
        }
    }

    #[test]
    fn verify_spec_paths_one_nonexistent_others_succeed() {
        let tmp = tempfile::tempdir().unwrap();
        let git_root = tmp.path();
        write_temp_spec(git_root, "good1.spec.md");
        write_temp_spec(git_root, "good2.spec.md");
        // "missing.spec.md" is intentionally not created.

        let paths: Vec<String> = ["good1.spec.md", "missing.spec.md", "good2.spec.md"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let result = verify_spec_paths(git_root, &paths);

        assert!(result.any_failed, "should fail when a path is missing");

        // The two valid specs still produce success lines.
        assert!(result.stdout_lines.contains(&"Verified good1.spec.md".to_string()));
        assert!(result.stdout_lines.contains(&"Verified good2.spec.md".to_string()));

        // The missing spec produces a stderr line.
        assert_eq!(result.stderr_lines.len(), 1);
        assert!(
            result.stderr_lines[0].contains("missing.spec.md"),
            "stderr should mention the missing path, got: {}",
            result.stderr_lines[0]
        );

        // The two good specs were actually written.
        for name in &["good1.spec.md", "good2.spec.md"] {
            let def = specs::load_spec(&git_root.join(name)).unwrap();
            assert!(def.verified.is_some(), "{name} should have verified timestamp");
        }
    }

    // ─── `ns2 hook` CLI parse tests ──────────────────────────────────────────

    #[test]
    fn hook_new_parses_required_flags() {
        let cli = Cli::try_parse_from([
            "ns2", "hook", "new",
            "--name", "notify",
            "--source", "internal",
            "--event-type", "issue.status_changed",
            "--action", "send-message",
            "--target", "issue:abc1",
            "--body", "Status changed",
        ]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::New { name, source, event_types, action, target, body, .. } } => {
                assert_eq!(name, "notify");
                assert_eq!(source, "internal");
                assert_eq!(event_types, vec!["issue.status_changed"]);
                assert_eq!(action, "send-message");
                assert_eq!(target.as_deref(), Some("issue:abc1"));
                assert_eq!(body.as_deref(), Some("Status changed"));
            }
            _ => panic!("expected hook new command"),
        }
    }

    #[test]
    fn hook_new_parses_multiple_event_types() {
        let cli = Cli::try_parse_from([
            "ns2", "hook", "new",
            "--name", "multi",
            "--source", "internal",
            "--event-type", "issue.created",
            "--event-type", "issue.status_changed",
            "--action", "send-message",
            "--target", "issue:w1",
            "--body", "hi",
        ]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::New { event_types, .. } } => {
                assert_eq!(event_types.len(), 2);
                assert!(event_types.contains(&"issue.created".to_string()));
                assert!(event_types.contains(&"issue.status_changed".to_string()));
            }
            _ => panic!("expected hook new command"),
        }
    }

    #[test]
    fn hook_new_parses_filter_fields() {
        let cli = Cli::try_parse_from([
            "ns2", "hook", "new",
            "--name", "filtered",
            "--source", "internal",
            "--event-type", "issue.status_changed",
            "--filter-field", "data.issue.status=running",
            "--action", "send-message",
            "--target", "issue:w1",
            "--body", "hi",
        ]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::New { filter_fields, .. } } => {
                assert_eq!(filter_fields.len(), 1);
                assert_eq!(filter_fields[0], "data.issue.status=running");
            }
            _ => panic!("expected hook new command"),
        }
    }

    #[test]
    fn hook_list_parses_enabled_flag() {
        let cli = Cli::try_parse_from(["ns2", "hook", "list", "--enabled"]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::List { enabled, source_type } } => {
                assert!(enabled);
                assert!(source_type.is_none());
            }
            _ => panic!("expected hook list command"),
        }
    }

    #[test]
    fn hook_list_no_flags() {
        let cli = Cli::try_parse_from(["ns2", "hook", "list"]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::List { enabled, source_type } } => {
                assert!(!enabled);
                assert!(source_type.is_none());
            }
            _ => panic!("expected hook list command"),
        }
    }

    #[test]
    fn hook_show_parses_id() {
        let cli = Cli::try_parse_from(["ns2", "hook", "show", "--id", "h001"]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::Show { id } } => {
                assert_eq!(id, "h001");
            }
            _ => panic!("expected hook show command"),
        }
    }

    #[test]
    fn hook_enable_parses_id() {
        let cli = Cli::try_parse_from(["ns2", "hook", "enable", "--id", "h001"]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::Enable { id } } => {
                assert_eq!(id, "h001");
            }
            _ => panic!("expected hook enable command"),
        }
    }

    #[test]
    fn hook_disable_parses_id() {
        let cli = Cli::try_parse_from(["ns2", "hook", "disable", "--id", "h001"]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::Disable { id } } => {
                assert_eq!(id, "h001");
            }
            _ => panic!("expected hook disable command"),
        }
    }

    #[test]
    fn hook_delete_parses_id() {
        let cli = Cli::try_parse_from(["ns2", "hook", "delete", "--id", "h001"]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::Delete { id } } => {
                assert_eq!(id, "h001");
            }
            _ => panic!("expected hook delete command"),
        }
    }

    #[test]
    fn hook_logs_parses_id_and_limit() {
        let cli = Cli::try_parse_from([
            "ns2", "hook", "logs", "--id", "h001", "--limit", "5",
        ]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::Logs { id, limit } } => {
                assert_eq!(id, "h001");
                assert_eq!(limit, 5);
            }
            _ => panic!("expected hook logs command"),
        }
    }

    #[test]
    fn hook_logs_default_limit_is_20() {
        let cli = Cli::try_parse_from(["ns2", "hook", "logs", "--id", "h001"]).unwrap();
        match cli.command {
            Command::Hook { action: HookAction::Logs { limit, .. } } => {
                assert_eq!(limit, 20);
            }
            _ => panic!("expected hook logs command"),
        }
    }

    // ─── `ns2 issue watch` CLI parse tests ───────────────────────────────────

    #[test]
    fn issue_watch_parses_id_flag() {
        let cli = Cli::try_parse_from(["ns2", "issue", "watch", "--id", "ab12"]).unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Watch { id } } => {
                assert_eq!(id, "ab12");
            }
            _ => panic!("expected issue watch command"),
        }
    }

    #[test]
    fn issue_watch_missing_id_fails_to_parse() {
        let result = Cli::try_parse_from(["ns2", "issue", "watch"]);
        assert!(result.is_err(), "watch without --id should fail to parse");
    }

    // ─── `ns2 issue subscribe` CLI parse tests ────────────────────────────────

    #[test]
    fn issue_subscribe_parses_id_and_deliver_to_issue() {
        let cli = Cli::try_parse_from([
            "ns2", "issue", "subscribe", "--id", "ab12", "--deliver-to", "issue:watcher1",
        ])
        .unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Subscribe { id, deliver_to } } => {
                assert_eq!(id, "ab12");
                assert_eq!(deliver_to, "issue:watcher1");
            }
            _ => panic!("expected issue subscribe command"),
        }
    }

    #[test]
    fn issue_subscribe_parses_deliver_to_session() {
        let cli = Cli::try_parse_from([
            "ns2", "issue", "subscribe",
            "--id", "ab12",
            "--deliver-to", "session:550e8400-e29b-41d4-a716-446655440000",
        ])
        .unwrap();
        match cli.command {
            Command::Issue { action: IssueAction::Subscribe { deliver_to, .. } } => {
                assert_eq!(deliver_to, "session:550e8400-e29b-41d4-a716-446655440000");
            }
            _ => panic!("expected issue subscribe command"),
        }
    }

    #[test]
    fn issue_subscribe_missing_id_fails_to_parse() {
        let result = Cli::try_parse_from(["ns2", "issue", "subscribe", "--deliver-to", "issue:w1"]);
        assert!(result.is_err(), "subscribe without --id should fail to parse");
    }

    #[test]
    fn issue_subscribe_missing_deliver_to_fails_to_parse() {
        let result = Cli::try_parse_from(["ns2", "issue", "subscribe", "--id", "ab12"]);
        assert!(result.is_err(), "subscribe without --deliver-to should fail to parse");
    }
}
