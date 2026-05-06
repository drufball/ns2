use serde_json::json;
use crate::client::{handle_connection_error, print_error_response};

// ── Helper: parse "field=value" ───────────────────────────────────────────────

fn parse_filter_field(s: &str) -> (String, serde_json::Value) {
    match s.split_once('=') {
        Some((k, v)) => {
            // Try to parse as JSON value (for booleans, numbers, null, etc.)
            // Fall back to treating as string
            let json_val = serde_json::from_str::<serde_json::Value>(v)
                .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
            (k.to_string(), json_val)
        }
        None => (s.to_string(), serde_json::Value::Null),
    }
}

// ── run_new ───────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run_new(
    server: &str,
    name: String,
    source: String,
    event_types: Vec<String>,
    filter_fields: Vec<String>,
    action: String,
    target: Option<String>,
    body_template: Option<String>,
    title: Option<String>,
    assignee: Option<String>,
) {
    let client = reqwest::Client::new();
    let url = format!("{server}/hooks");

    // Build source JSON
    let source_json = match source.as_str() {
        "internal" => json!({
            "type": "internal",
            "event_types": event_types,
        }),
        "external" => json!({
            "type": "external",
        }),
        "timer" => {
            // For timer we'd need a schedule, but keep minimal for now
            json!({
                "type": "timer",
                "schedule": event_types.first().map_or("", std::string::String::as_str),
            })
        }
        other => {
            eprintln!("Error: unknown source type '{other}'. Use: internal, external, timer");
            std::process::exit(1);
        }
    };

    // Build filter JSON
    let filter_json = if filter_fields.is_empty() {
        serde_json::Value::Null
    } else {
        let conditions: Vec<serde_json::Value> = filter_fields
            .iter()
            .map(|f| {
                let (field, value) = parse_filter_field(f);
                json!({ "field": field, "op": "eq", "value": value })
            })
            .collect();
        json!({ "conditions": conditions })
    };

    // Build action JSON
    let action_json = match action.as_str() {
        "send-message" | "send_message" => {
            let (target_type, target_id) = parse_target(target.as_deref());
            json!({
                "type": "send_message",
                "target": { "type": target_type, "content": target_id },
                "body": body_template.unwrap_or_default(),
            })
        }
        "create-issue" | "create_issue" => {
            json!({
                "type": "create_issue",
                "title": title.unwrap_or_default(),
                "body": body_template.unwrap_or_default(),
                "assignee": assignee,
                "parent": null,
                "start": false,
            })
        }
        "run-shell" | "run_shell" => {
            json!({
                "type": "run_shell",
                "command": body_template.unwrap_or_default(),
                "timeout_secs": 30,
                "blocking": false,
            })
        }
        other => {
            eprintln!("Error: unknown action type '{other}'. Use: send-message, create-issue, run-shell");
            std::process::exit(1);
        }
    };

    let mut req_body = json!({
        "name": name,
        "source": source_json,
        "action": action_json,
    });
    if filter_json != serde_json::Value::Null {
        req_body["filter"] = filter_json;
    }

    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let hook: serde_json::Value = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    let hook_id = hook["id"].as_str().unwrap_or("?");
    eprintln!("Created hook: {} ({})", hook["name"].as_str().unwrap_or(""), hook_id);
    println!("{hook_id}");
}

fn parse_target(target: Option<&str>) -> (&'static str, String) {
    match target {
        Some(t) if t.starts_with("issue:") => {
            ("issue", t["issue:".len()..].to_string())
        }
        Some(t) if t.starts_with("session:") => {
            ("session", t["session:".len()..].to_string())
        }
        Some(t) => {
            eprintln!("Error: --target must be in the form 'issue:<id>' or 'session:<id>', got: {t}");
            std::process::exit(1);
        }
        None => {
            eprintln!("Error: --target is required for send-message action");
            std::process::exit(1);
        }
    }
}

// ── run_list ──────────────────────────────────────────────────────────────────

pub async fn run_list(
    server: &str,
    enabled_only: bool,
    source_type: Option<String>,
) {
    let client = reqwest::Client::new();
    let mut params: Vec<String> = vec![];
    if enabled_only {
        params.push("enabled=true".into());
    }
    if let Some(st) = &source_type {
        params.push(format!("source_type={st}"));
    }
    let url = if params.is_empty() {
        format!("{server}/hooks")
    } else {
        format!("{server}/hooks?{}", params.join("&"))
    };

    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let hooks: Vec<serde_json::Value> = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    if hooks.is_empty() {
        println!("No hooks found.");
    } else {
        println!("{:<6}  {:<20}  {:<10}  {:<12}  source", "id", "name", "enabled", "action");
        for hook in &hooks {
            let id = hook["id"].as_str().unwrap_or("?");
            let name = hook["name"].as_str().unwrap_or("?");
            let enabled = if hook["enabled"].as_bool().unwrap_or(false) { "yes" } else { "no" };
            let action_type = hook["action"]["type"].as_str().unwrap_or("?");
            let source_type_str = hook["source"]["type"].as_str().unwrap_or("?");
            println!("{id:<6}  {name:<20}  {enabled:<10}  {action_type:<12}  {source_type_str}");
        }
    }
}

// ── run_show ──────────────────────────────────────────────────────────────────

pub async fn run_show(server: &str, id: String) {
    let client = reqwest::Client::new();
    let url = format!("{server}/hooks/{id}");
    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Error: hook not found: {id}");
        std::process::exit(1);
    }
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let hook: serde_json::Value = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    let out = serde_json::to_string_pretty(&hook).unwrap_or_else(|e| {
        eprintln!("Error formatting response: {e}");
        std::process::exit(1);
    });
    println!("{out}");
}

// ── run_enable / run_disable ──────────────────────────────────────────────────

pub async fn run_enable(server: &str, id: String) {
    set_enabled(server, id, true).await;
}

pub async fn run_disable(server: &str, id: String) {
    set_enabled(server, id, false).await;
}

async fn set_enabled(server: &str, id: String, enabled: bool) {
    let client = reqwest::Client::new();
    let url = format!("{server}/hooks/{id}");
    let resp = client
        .patch(&url)
        .json(&json!({ "enabled": enabled }))
        .send()
        .await
        .unwrap_or_else(|e| handle_connection_error(&e));
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Error: hook not found: {id}");
        std::process::exit(1);
    }
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let word = if enabled { "enabled" } else { "disabled" };
    eprintln!("Hook {id} {word}.");
}

// ── run_delete ────────────────────────────────────────────────────────────────

pub async fn run_delete(server: &str, id: String) {
    let client = reqwest::Client::new();
    let url = format!("{server}/hooks/{id}");
    let resp = client.delete(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Error: hook not found: {id}");
        std::process::exit(1);
    }
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    eprintln!("Hook {id} deleted.");
}

// ── run_logs ──────────────────────────────────────────────────────────────────

pub async fn run_logs(server: &str, id: String, limit: usize) {
    let client = reqwest::Client::new();
    let url = format!("{server}/hooks/{id}/executions?limit={limit}");
    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Error: hook not found: {id}");
        std::process::exit(1);
    }
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let execs: Vec<serde_json::Value> = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    if execs.is_empty() {
        println!("No executions found.");
    } else {
        println!("{:<36}  {:<25}  {:<10}  result", "id", "triggered_at", "status");
        for exec in &execs {
            let exec_id = exec["id"].as_str().unwrap_or("?");
            let triggered_at = exec["triggered_at"].as_str().unwrap_or("?");
            let status = exec["status"].as_str().unwrap_or("?");
            let result = exec["result"].as_str().unwrap_or("-");
            // Truncate result to avoid line wrapping
            let result_truncated = if result.len() > 40 {
                &result[..40]
            } else {
                result
            };
            println!("{exec_id:<36}  {triggered_at:<25}  {status:<10}  {result_truncated}");
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_filter_field_key_eq_string_value() {
        let (k, v) = parse_filter_field("status=running");
        assert_eq!(k, "status");
        assert_eq!(v, serde_json::Value::String("running".to_string()));
    }

    #[test]
    fn parse_filter_field_json_boolean() {
        let (k, v) = parse_filter_field("enabled=true");
        assert_eq!(k, "enabled");
        assert_eq!(v, serde_json::Value::Bool(true));
    }

    #[test]
    fn parse_filter_field_json_number() {
        let (k, v) = parse_filter_field("count=42");
        assert_eq!(k, "count");
        assert_eq!(v, serde_json::json!(42));
    }

    #[test]
    fn parse_filter_field_no_equals_gives_null() {
        let (k, v) = parse_filter_field("just-a-key");
        assert_eq!(k, "just-a-key");
        assert_eq!(v, serde_json::Value::Null);
    }

    #[test]
    fn parse_target_issue() {
        let (t, id) = parse_target(Some("issue:abc1"));
        assert_eq!(t, "issue");
        assert_eq!(id, "abc1");
    }

    #[test]
    fn parse_target_session() {
        let (t, id) = parse_target(Some("session:sess-xyz"));
        assert_eq!(t, "session");
        assert_eq!(id, "sess-xyz");
    }
}
