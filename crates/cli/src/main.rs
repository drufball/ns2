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
    #[command(about = "Mark a spec as verified at the current time.", long_about = "Mark a spec as verified at the current time.\n\nWrites the current UTC timestamp into the `verified` frontmatter field. Run this after reviewing or updating a spec's targets to confirm the spec is in sync with the code. The body and targets are preserved.")]
    Verify {
        #[arg(help = "The spec file to verify. Required — you must verify specs one at a time.")]
        path: String,
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
    #[command(about = "Create an agent session for this issue and start it.", long_about = "Creates a new session using the issue's assignee agent, sends the issue title and body as the opening message, and links the session to the issue. Sets the issue status to 'running'.")]
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

// ────────────────────────────────────────────────────────────────────────────

fn load_dotenv() {
    let Some(root) = workspace::git_root() else { return };
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
    let repo_name = workspace::git_root()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "default".to_string());

    let data_dir = PathBuf::from(&home).join(".ns2").join(&repo_name);
    let pid_file = data_dir.join(format!("server-{port}.pid"));
    (data_dir, pid_file)
}

fn print_issue_row(issue: &Issue) {
    println!(
        "{:<6}  {:<30}  {:<10}  {:<12}  {}",
        issue.id,
        if issue.title.len() > 30 { &issue.title[..30] } else { &issue.title },
        issue.status.to_string(),
        issue.assignee.as_deref().unwrap_or("-"),
        issue.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
    );
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

pub fn session_is_terminal(status: &types::SessionStatus) -> bool {
    matches!(
        status,
        types::SessionStatus::Completed | types::SessionStatus::Failed | types::SessionStatus::Cancelled
    )
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
                        Arc::new(tools::ReadTool),
                        Arc::new(tools::BashTool),
                        Arc::new(tools::WriteTool),
                        Arc::new(tools::EditTool),
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
                let mut terminal_statuses: std::collections::HashMap<String, types::SessionStatus> =
                    std::collections::HashMap::new();
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
                        if session_is_terminal(&session.status) {
                            terminal_statuses.insert(id.clone(), session.status);
                        } else {
                            all_done = false;
                        }
                    }
                    if all_done {
                        break;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
                // Print one line per session: <uuid>  <status>
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
                let git_root = workspace::git_root().unwrap_or_else(|| {
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
                let git_root = workspace::git_root().unwrap_or_else(|| {
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
            SpecAction::Verify { path } => {
                let git_root = workspace::git_root().unwrap_or_else(|| {
                    eprintln!("Error: not inside a git repository");
                    std::process::exit(1);
                });
                let resolved = if PathBuf::from(&path).is_absolute() {
                    PathBuf::from(&path)
                } else {
                    git_root.join(&path)
                };
                let mut def = specs::load_spec(&resolved).unwrap_or_else(|| {
                    eprintln!("Error: could not load spec at {path}");
                    std::process::exit(1);
                });
                def.verified = Some(chrono::Utc::now());
                if let Err(e) = specs::write_spec(&resolved, &def) {
                    eprintln!("Error writing spec file: {e}");
                    std::process::exit(1);
                }
                println!("Verified {path}");
            }
        },
        Command::Issue { action } => {
            let client = reqwest::Client::new();
            match action {
                IssueAction::New { title, body, assignee, parent, blocked_on } => {
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
                }
                IssueAction::Edit { id, title, body, assignee, parent, blocked_on } => {
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
                        println!("{:<6}  {:<30}  {:<10}  {:<12}  created_at", "id", "title", "status", "assignee");
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
                    let mut any_failed = false;
                    loop {
                        let mut all_done = true;
                        for id in &ids {
                            let url = format!("{}/issues/{}", cli.server, id);
                            let resp = client.get(&url).send().await.unwrap_or_else(|e| {
                                handle_connection_error(&e);
                            });
                            if !resp.status().is_success() {
                                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                                    eprintln!("Error: issue not found: {id}");
                                } else {
                                    print_error_response(resp).await;
                                }
                                std::process::exit(1);
                            }
                            let issue: Issue = resp.json().await.unwrap_or_else(|e| {
                                eprintln!("Error parsing response: {e}");
                                std::process::exit(1);
                            });
                            if issue_is_terminal(&issue.status) {
                                if issue.status == types::IssueStatus::Failed {
                                    any_failed = true;
                                }
                            } else {
                                all_done = false;
                            }
                        }
                        if all_done {
                            break;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    if any_failed {
                        eprintln!("One or more issues failed.");
                        std::process::exit(1);
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
}
