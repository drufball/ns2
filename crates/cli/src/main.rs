use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use types::{Issue, Session};
use uuid::Uuid;

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
    #[command(about = "Manage git worktrees for branches.", long_about = "Worktrees let multiple branches be checked out simultaneously into separate directories.\nEach worktree maps a branch to a directory under the configured worktree base path.\n\nThe base path is read from ns2.toml ([worktrees] path = ...) or defaults to\n~/.ns2/<repo-name>/worktrees/.\n\nSubcommands:\n  list    Print all worktrees under the base path\n  create  Create a worktree for a branch (idempotent)\n  delete  Remove a worktree and its branch")]
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
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
    Stop,
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

pub fn data_dir_and_pid(port: u16) -> (PathBuf, PathBuf) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let repo_name = workspace::git_root_sync()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "default".to_string());

    let data_dir = PathBuf::from(&home).join(".ns2").join(&repo_name);
    let pid_file = data_dir.join(format!("server-{port}.pid"));
    (data_dir, pid_file)
}

fn format_issue_row(issue: &Issue) -> String {
    format!(
        "{:<6}  {:<30}  {:<10}  {:<12}  {:<25}  {}",
        issue.id,
        if issue.title.chars().count() > 30 {
            issue.title.chars().take(30).collect::<String>()
        } else {
            issue.title.clone()
        },
        issue.status.to_string(),
        issue.assignee.as_deref().unwrap_or("-"),
        if issue.branch.chars().count() > 25 {
            issue.branch.chars().take(25).collect::<String>()
        } else {
            issue.branch.clone()
        },
        issue.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
    )
}

fn print_issue_row(issue: &Issue) {
    println!("{}", format_issue_row(issue));
}

fn handle_connection_error(err: &reqwest::Error) -> ! {
    if err.is_connect() {
        eprintln!("Error: server is not running (connection refused). Start it with: ns2 server start");
    } else {
        eprintln!("Error: {err}");
    }
    std::process::exit(1);
}

async fn print_error_response(resp: reqwest::Response) -> ! {
    let status = resp.status();
    if let Ok(body) = resp.json::<serde_json::Value>().await {
        if let Some(msg) = body.get("error").and_then(|v| v.as_str()) {
            eprintln!("Error: {msg}");
        } else {
            eprintln!("Error: {status}");
        }
    } else {
        eprintln!("Error: {status}");
    }
    std::process::exit(1);
}

pub fn format_session_event(event: &types::SessionEvent) -> Option<String> {
    use types::SessionEvent::*;
    match event {
        TurnStarted { turn } => Some(format!("[turn {}]\n", turn.id)),
        ContentBlockDelta {
            delta: types::ContentBlockDelta::TextDelta { text },
            ..
        } => Some(text.clone()),
        ContentBlockDelta {
            delta: types::ContentBlockDelta::InputJsonDelta { .. },
            ..
        } => None,
        ContentBlockDone { block, .. } => match block {
            types::ContentBlock::Text { .. } => Some("\n".to_string()),
            types::ContentBlock::ToolUse { name, input, .. } => {
                Some(format!("[tool: {}({})]\n", name, input))
            }
            types::ContentBlock::ToolResult { content, .. } => {
                Some(format!("[result: {}]\n", content))
            }
        },
        TurnDone { .. } => None,
        SessionDone { .. } => Some("[done]\n".to_string()),
        Error { message } => Some(format!("[error] {message}\n")),
    }
}

fn print_session_event(event: &types::SessionEvent, to_stderr: bool) {
    use std::io::Write;
    use types::SessionEvent::*;
    match event {
        // Text deltas stream without a newline; flush so the terminal shows them immediately.
        ContentBlockDelta {
            delta: types::ContentBlockDelta::TextDelta { .. },
            ..
        } => {
            if let Some(text) = format_session_event(event) {
                if to_stderr {
                    eprint!("{text}");
                    std::io::stderr().flush().ok();
                } else {
                    print!("{text}");
                    std::io::stdout().flush().ok();
                }
            }
        }
        // Errors always go to stderr.
        Error { message } => eprintln!("[error] {message}"),
        // Everything else: print the formatted string (which already includes a newline).
        _ => {
            if let Some(output) = format_session_event(event) {
                if to_stderr {
                    eprint!("{output}");
                } else {
                    print!("{output}");
                }
            }
        }
    }
}

pub fn parse_sse_frames(buffer: &mut String, new_data: &str) -> Vec<String> {
    buffer.push_str(new_data);
    let mut frames = Vec::new();
    while let Some(pos) = buffer.find("\n\n") {
        let frame = buffer[..pos].to_string();
        *buffer = buffer[pos + 2..].to_string();
        frames.push(frame);
    }
    frames
}

pub fn format_sync_error(spec_path: &str, stale: &[PathBuf]) -> String {
    let mut out = format!("[error] spec {spec_path} has stale files:\n");
    for f in stale {
        out.push_str(&format!("  {}\n", f.display()));
    }
    out
}

pub fn format_sync_warning(spec_path: &str, stale: &[PathBuf]) -> String {
    let mut out = format!("[warning] spec {spec_path} has stale files:\n");
    for f in stale {
        out.push_str(&format!("  {}\n", f.display()));
    }
    out
}

async fn stream_events(url: &str, to_stderr: bool) {
    use futures::StreamExt;
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap_or_else(|e| handle_connection_error(&e));

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap_or_else(|e| {
            eprintln!("Stream error: {e}");
            std::process::exit(1);
        });
        if let Ok(s) = std::str::from_utf8(&chunk) {
            let frames = parse_sse_frames(&mut buffer, s);
            for line in frames {
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<types::SessionEvent>(data) {
                        print_session_event(&event, to_stderr);
                        if matches!(event, types::SessionEvent::SessionDone { .. }) {
                            return;
                        }
                        if matches!(event, types::SessionEvent::Error { .. }) {
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
    }
}

async fn resolve_session_id(server: &str, id: Option<String>, name: Option<String>) -> Uuid {
    if let Some(id) = id {
        id.parse().unwrap_or_else(|_| {
            eprintln!("Invalid session ID");
            std::process::exit(1);
        })
    } else if let Some(name) = name {
        let client = reqwest::Client::new();
        let sessions: Vec<types::Session> = client
            .get(format!("{server}/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        sessions
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.id)
            .unwrap_or_else(|| {
                eprintln!("Session not found: {name}");
                std::process::exit(1);
            })
    } else {
        eprintln!("Must provide --id or --name");
        std::process::exit(1);
    }
}

pub fn issue_is_terminal(status: &types::IssueStatus) -> bool {
    matches!(status, types::IssueStatus::Completed | types::IssueStatus::Failed)
}

/// Check whether every node in the issue tree (roots AND all descendants) is terminal.
/// Used by `issue wait` to decide when to stop polling.
pub fn all_nodes_terminal(roots: &[IssueTreeNode]) -> bool {
    fn node_terminal(node: &IssueTreeNode) -> bool {
        issue_is_terminal(&node.issue.status) && node.children.iter().all(node_terminal)
    }
    roots.iter().all(node_terminal)
}

/// The result of verifying a batch of spec paths.
pub struct VerifyResult {
    /// Lines to print to stdout (one per successfully verified path).
    pub stdout_lines: Vec<String>,
    /// Lines to print to stderr (one per failure).
    pub stderr_lines: Vec<String>,
    /// Whether any path failed.
    pub any_failed: bool,
}

/// Core logic for `ns2 spec verify <paths...>`.
///
/// For each path: resolve it relative to `git_root`, attempt to load + write the spec,
/// record success/failure.  Does NOT call `process::exit` — returns a [`VerifyResult`]
/// so callers (main and tests) can assert on the outcome.
pub fn verify_spec_paths(git_root: &std::path::Path, paths: &[String]) -> VerifyResult {
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut any_failed = false;

    for path in paths {
        let resolved = if PathBuf::from(path).is_absolute() {
            PathBuf::from(path)
        } else {
            git_root.join(path)
        };

        let mut def = match specs::load_spec(&resolved) {
            Some(d) => d,
            None => {
                stderr_lines.push(format!("Error: could not load spec at {path}"));
                any_failed = true;
                continue;
            }
        };

        def.verified = Some(chrono::Utc::now());

        if let Err(e) = specs::write_spec(&resolved, &def) {
            stderr_lines.push(format!("Error writing spec file {path}: {e}"));
            any_failed = true;
            continue;
        }

        stdout_lines.push(format!("Verified {path}"));
    }

    VerifyResult { stdout_lines, stderr_lines, any_failed }
}

pub fn session_is_terminal(status: &types::SessionStatus) -> bool {
    matches!(
        status,
        types::SessionStatus::Completed | types::SessionStatus::Failed | types::SessionStatus::Cancelled
    )
}

/// Braille spinner frames for animated "running" indicator.
pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Return the spinner character for the given tick index.
pub fn spinner_char(tick: usize) -> char {
    SPINNER_FRAMES[tick % SPINNER_FRAMES.len()]
}

/// Return the symbol and status label for an issue status.
pub fn issue_status_symbol(status: &types::IssueStatus, tick: usize) -> (String, &'static str) {
    match status {
        types::IssueStatus::Running => (spinner_char(tick).to_string(), "running"),
        types::IssueStatus::Completed => ("✔".to_string(), "completed"),
        types::IssueStatus::Failed => ("✗".to_string(), "failed"),
        types::IssueStatus::Open => ("●".to_string(), "open"),
    }
}

/// Truncate a string to at most `max_chars` Unicode characters.
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}…", truncated.trim_end())
    }
}

/// A node in the issue tree for rendering.
pub struct IssueTreeNode {
    pub issue: types::Issue,
    pub snippet: Option<String>,
    pub children: Vec<IssueTreeNode>,
}

/// Render a single tree line for one issue node.
///
/// - `prefix` is the indentation/connector string (e.g., "├── ", "└── ", "│   ├── ").
/// - `tick` is the spinner frame counter.
/// - `is_root` controls whether to include the snippet.
pub fn render_tree_line(node: &IssueTreeNode, prefix: &str, tick: usize, is_root: bool) -> String {
    let (sym, status_label) = issue_status_symbol(&node.issue.status, tick);
    let id = &node.issue.id;

    let title_part = truncate_str(&node.issue.title, 30);

    if is_root {
        let snippet_part = if let Some(ref snippet) = node.snippet {
            // Sanitize: replace newlines/carriage-returns with spaces so the
            // line-count assumption used for ANSI cursor-up redraw is not broken.
            let clean: String = snippet.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
            let s = truncate_str(&clean, 30);
            if s.is_empty() {
                String::new()
            } else {
                format!(": {s}")
            }
        } else {
            String::new()
        };
        format!("{prefix}[{id}] {title_part}{snippet_part}  {sym} {status_label}")
    } else {
        format!("{prefix}[{id}] {title_part}  {sym} {status_label}")
    }
}

/// Render the full issue tree to a vector of lines.
pub fn render_issue_tree(roots: &[IssueTreeNode], tick: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (ri, root) in roots.iter().enumerate() {
        let is_last_root = ri + 1 == roots.len();
        let root_prefix = if roots.len() > 1 {
            if !is_last_root { "├── " } else { "└── " }
        } else {
            ""
        };
        lines.push(render_tree_line(root, root_prefix, tick, true));
        // When there are multiple roots, children must be indented relative to the
        // root's own connector so the tree lines up correctly:
        //   ├── Root A          <- root_prefix = "├── "
        //   │   ├── Child 1     <- parent_indent = "│   "
        //   │   └── Child 2
        //   └── Root B          <- root_prefix = "└── "
        //       └── Child 3     <- parent_indent = "    "
        let child_indent = if roots.len() > 1 {
            if !is_last_root { "│   " } else { "    " }
        } else {
            ""
        };
        render_children(&root.children, child_indent, tick, &mut lines);
    }
    lines
}

fn render_children(children: &[IssueTreeNode], parent_indent: &str, tick: usize, lines: &mut Vec<String>) {
    for (i, child) in children.iter().enumerate() {
        let is_last = i + 1 == children.len();
        let connector = if is_last { "└── " } else { "├── " };
        let prefix = format!("{parent_indent}{connector}");
        lines.push(render_tree_line(child, &prefix, tick, false));

        // Recurse into grandchildren
        let child_indent = if is_last {
            format!("{parent_indent}    ")
        } else {
            format!("{parent_indent}│   ")
        };
        render_children(&child.children, &child_indent, tick, lines);
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Session wait progress rendering

/// Return `(symbol, label)` for the given session status at a tick.
/// Running sessions get an animated braille spinner; terminal sessions get a static symbol.
pub fn session_status_symbol(status: &types::SessionStatus, tick: usize) -> (String, &'static str) {
    match status {
        types::SessionStatus::Running => (spinner_char(tick).to_string(), "running"),
        types::SessionStatus::Created => (spinner_char(tick).to_string(), "created"),
        types::SessionStatus::Completed => ("✔".to_string(), "completed"),
        types::SessionStatus::Failed => ("✗".to_string(), "failed"),
        types::SessionStatus::Cancelled => ("●".to_string(), "cancelled"),
    }
}

/// Render one progress line for a session.
///
/// Format: `[<id-prefix>] <name>: <snippet>  <sym> <status>`
///
/// - `id` is the full session UUID string; the first 8 chars are shown.
/// - `name` is the session name (empty string renders as `-`).
/// - `snippet` is an optional last-content text snippet (truncated to 40 chars).
pub fn render_session_line(
    id: &str,
    name: &str,
    snippet: Option<&str>,
    status: &types::SessionStatus,
    tick: usize,
) -> String {
    let id_prefix = &id[..id.len().min(8)];
    let display_name = if name.is_empty() { "-" } else { name };
    let (sym, label) = session_status_symbol(status, tick);
    let snippet_part = match snippet {
        Some(s) if !s.trim().is_empty() => {
            let trimmed = s.trim().replace(['\n', '\r'], " ");
            format!("{}  ", truncate_str(&trimmed, 40))
        }
        _ => String::new(),
    };
    format!("[{id_prefix}] {display_name}: {snippet_part}{sym} {label}")
}

#[tokio::main]
async fn main() {
    load_dotenv();
    let cli = Cli::parse();

    match cli.command {
        Command::Server { action } => match action {
            ServerAction::Start { port } => {
                let (data_dir, pid_file) = data_dir_and_pid(port);
                let config = server::ServerConfig {
                    port,
                    data_dir,
                    pid_file,
                    client: {
                        let key = std::env::var("ANTHROPIC_API_KEY").ok();
                        let c: Arc<dyn anthropic::AnthropicClient> = match key {
                            Some(k) => Arc::new(anthropic::Client::new(k)),
                            None => {
                                eprintln!("Warning: ANTHROPIC_API_KEY not set — using stub client (responses will be fake)");
                                Arc::new(harness::StubClient)
                            }
                        };
                        c
                    },
                    tools: vec![
                        Arc::new(tools::ReadTool { cwd: None }),
                        Arc::new(tools::BashTool { cwd: None }),
                        Arc::new(tools::WriteTool { cwd: None }),
                        Arc::new(tools::EditTool { cwd: None }),
                    ],
                    model: std::env::var("ANTHROPIC_MODEL")
                        .unwrap_or_else(|_| "claude-sonnet-4-6".to_string()),
                };
                if let Err(e) = server::run(config).await {
                    eprintln!("Server error: {e}");
                    std::process::exit(1);
                }
            }
            ServerAction::Stop => {
                let (_, pid_file) = data_dir_and_pid(9876);
                match std::fs::read_to_string(&pid_file) {
                    Ok(pid_str) => {
                        let pid = pid_str.trim().to_string();
                        // Use sh to invoke kill so the shell builtin is available
                        // even on minimal systems without a standalone kill binary.
                        let result = std::process::Command::new("sh")
                            .args(["-c", &format!("kill -TERM {pid}")])
                            .output();
                        match result {
                            Ok(o) if o.status.success() => {
                                eprintln!("Server stopped (pid {pid})");
                            }
                            Ok(o) => {
                                let stderr = String::from_utf8_lossy(&o.stderr);
                                if stderr.contains("No such process") {
                                    // Stale PID file — process already gone
                                    let _ = std::fs::remove_file(&pid_file);
                                    eprintln!("Warning: server process {pid} was not running (stale PID file removed)");
                                } else {
                                    eprintln!("Failed to stop server: {stderr}");
                                    std::process::exit(1);
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to send signal: {e}");
                                std::process::exit(1);
                            }
                        }
                    }
                    Err(_) => {
                        eprintln!("No PID file found at {}", pid_file.display());
                        std::process::exit(1);
                    }
                }
            }
        },
        Command::Session { action } => match action {
            SessionAction::List { status, id } => {
                let client = reqwest::Client::new();

                // If --id is provided, fetch the specific session
                if let Some(session_id) = id {
                    let session_uuid: Uuid = session_id.parse().unwrap_or_else(|_| {
                        eprintln!("Invalid session ID: {session_id}");
                        std::process::exit(1);
                    });
                    let url = format!("{}/sessions/{}", cli.server, session_uuid);
                    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
                        handle_connection_error(&e);
                    });
                    if !resp.status().is_success() {
                        if resp.status() == reqwest::StatusCode::NOT_FOUND {
                            eprintln!("Error: session not found: {session_uuid}");
                        } else {
                            eprintln!("Error: {}", resp.status());
                        }
                        std::process::exit(1);
                    }
                    let session: Session = resp.json().await.unwrap_or_else(|e| {
                        eprintln!("Error parsing response: {e}");
                        std::process::exit(1);
                    });
                    println!("{:<36}  {:<20}  {:<10}  created_at", "id", "name", "status");
                    println!(
                        "{:<36}  {:<20}  {:<10}  {}",
                        session.id, session.name, session.status, session.created_at
                    );
                } else {
                    // List all sessions with optional status filter
                    let mut url = format!("{}/sessions", cli.server);
                    if let Some(s) = &status {
                        url = format!("{url}?status={s}");
                    }
                    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
                        handle_connection_error(&e);
                    });
                    if !resp.status().is_success() {
                        eprintln!("Error: {}", resp.status());
                        std::process::exit(1);
                    }
                    let sessions: Vec<Session> = resp.json().await.unwrap_or_else(|e| {
                        eprintln!("Error parsing response: {e}");
                        std::process::exit(1);
                    });
                    if sessions.is_empty() {
                        println!("No sessions found.");
                    } else {
                        println!("{:<36}  {:<20}  {:<10}  created_at", "id", "name", "status");
                        for s in &sessions {
                            println!(
                                "{:<36}  {:<20}  {:<10}  {}",
                                s.id, s.name, s.status, s.created_at
                            );
                        }
                    }
                }
            }
            SessionAction::New { name, agent, message, wait } => {
                let url = format!("{}/sessions", cli.server);
                let body = json!({
                    "name": name,
                    "agent": agent,
                    "initial_message": message,
                });
                let client = reqwest::Client::new();
                let resp = client.post(&url).json(&body).send().await.unwrap_or_else(|e| {
                    handle_connection_error(&e);
                });
                if !resp.status().is_success() {
                    eprintln!("Error: {}", resp.status());
                    std::process::exit(1);
                }
                let session: Session = resp.json().await.unwrap_or_else(|e| {
                    eprintln!("Error parsing response: {e}");
                    std::process::exit(1);
                });
                eprintln!("Created session: {} ({})", session.name, session.id);
                println!("{}", session.id);

                if wait {
                    // last_turns=1: show only the final turn, not full history.
                    // Output goes to stderr so stdout stays UUID-only for scripting.
                    let events_url = format!("{}/sessions/{}/events?last_turns=1", cli.server, session.id);
                    stream_events(&events_url, true).await;
                }
            }
            SessionAction::Tail { id, name, turns } => {
                let session_id = resolve_session_id(&cli.server, id, name).await;
                let mut url = format!("{}/sessions/{}/events", cli.server, session_id);
                if let Some(n) = turns {
                    url = format!("{url}?last_turns={n}");
                }
                stream_events(&url, false).await;
            }
            SessionAction::Send { id, name, message } => {
                let session_id = resolve_session_id(&cli.server, id, name).await;
                let url = format!("{}/sessions/{}/messages", cli.server, session_id);
                let body = serde_json::json!({ "message": message });
                let client = reqwest::Client::new();
                let resp = client.post(&url).json(&body).send().await.unwrap_or_else(|e| {
                    handle_connection_error(&e);
                });
                if !resp.status().is_success() {
                    eprintln!("Error sending message: {}", resp.status());
                    std::process::exit(1);
                }
                eprintln!("Message sent.");
            }
            SessionAction::Stop { id, name } => {
                let session_id = resolve_session_id(&cli.server, id, name).await;
                // For MVP, just print "not implemented" — real cancellation is out of scope
                println!("Stop not yet implemented for session {session_id}");
            }
            SessionAction::Wait { ids } => {
                if ids.is_empty() {
                    eprintln!("Error: at least one --id is required");
                    std::process::exit(1);
                }
                let client = reqwest::Client::new();
                // Validate that all session IDs parse as valid UUIDs up-front.
                for id in &ids {
                    if id.parse::<Uuid>().is_err() {
                        eprintln!("Error: invalid session ID: {id}");
                        std::process::exit(1);
                    }
                }

                use std::io::Write;
                use std::collections::HashMap;

                let mut terminal_statuses: HashMap<String, types::SessionStatus> = HashMap::new();
                // Track snippet text per session id for the progress display.
                let mut snippets: HashMap<String, String> = HashMap::new();
                // Track session names fetched from the server.
                let mut names: HashMap<String, String> = HashMap::new();
                // lines_rendered tracks how many progress lines we've printed so we can
                // cursor-up to overwrite them on the next tick.
                let mut lines_rendered: usize = 0;
                let mut tick: usize = 0;

                loop {
                    let mut all_done = true;
                    for id in &ids {
                        if terminal_statuses.contains_key(id.as_str()) {
                            continue;
                        }
                        let url = format!("{}/sessions/{}", cli.server, id);
                        let resp = client.get(&url).send().await.unwrap_or_else(|e| {
                            handle_connection_error(&e);
                        });
                        if !resp.status().is_success() {
                            // Clear progress lines before printing the error.
                            if lines_rendered > 0 {
                                eprint!("\x1b[{}A\x1b[J", lines_rendered);
                            }
                            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                                eprintln!("Error: session not found: {id}");
                            } else {
                                print_error_response(resp).await;
                            }
                            std::process::exit(1);
                        }
                        let session: Session = resp.json().await.unwrap_or_else(|e| {
                            eprintln!("Error parsing response: {e}");
                            std::process::exit(1);
                        });
                        // Cache the name on first fetch.
                        names.entry(id.clone()).or_insert_with(|| session.name.clone());

                        if session_is_terminal(&session.status) {
                            terminal_statuses.insert(id.clone(), session.status);
                        } else {
                            all_done = false;
                            // Fetch snippet for running sessions.
                            let text_url = format!("{}/sessions/{}/last_text", cli.server, id);
                            if let Ok(text_resp) = client.get(&text_url).send().await {
                                if text_resp.status().is_success() {
                                    if let Ok(body) = text_resp.json::<serde_json::Value>().await {
                                        if let Some(t) = body.get("text").and_then(|v| v.as_str()) {
                                            snippets.insert(id.clone(), t.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Render progress lines to stderr.
                    // On subsequent ticks, move cursor up to overwrite previous lines.
                    {
                        let stderr = std::io::stderr();
                        let mut out = stderr.lock();
                        if lines_rendered > 0 {
                            // Move up `lines_rendered` lines and clear to end of screen.
                            write!(out, "\x1b[{}A\x1b[J", lines_rendered).ok();
                        }
                        let mut count = 0;
                        for id in &ids {
                            let status = terminal_statuses
                                .get(id.as_str())
                                .cloned()
                                .unwrap_or(types::SessionStatus::Running);
                            let name = names.get(id.as_str()).map(|s| s.as_str()).unwrap_or("");
                            let snippet = snippets.get(id.as_str()).map(|s| s.as_str());
                            let line = render_session_line(id, name, snippet, &status, tick);
                            writeln!(out, "{line}").ok();
                            count += 1;
                        }
                        lines_rendered = count;
                    }

                    if all_done {
                        break;
                    }
                    tick = tick.wrapping_add(1);
                    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
                }

                // Print final static lines to stdout: <uuid>  <status>
                // (The progress lines on stderr already show the final state.)
                let mut any_failed = false;
                for id in &ids {
                    let status = terminal_statuses.get(id.as_str()).expect("all ids terminal");
                    println!("{id}  {status}");
                    if *status == types::SessionStatus::Failed {
                        any_failed = true;
                    }
                }
                if any_failed {
                    std::process::exit(1);
                }
            }
        },
        Command::Agent { action } => match action {
            AgentAction::List => {
                let dir = agents::agents_dir().unwrap_or_else(|| {
                    eprintln!("Error: not inside a git repository");
                    std::process::exit(1);
                });
                if !dir.exists() {
                    println!("No agents found (directory does not exist: {})", dir.display());
                    return;
                }
                let agent_list = agents::list_agents(&dir);
                if agent_list.is_empty() {
                    println!("No agents found.");
                } else {
                    println!("{:<20} description", "name");
                    for a in &agent_list {
                        println!("{:<20} {}", a.name, a.description);
                    }
                }
            }
            AgentAction::New { name, description, body } => {
                let name = name.unwrap_or_else(|| {
                    eprintln!("Error: --name is required");
                    std::process::exit(1);
                });
                let dir = agents::agents_dir().unwrap_or_else(|| {
                    eprintln!("Error: not inside a git repository");
                    std::process::exit(1);
                });
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    eprintln!("Error creating agents directory: {e}");
                    std::process::exit(1);
                }
                let path = dir.join(format!("{name}.md"));
                if path.exists() {
                    eprintln!("Error: agent '{name}' already exists at {}", path.display());
                    std::process::exit(1);
                }
                let open_editor = body.is_none();
                let def = agents::AgentDef {
                    name: name.clone(),
                    description: description.unwrap_or_default(),
                    body: body.unwrap_or_default(),
                    include_project_config: false,
                    hooks: agents::AgentHooks::default(),
                };
                if let Err(e) = agents::write_agent(&dir, &def) {
                    eprintln!("Error writing agent file: {e}");
                    std::process::exit(1);
                }
                if open_editor {
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
                    std::process::Command::new(&editor).arg(&path).status().ok();
                }
                eprintln!("Created agent '{name}' at {}", path.display());
            }
            AgentAction::Edit { name, description, body } => {
                let name = name.unwrap_or_else(|| {
                    eprintln!("Error: --name is required");
                    std::process::exit(1);
                });
                if description.is_none() && body.is_none() {
                    eprintln!("Error: at least one of --description or --body must be provided");
                    std::process::exit(1);
                }
                let dir = agents::agents_dir().unwrap_or_else(|| {
                    eprintln!("Error: not inside a git repository");
                    std::process::exit(1);
                });
                let mut def = agents::load_agent(&dir, &name).unwrap_or_else(|| {
                    eprintln!(
                        "Error: agent '{name}' not found at {}",
                        dir.join(format!("{name}.md")).display()
                    );
                    std::process::exit(1);
                });
                if let Some(d) = description {
                    def.description = d;
                }
                if let Some(b) = body {
                    def.body = b;
                }
                if let Err(e) = agents::write_agent(&dir, &def) {
                    eprintln!("Error writing agent file: {e}");
                    std::process::exit(1);
                }
                eprintln!("Updated agent '{name}'.");
            }
        },
        Command::Spec { action } => match action {
            SpecAction::New { path, targets, severity } => {
                if targets.is_empty() {
                    eprintln!("Error: at least one --target is required");
                    std::process::exit(1);
                }
                let severity = match severity.as_str() {
                    "warning" => specs::Severity::Warning,
                    "error" => specs::Severity::Error,
                    _ => {
                        eprintln!("Error: --severity must be 'error' or 'warning'");
                        std::process::exit(1);
                    }
                };
                let git_root = workspace::git_root_sync().unwrap_or_else(|| {
                    eprintln!("Error: not inside a git repository");
                    std::process::exit(1);
                });
                let resolved = if PathBuf::from(&path).is_absolute() {
                    PathBuf::from(&path)
                } else {
                    git_root.join(&path)
                };
                let path_display = path.clone();
                let path = resolved;
                if path.exists() {
                    eprintln!("Error: spec already exists at {}", path.display());
                    std::process::exit(1);
                }
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            eprintln!("Error creating directories: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                let def = specs::SpecDef { targets, verified: None, severity, body: String::new() };
                if let Err(e) = specs::write_spec(&path, &def) {
                    eprintln!("Error writing spec file: {e}");
                    std::process::exit(1);
                }
                println!("Created spec at {path_display}");
            }
            SpecAction::Sync { path, error_on_warnings } => {
                let git_root = workspace::git_root_sync().unwrap_or_else(|| {
                    eprintln!("Error: not inside a git repository");
                    std::process::exit(1);
                });
                if let Some(p) = path {
                    let resolved = if PathBuf::from(&p).is_absolute() {
                        PathBuf::from(&p)
                    } else {
                        git_root.join(&p)
                    };
                    if resolved.is_dir() {
                        let all_specs = specs::list_specs(&resolved);
                        let mut has_errors = false;
                        for (spec_path, def) in &all_specs {
                            let stale = specs::stale_files(&git_root, spec_path, def);
                            if !stale.is_empty() {
                                let display_path = spec_path
                                    .strip_prefix(&git_root)
                                    .unwrap_or(spec_path)
                                    .display()
                                    .to_string();
                                let is_error =
                                    def.severity == specs::Severity::Error || error_on_warnings;
                                if is_error {
                                    eprint!("{}", format_sync_error(&display_path, &stale));
                                    has_errors = true;
                                } else {
                                    eprint!("{}", format_sync_warning(&display_path, &stale));
                                }
                            }
                        }
                        if has_errors {
                            std::process::exit(1);
                        }
                    } else {
                        let def = specs::load_spec(&resolved).unwrap_or_else(|| {
                            eprintln!("Error: could not load spec at {p}");
                            std::process::exit(1);
                        });
                        let stale = specs::stale_files(&git_root, &resolved, &def);
                        if !stale.is_empty() {
                            let is_error =
                                def.severity == specs::Severity::Error || error_on_warnings;
                            if is_error {
                                eprint!("{}", format_sync_error(&p, &stale));
                                std::process::exit(1);
                            } else {
                                eprint!("{}", format_sync_warning(&p, &stale));
                            }
                        }
                    }
                } else {
                    let all_specs = specs::list_specs(&git_root);
                    let mut has_errors = false;
                    for (spec_path, def) in &all_specs {
                        let stale = specs::stale_files(&git_root, spec_path, def);
                        if !stale.is_empty() {
                            let display_path = spec_path
                                .strip_prefix(&git_root)
                                .unwrap_or(spec_path)
                                .display()
                                .to_string();
                            let is_error =
                                def.severity == specs::Severity::Error || error_on_warnings;
                            if is_error {
                                eprint!("{}", format_sync_error(&display_path, &stale));
                                has_errors = true;
                            } else {
                                eprint!("{}", format_sync_warning(&display_path, &stale));
                            }
                        }
                    }
                    if has_errors {
                        std::process::exit(1);
                    }
                }
            }
            SpecAction::Verify { paths } => {
                let git_root = workspace::git_root_sync().unwrap_or_else(|| {
                    eprintln!("Error: not inside a git repository");
                    std::process::exit(1);
                });
                let result = verify_spec_paths(&git_root, &paths);
                for line in &result.stdout_lines {
                    println!("{line}");
                }
                for line in &result.stderr_lines {
                    eprintln!("{line}");
                }
                if result.any_failed {
                    std::process::exit(1);
                }
            }
        },
        Command::Issue { action } => {
            let client = reqwest::Client::new();
            match action {
                IssueAction::New { title, body, assignee, parent, blocked_on, start, branch } => {
                    if start && assignee.is_none() {
                        eprintln!("Error: --start requires --assignee (start needs an agent to run)");
                        std::process::exit(1);
                    }
                    if let Some(ref a) = assignee {
                        if let Some(dir) = agents::agents_dir() {
                            if agents::load_agent(&dir, a).is_none() {
                                eprintln!("Error: agent type '{a}' not found in .ns2/agents/");
                                std::process::exit(1);
                            }
                        }
                    }
                    let url = format!("{}/issues", cli.server);
                    let req_body = json!({
                        "title": title,
                        "body": body,
                        "assignee": assignee,
                        "parent_id": parent,
                        "blocked_on": blocked_on,
                        "branch": branch,
                    });
                    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
                        handle_connection_error(&e);
                    });
                    if !resp.status().is_success() {
                        print_error_response(resp).await;
                    }
                    let issue: Issue = resp.json().await.unwrap_or_else(|e| {
                        eprintln!("Error parsing response: {e}");
                        std::process::exit(1);
                    });
                    eprintln!("Created issue: {} ({})", issue.title, issue.id);
                    println!("{}", issue.id);

                    if start {
                        let start_url = format!("{}/issues/{}/start", cli.server, issue.id);
                        let start_resp = client.post(&start_url).send().await.unwrap_or_else(|e| {
                            handle_connection_error(&e);
                        });
                        if !start_resp.status().is_success() {
                            if start_resp.status() == reqwest::StatusCode::NOT_FOUND {
                                eprintln!("Error: issue not found: {}", issue.id);
                                std::process::exit(1);
                            }
                            print_error_response(start_resp).await;
                        }
                        let started: Issue = start_resp.json().await.unwrap_or_else(|e| {
                            eprintln!("Error parsing response: {e}");
                            std::process::exit(1);
                        });
                        eprintln!("Started issue {}. Session: {}", started.id, started.session_id.map(|id| id.to_string()).unwrap_or_default());
                    }
                }
                IssueAction::Edit { id, title, body, assignee, parent, blocked_on, branch } => {
                    let url = format!("{}/issues/{}", cli.server, id);
                    let mut req_body = serde_json::Map::new();
                    if let Some(t) = title {
                        req_body.insert("title".into(), json!(t));
                    }
                    if let Some(b) = body {
                        req_body.insert("body".into(), json!(b));
                    }
                    if let Some(a) = assignee {
                        if a.is_empty() {
                            req_body.insert("assignee".into(), json!(null));
                        } else {
                            if let Some(dir) = agents::agents_dir() {
                                if agents::load_agent(&dir, &a).is_none() {
                                    eprintln!("Error: agent type '{a}' not found in .ns2/agents/");
                                    std::process::exit(1);
                                }
                            }
                            req_body.insert("assignee".into(), json!(a));
                        }
                    }
                    if let Some(p) = parent {
                        if p.is_empty() {
                            req_body.insert("parent_id".into(), json!(null));
                        } else {
                            req_body.insert("parent_id".into(), json!(p));
                        }
                    }
                    if let Some(bo) = blocked_on {
                        req_body.insert("blocked_on".into(), json!(bo));
                    }
                    if let Some(br) = branch {
                        req_body.insert("branch".into(), json!(br));
                    }
                    let resp = client
                        .patch(&url)
                        .json(&serde_json::Value::Object(req_body))
                        .send()
                        .await
                        .unwrap_or_else(|e| handle_connection_error(&e));
                    if !resp.status().is_success() {
                        if resp.status() == reqwest::StatusCode::NOT_FOUND {
                            eprintln!("Error: issue not found: {id}");
                            std::process::exit(1);
                        }
                        print_error_response(resp).await;
                    }
                    let issue: Issue = resp.json().await.unwrap_or_else(|e| {
                        eprintln!("Error parsing response: {e}");
                        std::process::exit(1);
                    });
                    eprintln!("Updated issue {}.", issue.id);
                }
                IssueAction::Comment { id, body, author } => {
                    let url = format!("{}/issues/{}/comments", cli.server, id);
                    let req_body = json!({ "author": author, "body": body });
                    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
                        handle_connection_error(&e);
                    });
                    if !resp.status().is_success() {
                        if resp.status() == reqwest::StatusCode::NOT_FOUND {
                            eprintln!("Error: issue not found: {id}");
                            std::process::exit(1);
                        }
                        print_error_response(resp).await;
                    }
                    eprintln!("Comment added to issue {id}.");
                }
                IssueAction::Start { id } => {
                    let url = format!("{}/issues/{}/start", cli.server, id);
                    let resp = client.post(&url).send().await.unwrap_or_else(|e| {
                        handle_connection_error(&e);
                    });
                    if !resp.status().is_success() {
                        if resp.status() == reqwest::StatusCode::NOT_FOUND {
                            eprintln!("Error: issue not found: {id}");
                            std::process::exit(1);
                        }
                        print_error_response(resp).await;
                    }
                    let issue: Issue = resp.json().await.unwrap_or_else(|e| {
                        eprintln!("Error parsing response: {e}");
                        std::process::exit(1);
                    });
                    eprintln!("Started issue {id}. Session: {}", issue.session_id.map(|id| id.to_string()).unwrap_or_default());
                }
                IssueAction::Complete { id, comment } => {
                    let url = format!("{}/issues/{}/complete", cli.server, id);
                    let req_body = json!({ "comment": comment });
                    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
                        handle_connection_error(&e);
                    });
                    if !resp.status().is_success() {
                        if resp.status() == reqwest::StatusCode::NOT_FOUND {
                            eprintln!("Error: issue not found: {id}");
                            std::process::exit(1);
                        }
                        print_error_response(resp).await;
                    }
                    eprintln!("Issue {id} marked as completed.");
                }
                IssueAction::Reopen { id, comment, start } => {
                    let url = format!("{}/issues/{}/reopen", cli.server, id);
                    let req_body = json!({ "comment": comment });
                    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
                        handle_connection_error(&e);
                    });
                    if resp.status() == reqwest::StatusCode::NOT_FOUND {
                        eprintln!("Error: issue not found: {id}");
                        std::process::exit(1);
                    }
                    if !resp.status().is_success() {
                        print_error_response(resp).await;
                    }
                    eprintln!("Issue {id} reopened.");

                    if start {
                        let start_url = format!("{}/issues/{}/start", cli.server, id);
                        let start_resp = client.post(&start_url).send().await.unwrap_or_else(|e| {
                            handle_connection_error(&e);
                        });
                        if !start_resp.status().is_success() {
                            if start_resp.status() == reqwest::StatusCode::NOT_FOUND {
                                eprintln!("Error: issue not found: {id}");
                                std::process::exit(1);
                            }
                            print_error_response(start_resp).await;
                        }
                        let started: Issue = start_resp.json().await.unwrap_or_else(|e| {
                            eprintln!("Error parsing response: {e}");
                            std::process::exit(1);
                        });
                        eprintln!("Started issue {id}. Session: {}", started.session_id.map(|id| id.to_string()).unwrap_or_default());
                    }
                }
                IssueAction::List { status, assignee, parent, blocked_on } => {
                    let mut url = format!("{}/issues", cli.server);
                    let mut params: Vec<String> = vec![];
                    if let Some(s) = &status {
                        params.push(format!("status={s}"));
                    }
                    if let Some(a) = &assignee {
                        params.push(format!("assignee={a}"));
                    }
                    if let Some(p) = &parent {
                        params.push(format!("parent_id={p}"));
                    }
                    if let Some(bo) = &blocked_on {
                        params.push(format!("blocked_on={bo}"));
                    }
                    if !params.is_empty() {
                        url = format!("{url}?{}", params.join("&"));
                    }
                    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
                        handle_connection_error(&e);
                    });
                    if !resp.status().is_success() {
                        eprintln!("Error: {}", resp.status());
                        std::process::exit(1);
                    }
                    let issues: Vec<Issue> = resp.json().await.unwrap_or_else(|e| {
                        eprintln!("Error parsing response: {e}");
                        std::process::exit(1);
                    });
                    if issues.is_empty() {
                        println!("No issues found.");
                    } else {
                        println!("{:<6}  {:<30}  {:<10}  {:<12}  {:<25}  created_at", "id", "title", "status", "assignee", "branch");
                        for issue in &issues {
                            print_issue_row(issue);
                        }
                    }
                }
                IssueAction::Wait { ids } => {
                    if ids.is_empty() {
                        eprintln!("Error: at least one --id is required");
                        std::process::exit(1);
                    }

                    // Helper: fetch an issue tree rooted at `id` recursively.
                    async fn fetch_issue_tree(
                        client: &reqwest::Client,
                        server: &str,
                        id: &str,
                    ) -> Option<IssueTreeNode> {
                        let url = format!("{}/issues/{}", server, id);
                        let resp = client.get(&url).send().await.ok()?;
                        if !resp.status().is_success() {
                            return None;
                        }
                        let issue: types::Issue = resp.json().await.ok()?;

                        // Fetch children (issues with this parent_id)
                        let children_url = format!("{}/issues?parent_id={}", server, id);
                        let children_resp = client.get(&children_url).send().await.ok()?;
                        let child_issues: Vec<types::Issue> = if children_resp.status().is_success() {
                            children_resp.json().await.unwrap_or_default()
                        } else {
                            vec![]
                        };

                        // Recursively fetch children
                        let mut children = Vec::new();
                        for child in &child_issues {
                            // Use Box::pin to handle recursive async
                            if let Some(node) = Box::pin(fetch_issue_tree(client, server, &child.id)).await {
                                children.push(node);
                            }
                        }

                        Some(IssueTreeNode { issue, snippet: None, children })
                    }

                    // Helper: fetch last text snippet for a running issue with a session.
                    async fn fetch_snippet(
                        client: &reqwest::Client,
                        server: &str,
                        session_id: uuid::Uuid,
                    ) -> Option<String> {
                        let url = format!("{}/sessions/{}/last_text", server, session_id);
                        let resp = client.get(&url).send().await.ok()?;
                        if !resp.status().is_success() {
                            return None;
                        }
                        let v: serde_json::Value = resp.json().await.ok()?;
                        v["text"].as_str().map(|s| s.to_string())
                    }

                    // Recursively attach snippets to running nodes.
                    fn attach_snippets(
                        node: &mut IssueTreeNode,
                        snippets: &std::collections::HashMap<uuid::Uuid, Option<String>>,
                    ) {
                        if node.issue.status == types::IssueStatus::Running {
                            if let Some(session_id) = node.issue.session_id {
                                if let Some(snippet_opt) = snippets.get(&session_id) {
                                    node.snippet = snippet_opt.clone();
                                }
                            }
                        }
                        for child in &mut node.children {
                            attach_snippets(child, snippets);
                        }
                    }

                    // Collect all running session IDs from a tree.
                    fn collect_running_sessions(node: &IssueTreeNode, out: &mut Vec<uuid::Uuid>) {
                        if node.issue.status == types::IssueStatus::Running {
                            if let Some(sid) = node.issue.session_id {
                                out.push(sid);
                            }
                        }
                        for child in &node.children {
                            collect_running_sessions(child, out);
                        }
                    }

                    use std::io::Write;

                    let mut tick: usize = 0;
                    let mut prev_line_count = 0usize;
                    let mut any_failed = false;
                    let mut final_statuses: Vec<(String, types::IssueStatus)> = Vec::new();

                    loop {
                        // Fetch trees for all root IDs
                        let mut roots: Vec<IssueTreeNode> = Vec::new();
                        let mut fetch_error = false;
                        for id in &ids {
                            match fetch_issue_tree(&client, &cli.server, id).await {
                                Some(node) => roots.push(node),
                                None => {
                                    eprintln!("Error: issue not found: {id}");
                                    fetch_error = true;
                                }
                            }
                        }
                        if fetch_error {
                            std::process::exit(1);
                        }

                        // Collect running sessions and fetch snippets
                        let mut session_ids: Vec<uuid::Uuid> = Vec::new();
                        for root in &roots {
                            collect_running_sessions(root, &mut session_ids);
                        }
                        let mut snippets: std::collections::HashMap<uuid::Uuid, Option<String>> =
                            std::collections::HashMap::new();
                        for sid in &session_ids {
                            let snippet = fetch_snippet(&client, &cli.server, *sid).await;
                            snippets.insert(*sid, snippet);
                        }

                        // Attach snippets to running nodes
                        for root in &mut roots {
                            attach_snippets(root, &snippets);
                        }

                        // Render the tree
                        let lines = render_issue_tree(&roots, tick);

                        // Clear previous frame (cursor-up + clear line for each previous line)
                        let stderr = std::io::stderr();
                        let mut out = stderr.lock();
                        for _ in 0..prev_line_count {
                            write!(out, "\x1b[1A\x1b[2K").ok();
                        }

                        // Write new frame
                        for line in &lines {
                            writeln!(out, "{}", line).ok();
                        }
                        out.flush().ok();
                        prev_line_count = lines.len();

                        // Check if all done
                        if all_nodes_terminal(&roots) {
                            for root in &roots {
                                if root.issue.status == types::IssueStatus::Failed {
                                    any_failed = true;
                                }
                                final_statuses.push((root.issue.id.clone(), root.issue.status.clone()));
                            }
                            break;
                        }

                        tick = tick.wrapping_add(1);
                        tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
                    }

                    // Print stdout contract: one line per waited issue
                    for (id, status) in &final_statuses {
                        println!("{id}  {status}");
                    }

                    if any_failed {
                        eprintln!("One or more issues failed.");
                        std::process::exit(1);
                    }
                }
            }
        }
        Command::Worktree { action } => {
            let git_root = workspace::git_root_sync().unwrap_or_else(|| {
                eprintln!("Error: not inside a git repository");
                std::process::exit(1);
            });
            let config = workspace::read_ns2_config(&git_root);

            match action {
                WorktreeAction::List => {
                    let entries = workspace::list_worktrees(&git_root, &config.worktree_base).await;
                    if entries.is_empty() {
                        println!("No worktrees found.");
                    } else {
                        println!("{:<40}  path", "branch");
                        for entry in &entries {
                            println!(
                                "{:<40}  {}",
                                entry.branch,
                                entry.path.display()
                            );
                        }
                    }
                }
                WorktreeAction::Create { branch } => {
                    let worktree_path = config.worktree_base.join(&branch);
                    match workspace::ensure_worktree(&git_root, &worktree_path, &branch).await {
                        Some(path) => {
                            eprintln!(
                                "Created worktree for branch {} at {}",
                                branch,
                                path.display()
                            );
                        }
                        None => {
                            eprintln!("Error: failed to create worktree for branch {branch}");
                            std::process::exit(1);
                        }
                    }
                }
                WorktreeAction::Delete { branch, force } => {
                    match workspace::delete_worktree(
                        &git_root,
                        &config.worktree_base,
                        &branch,
                        force,
                    ).await {
                        Ok(_path) => {
                            eprintln!("Deleted worktree for branch {branch}");
                        }
                        Err(workspace::DeleteWorktreeError::NotFound(_)) => {
                            eprintln!("Error: no worktree found for branch {branch}");
                            std::process::exit(1);
                        }
                        Err(workspace::DeleteWorktreeError::UnmergedCommits(_)) => {
                            eprintln!(
                                "Error: branch {branch} has unmerged commits. Use --force to delete anyway."
                            );
                            std::process::exit(1);
                        }
                        Err(workspace::DeleteWorktreeError::GitFailed(msg)) => {
                            eprintln!("Error: {msg}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::*;
    use uuid::Uuid;

    #[test]
    fn issue_is_terminal_completed_is_true() {
        assert!(issue_is_terminal(&types::IssueStatus::Completed));
    }

    #[test]
    fn issue_is_terminal_failed_is_true() {
        assert!(issue_is_terminal(&types::IssueStatus::Failed));
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
        let event = types::SessionEvent::Error { message: "something went wrong".into() };
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
            Command::Session { action: SessionAction::Wait { ids } } => {
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
            "rendered tree line must not contain literal newline, got: {:?}",
            line
        );
        // The snippet content should appear with a space substituted for the newline
        assert!(
            line.contains("line one line two"),
            "newline in snippet should be replaced with space, got: {:?}",
            line
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
            "render_session_line must not embed carriage return in output, got: {:?}",
            line
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
            ["a.spec.md", "b.spec.md", "c.spec.md"].iter().map(|s| s.to_string()).collect();
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
            .map(|s| s.to_string())
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
}
