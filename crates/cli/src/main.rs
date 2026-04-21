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

fn git_root() -> Option<PathBuf> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|p| PathBuf::from(p.trim()))
}

fn load_dotenv() {
    let Some(root) = git_root() else { return };
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

fn data_dir_and_pid(port: u16) -> (PathBuf, PathBuf) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let repo_name = git_root()
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
                            None => Arc::new(harness::StubClient),
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
                        let result = std::process::Command::new("kill")
                            .args(["-TERM", &pid])
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
                        buffer.push_str(s);
                    }
                    while let Some(pos) = buffer.find("\n\n") {
                        let line = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use types::*;

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
}
