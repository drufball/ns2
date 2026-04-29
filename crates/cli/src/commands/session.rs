use std::collections::HashMap;
use types::{Session, SessionStatus};
use uuid::Uuid;
use crate::client::{handle_connection_error, print_error_response, resolve_session_id, stream_events};
use crate::render::{render_session_line};

pub(crate) fn session_is_terminal(status: &SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Completed | SessionStatus::Failed | SessionStatus::Cancelled
    )
}

pub(crate) async fn run_list(server: &str, status: Option<String>, id: Option<String>) {
    let client = reqwest::Client::new();

    // If --id is provided, fetch the specific session
    if let Some(session_id) = id {
        let session_uuid: Uuid = session_id.parse().unwrap_or_else(|_| {
            eprintln!("Invalid session ID: {session_id}");
            std::process::exit(1);
        });
        let url = format!("{}/sessions/{}", server, session_uuid);
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
        let mut url = format!("{}/sessions", server);
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

pub(crate) async fn run_new(
    server: &str,
    name: Option<String>,
    agent: Option<String>,
    message: Option<String>,
    wait: bool,
) {
    let url = format!("{}/sessions", server);
    let body = serde_json::json!({
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
        let events_url = format!("{}/sessions/{}/events?last_turns=1", server, session.id);
        stream_events(&events_url, true).await;
    }
}

pub(crate) async fn run_tail(server: &str, id: Option<String>, name: Option<String>, turns: Option<usize>) {
    let session_id = resolve_session_id(server, id, name).await;
    let mut url = format!("{}/sessions/{}/events", server, session_id);
    if let Some(n) = turns {
        url = format!("{url}?last_turns={n}");
    }
    stream_events(&url, false).await;
}

pub(crate) async fn run_send(server: &str, id: Option<String>, name: Option<String>, message: String) {
    let session_id = resolve_session_id(server, id, name).await;
    let url = format!("{}/sessions/{}/messages", server, session_id);
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

pub(crate) async fn run_stop(server: &str, id: Option<String>, name: Option<String>) {
    let session_id = resolve_session_id(server, id, name).await;
    // For MVP, just print "not implemented" — real cancellation is out of scope
    println!("Stop not yet implemented for session {session_id}");
}

pub(crate) async fn run_wait(server: &str, ids: Vec<String>) {
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

    let mut terminal_statuses: HashMap<String, SessionStatus> = HashMap::new();
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
            let url = format!("{}/sessions/{}", server, id);
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
                let text_url = format!("{}/sessions/{}/last_text", server, id);
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
                    .unwrap_or(SessionStatus::Running);
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
        if *status == SessionStatus::Failed {
            any_failed = true;
        }
    }
    if any_failed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::SessionStatus;

    #[test]
    fn session_is_terminal_completed() {
        assert!(session_is_terminal(&SessionStatus::Completed));
    }

    #[test]
    fn session_is_terminal_failed() {
        assert!(session_is_terminal(&SessionStatus::Failed));
    }

    #[test]
    fn session_is_terminal_cancelled() {
        assert!(session_is_terminal(&SessionStatus::Cancelled));
    }

    #[test]
    fn session_not_terminal_running() {
        assert!(!session_is_terminal(&SessionStatus::Running));
    }

    #[test]
    fn session_not_terminal_created() {
        assert!(!session_is_terminal(&SessionStatus::Created));
    }
}
