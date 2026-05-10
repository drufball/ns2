use crate::client;

/// Execute `ns2 event emit <event-type> [payload-json]`.
///
/// - `payload_json`: optional JSON string; defaults to `null` if omitted.
///   Exits non-zero with an error message if provided but not valid JSON.
pub async fn run_emit(server: &str, event_type: String, payload_json: Option<String>) {
    let payload: serde_json::Value = payload_json.map_or(serde_json::Value::Null, |s| {
        serde_json::from_str(&s).unwrap_or_else(|_| {
            eprintln!("Error: invalid JSON payload — {s:?}");
            std::process::exit(1);
        })
    });

    let url = format!("{server}/events/emit");
    let http = reqwest::Client::new();
    let body = serde_json::json!({
        "type": event_type,
        "payload": payload,
    });

    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|e| client::handle_connection_error(&e));

    if !resp.status().is_success() {
        client::print_error_response(resp).await;
    }

    // Success: silent exit 0 (matches the CLI design philosophy of quiet success)
}
