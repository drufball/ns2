use futures::StreamExt;
use std::io::Write;
use std::path::Path;

/// Read the server URL from the given `ns2.toml` path.
///
/// Returns the `[server] url` value if present, otherwise the default
/// `http://127.0.0.1:9876`.  Accepts a `&Path` so it is testable with
/// temporary files.
pub fn read_server_url(path: &Path) -> String {
    if let Ok(contents) = std::fs::read_to_string(path) {
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
    "http://127.0.0.1:9876".to_string()
}

/// Read `channel-id` from the given `ns2.local.toml` path.
///
/// Returns an error message if the file doesn't exist or the field is missing.
/// Accepts a `&Path` so it is testable with temporary files.
pub fn read_channel_id(path: &Path) -> Result<String, String> {
    let contents = std::fs::read_to_string(path).map_err(|_| {
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

/// Build the JSON-RPC `initialize` response with the `claude/channel` capability.
///
/// Pure function extracted from `run_mcp` so it can be tested in isolation.
#[must_use]
pub fn build_initialize_response(id: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
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
    })
}

/// `ns2 mcp` — run the MCP server plugin.
///
/// Reads the `channel-id` from `ns2.local.toml` and subscribes to SSE events
/// from the ns2 server, forwarding `McpChannelNotification` events as JSON-RPC
/// `notifications/claude/channel` messages to stdout.
///
/// **Sequencing:** The MCP `initialize` handshake MUST complete BEFORE
/// opening the SSE connection.  Claude Code sends `initialize` first and
/// waits for a response before proceeding — opening the SSE connection
/// prematurely causes the process to crash if the server is not yet
/// available, breaking Claude Code's session.
///
/// **Failure handling:** If the SSE connection fails (server not running or
/// temporarily unavailable), a warning is written to stderr and the process
/// continues — Claude Code's session is not interrupted.
#[allow(clippy::too_many_lines)]
pub async fn run_mcp() {
    // 1. Read server URL from ns2.toml
    let server_url = {
        let path = workspace::git_root_sync().map_or_else(
            || std::path::PathBuf::from("ns2.toml"),
            |r| r.join("ns2.toml"),
        );
        read_server_url(&path)
    };

    // 2. Read channel-id from ns2.local.toml
    let channel_id = {
        let path = workspace::git_root_sync().map_or_else(
            || std::path::PathBuf::from("ns2.local.toml"),
            |r| r.join("ns2.local.toml"),
        );
        match read_channel_id(&path) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    };

    eprintln!("ns2 mcp: channel_id={channel_id} server={server_url}");

    // 3. Start reading stdin for the MCP initialize request BEFORE connecting
    //    to the SSE stream.  Claude Code sends `initialize` first; we must
    //    respond before doing anything else.
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

    // 4. Wait for the `initialize` request and respond to it immediately.
    //    This must complete BEFORE opening the SSE connection.
    loop {
        match stdin_rx.recv().await {
            None => return, // stdin closed before initialize — nothing to do
            Some(line) => {
                if let Ok(req) = serde_json::from_str::<serde_json::Value>(&line) {
                    if req.get("method").and_then(|m| m.as_str()) == Some("initialize") {
                        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        let response = build_initialize_response(&id);
                        let stdout = std::io::stdout();
                        let mut out = stdout.lock();
                        writeln!(
                            out,
                            "{}",
                            serde_json::to_string(&response).unwrap_or_default()
                        )
                        .ok();
                        out.flush().ok();
                        break;
                    }
                    // Non-initialize message before initialize: ignore and keep waiting
                }
            }
        }
    }

    // 5. NOW open the SSE connection (non-fatally — Claude Code's session must
    //    survive even if the server is temporarily unavailable).
    let sse_url = format!(
        "{server_url}/events?event_type=mcp.channel_notification&channel_id={channel_id}"
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(0)) // no timeout — long-lived SSE
        .build()
        .unwrap_or_default();

    let sse_stream_result = client
        .get(&sse_url)
        .header("Accept", "text/event-stream")
        .send()
        .await;

    let sse_stream = match sse_stream_result {
        Err(e) => {
            eprintln!("ns2 mcp: warning: could not connect to SSE stream: {e}");
            eprintln!("ns2 mcp: continuing without live notifications");
            // Keep stdin loop running so Claude Code's session is not interrupted
            loop {
                match stdin_rx.recv().await {
                    None => return,
                    Some(_line) => {
                        // Silently ignore any further stdin messages (ping, etc.)
                    }
                }
            }
        }
        Ok(resp) if !resp.status().is_success() => {
            eprintln!(
                "ns2 mcp: warning: SSE stream returned {}: continuing without live notifications",
                resp.status()
            );
            loop {
                match stdin_rx.recv().await {
                    None => return,
                    Some(_line) => {}
                }
            }
        }
        Ok(resp) => resp.bytes_stream(),
    };

    let mut sse_stream = sse_stream;
    let mut sse_buf = String::new();

    // 6. Main loop: forward SSE notifications and handle stdin (ping, etc.)
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
                                let Ok(ev) = serde_json::from_str::<events::SystemEvent>(data)
                                else {
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
                                    writeln!(
                                        out,
                                        "{}",
                                        serde_json::to_string(&notification)
                                            .unwrap_or_default()
                                    )
                                    .ok();
                                    out.flush().ok();
                                }
                            }
                        }
                    }
                }
            }
            // Handle incoming JSON-RPC messages from stdin (ping, etc.)
            msg = stdin_rx.recv() => {
                match msg {
                    None => break, // stdin closed
                    Some(_line) => {
                        // Additional stdin messages (ping, etc.) are silently ignored
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_initialize_response, read_channel_id, read_server_url};
    use std::io::Write;

    // ─── Helpers ──────────────────────────────────────────────────────────────

    /// Write `content` to a temp file and return a `NamedTempFile` whose
    /// lifetime keeps the file alive for the duration of the test.
    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("create tempfile");
        f.write_all(content.as_bytes()).expect("write tempfile");
        f
    }

    // ── Scenario F — MCP handshake ────────────────────────────────────────────

    /// `build_initialize_response` must return a JSON-RPC 2.0 response with
    /// the correct `experimental.claude/channel` capability, matching id, and
    /// server info.
    #[test]
    fn build_initialize_response_has_correct_structure() {
        let id = serde_json::json!(1);
        let response = build_initialize_response(&id);
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert!(response["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
        assert_eq!(response["result"]["serverInfo"]["name"], "ns2-mcp");
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
    }

    /// `build_initialize_response` must echo back a string id.
    #[test]
    fn build_initialize_response_echoes_string_id() {
        let id = serde_json::json!("req-42");
        let response = build_initialize_response(&id);
        assert_eq!(response["id"], "req-42");
    }

    /// `build_initialize_response` must echo back a null id.
    #[test]
    fn build_initialize_response_echoes_null_id() {
        let id = serde_json::Value::Null;
        let response = build_initialize_response(&id);
        assert!(response["id"].is_null());
    }

    // ── Scenario F — read_channel_id: real function with temp files ──────────

    /// Calling `read_channel_id` on a file containing `channel-id = "dev-local"`
    /// must return the string `"dev-local"`.
    #[test]
    fn read_channel_id_parses_correctly_from_temp_file() {
        let f = write_temp("channel-id = \"dev-local\"\n");
        let result = read_channel_id(f.path());
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert_eq!(result.unwrap(), "dev-local");
    }

    /// Calling `read_channel_id` on a missing path must return an error whose
    /// message mentions `"channel-id"`.
    #[test]
    fn read_channel_id_missing_file_returns_error_mentioning_channel_id() {
        let result = read_channel_id(std::path::Path::new("/tmp/this-file-does-not-exist.toml"));
        assert!(result.is_err(), "expected Err for missing file");
        let err = result.unwrap_err();
        assert!(
            err.contains("channel-id"),
            "error must mention 'channel-id', got: {err:?}"
        );
    }

    /// Calling `read_channel_id` on a TOML file that is missing the field must
    /// return an error whose message mentions `"channel-id"`.
    #[test]
    fn read_channel_id_missing_field_returns_error_mentioning_channel_id() {
        let f = write_temp("some-other-field = \"value\"\n");
        let result = read_channel_id(f.path());
        assert!(result.is_err(), "expected Err when channel-id field missing");
        let err = result.unwrap_err();
        assert!(
            err.contains("channel-id"),
            "error must mention 'channel-id', got: {err:?}"
        );
    }

    // ── Scenario G — read_server_url: real function with temp files ───────────

    /// Calling `read_server_url` on a file containing `[server]\nurl = "http://…"`
    /// must return that URL.
    #[test]
    fn read_server_url_returns_url_from_temp_file() {
        let f = write_temp("[server]\nurl = \"http://localhost:1234\"\n");
        let url = read_server_url(f.path());
        assert_eq!(url, "http://localhost:1234");
    }

    /// Calling `read_server_url` on a missing file must return the default URL.
    #[test]
    fn read_server_url_missing_file_returns_default() {
        let url = read_server_url(std::path::Path::new("/tmp/no-ns2.toml"));
        assert_eq!(url, "http://127.0.0.1:9876");
    }

    /// Calling `read_server_url` on a TOML file without a `[server]` section
    /// must return the default URL.
    #[test]
    fn read_server_url_missing_section_returns_default() {
        let f = write_temp("[other]\nfoo = \"bar\"\n");
        let url = read_server_url(f.path());
        assert_eq!(url, "http://127.0.0.1:9876");
    }
}
