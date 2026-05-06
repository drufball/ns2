use crate::render::{parse_sse_frames, print_session_event};
use uuid::Uuid;

pub fn handle_connection_error(err: &reqwest::Error) -> ! {
    if err.is_connect() {
        eprintln!(
            "Error: server is not running (connection refused). Start it with: ns2 server start"
        );
    } else {
        eprintln!("Error: {err}");
    }
    std::process::exit(1);
}

pub async fn print_error_response(resp: reqwest::Response) -> ! {
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

pub async fn stream_events(url: &str, to_stderr: bool) {
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
                    // Try to parse as SystemEvent first (new /events endpoint).
                    // Fall back to SessionEvent (legacy, kept for backward compat).
                    let session_event: Option<events::SessionEvent> =
                        serde_json::from_str::<events::SystemEvent>(data).map_or_else(
                            |_| {
                                // Fallback: try old SessionEvent format
                                serde_json::from_str::<events::SessionEvent>(data).ok()
                            },
                            |ev| match ev {
                                events::SystemEvent::Session { event, .. } => Some(event),
                                _ => None,
                            },
                        );

                    if let Some(event) = session_event {
                        print_session_event(&event, to_stderr);
                        if matches!(event, events::SessionEvent::Done) {
                            return;
                        }
                        if matches!(event, events::SessionEvent::Error { .. }) {
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
    }
}

pub async fn resolve_session_id(server: &str, id: Option<String>, name: Option<String>) -> Uuid {
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
        sessions.into_iter().find(|s| s.name == name).map_or_else(
            || {
                eprintln!("Session not found: {name}");
                std::process::exit(1);
            },
            |s| s.id,
        )
    } else {
        eprintln!("Must provide --id or --name");
        std::process::exit(1);
    }
}
