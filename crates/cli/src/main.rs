use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use types::Session;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "ns2")]
struct Cli {
    #[arg(long, default_value = "http://localhost:9876")]
    server: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    Spec {
        #[command(subcommand)]
        action: SpecAction,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    List,
    New {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        body: Option<String>,
    },
    Edit {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        body: Option<String>,
    },
}

#[derive(Subcommand)]
enum ServerAction {
    Start {
        #[arg(long, default_value_t = 9876)]
        port: u16,
    },
    Stop,
}

#[derive(Subcommand)]
enum SessionAction {
    List {
        #[arg(long)]
        status: Option<String>,
    },
    New {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        message: Option<String>,
    },
    Tail {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    Send {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        message: String,
    },
    Stop {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum SpecAction {
    New {
        path: String,
        #[arg(long = "target", num_args = 1..)]
        targets: Vec<String>,
    },
    Sync {
        path: Option<String>,
    },
    Verify {
        path: String,
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

fn handle_connection_error(err: &reqwest::Error) -> ! {
    if err.is_connect() {
        eprintln!("Error: server is not running (connection refused). Start it with: ns2 server start");
    } else {
        eprintln!("Error: {err}");
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

fn print_session_event(event: &types::SessionEvent) {
    use types::SessionEvent::*;
    match event {
        // Text deltas stream without a newline; flush so the terminal shows them immediately.
        ContentBlockDelta {
            delta: types::ContentBlockDelta::TextDelta { .. },
            ..
        } => {
            if let Some(text) = format_session_event(event) {
                print!("{text}");
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
        }
        // Errors go to stderr.
        Error { message } => eprintln!("[error] {message}"),
        // Everything else: print the formatted string (which already includes a newline).
        _ => {
            if let Some(output) = format_session_event(event) {
                print!("{output}");
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
    let mut out = format!("spec {spec_path} has stale files:\n");
    for f in stale {
        out.push_str(&format!("  {}\n", f.display()));
    }
    out
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
                        .unwrap_or_else(|_| "claude-opus-4-5".to_string()),
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
            SessionAction::List { status } => {
                let mut url = format!("{}/sessions", cli.server);
                if let Some(s) = &status {
                    url = format!("{url}?status={s}");
                }
                let client = reqwest::Client::new();
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
            SessionAction::New { name, agent, message } => {
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
            }
            SessionAction::Tail { id, name } => {
                let session_id = resolve_session_id(&cli.server, id, name).await;
                let url = format!("{}/sessions/{}/events", cli.server, session_id);

                let client = reqwest::Client::new();
                let resp = client
                    .get(&url)
                    .header("Accept", "text/event-stream")
                    .send()
                    .await
                    .unwrap_or_else(|e| handle_connection_error(&e));

                use futures::StreamExt;
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
                                if let Ok(event) =
                                    serde_json::from_str::<types::SessionEvent>(data)
                                {
                                    print_session_event(&event);
                                    // Exit on both SessionDone (success) and Error (failure) — never hang on a failed session.
                                    if matches!(event, types::SessionEvent::SessionDone { .. } | types::SessionEvent::Error { .. }) {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
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
                    println!("{:<20}  description", "name");
                    for a in &agent_list {
                        println!("{:<20}  {}", a.name, a.description);
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
                    eprintln!("Error: must provide at least one of --description or --body");
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
            SpecAction::New { path, targets } => {
                if targets.is_empty() {
                    eprintln!("Error: at least one --target is required");
                    std::process::exit(1);
                }
                let path = PathBuf::from(&path);
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
                let def = specs::SpecDef { targets, verified: None, body: String::new() };
                if let Err(e) = specs::write_spec(&path, &def) {
                    eprintln!("Error writing spec file: {e}");
                    std::process::exit(1);
                }
                println!("Created spec at {}", path.display());
            }
            SpecAction::Sync { path } => {
                let git_root = workspace::git_root().unwrap_or_else(|| {
                    eprintln!("Error: not inside a git repository");
                    std::process::exit(1);
                });
                if let Some(p) = path {
                    let def = specs::load_spec(std::path::Path::new(&p)).unwrap_or_else(|| {
                        eprintln!("Error: could not load spec at {p}");
                        std::process::exit(1);
                    });
                    let stale = specs::stale_files(&git_root, &def);
                    if !stale.is_empty() {
                        eprint!("{}", format_sync_error(&p, &stale));
                        std::process::exit(1);
                    }
                } else {
                    let all_specs = specs::list_specs(&git_root);
                    let mut any_stale = false;
                    for (spec_path, def) in &all_specs {
                        let stale = specs::stale_files(&git_root, def);
                        if !stale.is_empty() {
                            eprint!("{}", format_sync_error(&spec_path.display().to_string(), &stale));
                            any_stale = true;
                        }
                    }
                    if any_stale {
                        std::process::exit(1);
                    }
                }
            }
            SpecAction::Verify { path } => {
                let mut def = specs::load_spec(std::path::Path::new(&path)).unwrap_or_else(|| {
                    eprintln!("Error: could not load spec at {path}");
                    std::process::exit(1);
                });
                def.verified = Some(chrono::Utc::now());
                if let Err(e) = specs::write_spec(std::path::Path::new(&path), &def) {
                    eprintln!("Error writing spec file: {e}");
                    std::process::exit(1);
                }
                println!("Verified {path}");
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::*;
    use uuid::Uuid;

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
}
