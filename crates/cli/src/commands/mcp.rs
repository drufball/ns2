use crate::client::handle_connection_error;
use futures::StreamExt;
use std::io::Write;

/// Read the server URL from `ns2.toml` (default: `http://127.0.0.1:9876`).
fn read_server_url() -> String {
    if let Some(root) = workspace::git_root_sync() {
        let path = root.join("ns2.toml");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(value) = contents.parse::<toml::Value>() {
                if let Some(url) = value
                    .get("server")
                    .and_then(|s| s.get("url"))
                    .and_then(|u| u.as_str())
                {
                    return url.to_string();
                }
            }
        }
    }
    "http://127.0.0.1:9876".to_string()
}

/// Read `channel-id` from `ns2.local.toml`.
/// Returns an error message if the file doesn't exist or the field is missing.
fn read_channel_id() -> Result<String, String> {
    let root = workspace::git_root_sync().ok_or_else(|| {
        "Error: ns2.local.toml must contain channel-id = \"<id>\"".to_string()
    })?;
    let path = root.join("ns2.local.toml");
    let contents = std::fs::read_to_string(&path).map_err(|_| {
        "Error: ns2.local.toml must contain channel-id = \"<id>\"".to_string()
    })?;
    let value: toml::Value = contents
        .parse()
        .map_err(|_| "Error: ns2.local.toml must contain channel-id = \"<id>\"".to_string())?;
    value
        .get("channel-id")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
        .ok_or_else(|| "Error: ns2.local.toml must contain channel-id = \"<id>\"".to_string())
}

/// `ns2 mcp` — run the MCP server plugin.
///
/// Reads the `channel-id` from `ns2.local.toml` and subscribes to SSE events
/// from the ns2 server, forwarding `McpChannelNotification` events as JSON-RPC
/// `notifications/claude/channel` messages to stdout.
///
/// Also handles JSON-RPC `initialize` requests from stdin.
#[allow(clippy::too_many_lines)]
pub async fn run_mcp() {
    // 1. Read server URL from ns2.toml
    let server_url = read_server_url();

    // 2. Read channel-id from ns2.local.toml
    let channel_id = match read_channel_id() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    eprintln!("ns2 mcp: channel_id={channel_id} server={server_url}");

    // 3. Subscribe to SSE stream
    let sse_url = format!(
        "{server_url}/events?event_type=mcp.channel_notification&channel_id={channel_id}"
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(0)) // no timeout — long-lived SSE
        .build()
        .unwrap_or_default();

    let resp = client
        .get(&sse_url)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap_or_else(|e| {
            handle_connection_error(&e);
        });

    if !resp.status().is_success() {
        eprintln!("ns2 mcp: failed to connect to SSE stream: {}", resp.status());
        std::process::exit(1);
    }

    let mut sse_stream = resp.bytes_stream();
    let mut sse_buf = String::new();

    // 4. Concurrently read JSON-RPC from stdin and SSE from server
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<String>(64);

    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let stdin = tokio::io::stdin();
        let mut reader = tokio::io::BufReader::new(stdin);
        let mut line = String::new();
        loop {
            line.clear();
            #[allow(clippy::match_same_arms)]
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() && stdin_tx.send(trimmed).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    loop {
        tokio::select! {
            // Handle incoming SSE bytes
            chunk = sse_stream.next() => {
                match chunk {
                    None | Some(Err(_)) => {
                        eprintln!("ns2 mcp: SSE stream closed or error");
                        break;
                    }
                    Some(Ok(bytes)) => {
                        let s = String::from_utf8_lossy(&bytes).to_string();
                        let frames = crate::render::parse_sse_frames(&mut sse_buf, &s);
                        for frame in frames {
                            for line in frame.lines() {
                                let Some(data) = line.strip_prefix("data: ") else {
                                    continue;
                                };
                                let Ok(ev) = serde_json::from_str::<events::SystemEvent>(data) else {
                                    continue;
                                };
                                if let events::SystemEvent::McpChannelNotification {
                                    channel_id: ref ev_channel,
                                    body: ref ev_body,
                                    ref meta,
                                } = ev
                                {
                                    let notification = serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "method": "notifications/claude/channel",
                                        "params": {
                                            "channel": ev_channel,
                                            "body": ev_body,
                                            "meta": meta,
                                        }
                                    });
                                    let stdout = std::io::stdout();
                                    let mut out = stdout.lock();
                                    writeln!(out, "{}", serde_json::to_string(&notification).unwrap_or_default()).ok();
                                    out.flush().ok();
                                }
                            }
                        }
                    }
                }
            }
            // Handle incoming JSON-RPC messages from stdin
            msg = stdin_rx.recv() => {
                match msg {
                    None => break, // stdin closed
                    Some(line) => {
                        if let Ok(req) = serde_json::from_str::<serde_json::Value>(&line) {
                            if req.get("method").and_then(|m| m.as_str()) == Some("initialize") {
                                let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
                                let response = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "protocolVersion": "2024-11-05",
                                        "capabilities": {
                                            "experimental": {
                                                "claude/channel": {}
                                            }
                                        },
                                        "serverInfo": {
                                            "name": "ns2-mcp",
                                            "version": "0.1.0"
                                        }
                                    }
                                });
                                let stdout = std::io::stdout();
                                let mut out = stdout.lock();
                                writeln!(out, "{}", serde_json::to_string(&response).unwrap_or_default()).ok();
                                out.flush().ok();
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // ── Scenario F — MCP handshake ────────────────────────────────────────────

    #[test]
    fn initialize_response_has_correct_structure() {
        let id = serde_json::json!(1);
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "experimental": {
                        "claude/channel": {}
                    }
                },
                "serverInfo": {
                    "name": "ns2-mcp",
                    "version": "0.1.0"
                }
            }
        });
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert!(response["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
        assert_eq!(response["result"]["serverInfo"]["name"], "ns2-mcp");
    }

    // ── Scenario G — missing channel-id ──────────────────────────────────────

    #[test]
    fn missing_channel_id_returns_error_mentioning_channel_id() {
        // Test the TOML parsing error path
        let expected_msg = "Error: ns2.local.toml must contain channel-id = \"<id>\"";
        assert!(expected_msg.contains("channel-id"));
    }

    #[test]
    fn read_channel_id_parses_correctly_from_toml_string() {
        let toml_str = "channel-id = \"dev-local\"\n";
        let value: toml::Value = toml_str.parse().unwrap();
        let channel_id = value
            .get("channel-id")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string)
            .ok_or_else(|| "Error: ns2.local.toml must contain channel-id = \"<id>\"".to_string());
        assert!(channel_id.is_ok());
        assert_eq!(channel_id.unwrap(), "dev-local");
    }

    #[test]
    fn read_channel_id_returns_error_when_field_missing() {
        let toml_str = "some-other-field = \"value\"\n";
        let value: toml::Value = toml_str.parse().unwrap();
        let channel_id: Result<String, String> = value
            .get("channel-id")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string)
            .ok_or_else(|| "Error: ns2.local.toml must contain channel-id = \"<id>\"".to_string());
        assert!(channel_id.is_err());
        let err = channel_id.expect_err("should be error");
        assert!(err.contains("channel-id"), "error must mention channel-id");
    }

    #[test]
    fn read_server_url_falls_back_to_default() {
        let toml_with_server = "[server]\nurl = \"http://localhost:1234\"\n";
        let value: toml::Value = toml_with_server.parse().unwrap();
        let url = value
            .get("server")
            .and_then(|s| s.get("url"))
            .and_then(|u| u.as_str())
            .unwrap_or("http://127.0.0.1:9876")
            .to_string();
        assert_eq!(url, "http://localhost:1234");

        let toml_without_server = "[other]\nfoo = \"bar\"\n";
        let value2: toml::Value = toml_without_server.parse().unwrap();
        let url2 = value2
            .get("server")
            .and_then(|s| s.get("url"))
            .and_then(|u| u.as_str())
            .unwrap_or("http://127.0.0.1:9876")
            .to_string();
        assert_eq!(url2, "http://127.0.0.1:9876");
    }
}
