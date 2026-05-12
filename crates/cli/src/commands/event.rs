use crate::client::{handle_connection_error, print_error_response};

/// `ns2 event new <name> --type webhook [--secret <secret>] [--description <desc>]`
/// `ns2 event new <name> --type timer --schedule "* * * * *"`
pub async fn run_new(
    server: &str,
    name: String,
    event_type: String,
    secret: Option<String>,
    schedule: Option<String>,
    description: Option<String>,
) {
    let client = reqwest::Client::new();
    let url = format!("{server}/named-events");

    let kind = match event_type.as_str() {
        "webhook" => {
            serde_json::json!({ "type": "webhook", "secret": secret })
        }
        "timer" => {
            let Some(sched) = schedule else {
                eprintln!("Error: --schedule is required when --type timer is used.");
                eprintln!("Example: ns2 event new heartbeat --type timer --schedule \"* * * * *\"");
                std::process::exit(1);
            };
            serde_json::json!({ "type": "timer", "schedule": sched })
        }
        other => {
            eprintln!("Error: unknown event type '{other}'. Use: webhook, timer");
            std::process::exit(1);
        }
    };

    let mut req_body = serde_json::json!({
        "name": name,
        "kind": kind,
    });
    if let Some(desc) = description {
        req_body["description"] = serde_json::Value::String(desc);
    }

    let resp = client
        .post(&url)
        .json(&req_body)
        .send()
        .await
        .unwrap_or_else(|e| {
            handle_connection_error(&e);
        });
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let event: serde_json::Value = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    let event_id = event["id"].as_str().unwrap_or("?");
    eprintln!(
        "Created event: {} ({})",
        event["name"].as_str().unwrap_or(""),
        event_id
    );
    println!("{event_id}");
}

/// `ns2 event list`
pub async fn run_list(server: &str) {
    let client = reqwest::Client::new();
    let url = format!("{server}/named-events");

    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let events: Vec<serde_json::Value> = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    if events.is_empty() {
        println!("No events found.");
    } else {
        println!(
            "{:<6}  {:<20}  {:<10}  {:<10}  created_at",
            "ID", "NAME", "TYPE", "STATUS"
        );
        for ev in &events {
            let id = ev["id"].as_str().unwrap_or("?");
            let name = ev["name"].as_str().unwrap_or("?");
            let kind_type = ev["kind"]["type"].as_str().unwrap_or("?");
            let status = if ev["enabled"].as_bool().unwrap_or(true) {
                "enabled"
            } else {
                "disabled"
            };
            let created_at = ev["created_at"].as_str().unwrap_or("?");
            println!("{id:<6}  {name:<20}  {kind_type:<10}  {status:<10}  {created_at}");
        }
    }
}

/// `ns2 event delete --id <id>`
pub async fn run_delete(server: &str, id: String) {
    let client = reqwest::Client::new();
    let url = format!("{server}/named-events/{id}");

    let resp = client.delete(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Error: event not found: {id}");
        std::process::exit(1);
    }
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    eprintln!("Event {id} deleted.");
}

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
        .unwrap_or_else(|e| handle_connection_error(&e));

    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // CLI parse tests for `ns2 event` are in cli/src/main.rs test module.
}
