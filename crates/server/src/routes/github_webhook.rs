use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::Utc;

use crate::state::AppState;

/// Label prefix used by the GitHub backend to store the ns2 issue ID.
const NS2_ID_LABEL_PREFIX: &str = "ns2-id:";

/// Extract the ns2 ID from a GitHub issue's label list, if present.
fn ns2_id_from_labels(gh_issue: &serde_json::Value) -> Option<String> {
    gh_issue["labels"]
        .as_array()?
        .iter()
        .find_map(|l| {
            l["name"]
                .as_str()
                .and_then(|name| name.strip_prefix(NS2_ID_LABEL_PREFIX))
                .map(String::from)
        })
}

fn ok_response() -> axum::response::Response {
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}

fn unauthorized(msg: &str) -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

/// Reason a signature check failed, used to construct the response in the caller.
enum HmacError {
    Missing,
    InvalidEncoding,
    Mismatch,
}

/// Validate the `X-Hub-Signature-256` header against `secret`.
///
/// Returns `Ok(())` when the signature is valid, `Err(HmacError)` on failure.
fn validate_hmac(headers: &HeaderMap, body: &[u8], secret: &str) -> Result<(), HmacError> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let sig_header = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let Some(hex_digest) = sig_header.strip_prefix("sha256=") else {
        return Err(HmacError::Missing);
    };

    let Ok(expected_bytes) = hex::decode(hex_digest) else {
        return Err(HmacError::InvalidEncoding);
    };

    // new_from_slice only fails for zero-length keys; our secret is non-empty.
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return Err(HmacError::Mismatch);
    };
    mac.update(body);
    if mac.verify_slice(&expected_bytes).is_err() {
        return Err(HmacError::Mismatch);
    }
    Ok(())
}

/// Handle `issues` + `opened` — emit `IssueEvent::Created`.
async fn handle_issues_opened(state: &AppState, payload: &serde_json::Value) {
    let gh_issue = &payload["issue"];
    let Some(ns2_id) = ns2_id_from_labels(gh_issue) else {
        tracing::debug!("GitHub webhook: issues.opened has no ns2-id label, skipping");
        return;
    };
    match state.db.get_issue(ns2_id.clone()).await {
        Ok(issue) => {
            state
                .event_bus
                .send(events::SystemEvent::Issue(events::IssueEvent::Created(issue)));
        }
        Err(e) => {
            tracing::debug!(ns2_id = %ns2_id, error = %e,
                "GitHub webhook: issues.opened — issue not found locally");
        }
    }
}

/// Handle `issues` + `closed` — emit `IssueEvent::StatusChanged { to: Completed }`.
async fn handle_issues_closed(state: &AppState, payload: &serde_json::Value) {
    let gh_issue = &payload["issue"];
    let Some(ns2_id) = ns2_id_from_labels(gh_issue) else {
        tracing::debug!("GitHub webhook: issues.closed has no ns2-id label, skipping");
        return;
    };
    match state.db.get_issue(ns2_id.clone()).await {
        Ok(issue) => {
            let from = issue.status.clone();
            state.event_bus.send(events::SystemEvent::Issue(
                events::IssueEvent::StatusChanged {
                    issue,
                    from,
                    to: types::IssueStatus::Completed,
                },
            ));
        }
        Err(e) => {
            tracing::debug!(ns2_id = %ns2_id, error = %e,
                "GitHub webhook: issues.closed — issue not found locally");
        }
    }
}

/// Handle `issue_comment` + `created` — emit `IssueEvent::CommentAdded`.
async fn handle_issue_comment_created(state: &AppState, payload: &serde_json::Value) {
    let gh_issue = &payload["issue"];
    let Some(ns2_id) = ns2_id_from_labels(gh_issue) else {
        tracing::debug!("GitHub webhook: issue_comment.created has no ns2-id label, skipping");
        return;
    };
    let comment = types::IssueComment {
        author: payload["comment"]["user"]["login"]
            .as_str()
            .unwrap_or("github")
            .to_string(),
        body: payload["comment"]["body"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        created_at: Utc::now(),
    };
    match state.db.get_issue(ns2_id.clone()).await {
        Ok(issue) => {
            state.event_bus.send(events::SystemEvent::Issue(
                events::IssueEvent::CommentAdded { issue, comment },
            ));
        }
        Err(e) => {
            tracing::debug!(ns2_id = %ns2_id, error = %e,
                "GitHub webhook: issue_comment.created — issue not found locally");
        }
    }
}

/// `POST /webhooks/github`
///
/// Receives GitHub webhook events, validates the HMAC-SHA256 signature (when
/// `NS2_GITHUB_WEBHOOK_SECRET` is set), and translates recognised GitHub events
/// into [`events::SystemEvent`] emissions on the global event bus.
///
/// Supported events:
/// - `X-GitHub-Event: issues`, `action: opened`   → `IssueEvent::Created`
/// - `X-GitHub-Event: issues`, `action: closed`   → `IssueEvent::StatusChanged { to: Completed }`
/// - `X-GitHub-Event: issue_comment`, `action: created` → `IssueEvent::CommentAdded`
///
/// Unknown/unrecognised events always return 200 OK (GitHub retries on non-2xx).
/// Invalid HMAC signatures return 401.
pub async fn receive_github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Validate HMAC when a webhook secret is configured.
    if let Some(ref secret) = state.github_webhook_secret {
        if let Err(e) = validate_hmac(&headers, &body, secret) {
            let msg = match e {
                HmacError::Missing => "missing or invalid signature",
                HmacError::InvalidEncoding => "invalid signature encoding",
                HmacError::Mismatch => "signature mismatch",
            };
            return unauthorized(msg);
        }
    }

    // Parse body as JSON (bad JSON → 200 OK, GitHub retrying won't fix it).
    let Ok(payload): Result<serde_json::Value, _> = serde_json::from_slice(&body) else {
        tracing::warn!("GitHub webhook: failed to parse JSON body");
        return ok_response();
    };

    let gh_event = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let action = payload["action"].as_str().unwrap_or("");

    tracing::debug!(gh_event, action, "GitHub webhook received");

    match (gh_event, action) {
        ("issues", "opened") => handle_issues_opened(&state, &payload).await,
        ("issues", "closed") => handle_issues_closed(&state, &payload).await,
        ("issue_comment", "created") => handle_issue_comment_created(&state, &payload).await,
        _ => tracing::debug!(gh_event, action, "GitHub webhook: unrecognised event, ignoring"),
    }

    ok_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Compute the `sha256=<hex>` HMAC signature for a body with the given secret.
    fn sign_body(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize().into_bytes();
        format!("sha256={}", hex::encode(result))
    }

    /// Build a test request to `POST /webhooks/github`.
    fn make_request(event: &str, body: &str, sig: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("content-type", "application/json")
            .header("x-github-event", event);
        if let Some(s) = sig {
            builder = builder.header("x-hub-signature-256", s);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    fn make_issue_record(id: &str, status: types::IssueStatus) -> types::Issue {
        types::Issue {
            id: id.into(),
            title: "Test".into(),
            body: "body".into(),
            status,
            branch: String::new(),
            assignee: None,
            session_id: None,
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Build a test state with an optional webhook secret.
    async fn test_state_with_secret(secret: Option<&str>) -> crate::state::AppState {
        let mut state = crate::tests::test_state().await;
        state.github_webhook_secret = secret.map(String::from);
        state
    }

    // ── Scenario: invalid HMAC → 401 ─────────────────────────────────────────

    #[tokio::test]
    async fn invalid_hmac_returns_401() {
        let state = test_state_with_secret(Some("test-secret")).await;
        let app = crate::build_router(state);

        let body = r#"{"action":"opened","issue":{"number":1,"labels":[]}}"#;
        let bad_sig = "sha256=0000000000000000000000000000000000000000000000000000000000000000";

        let resp = app
            .oneshot(make_request("issues", body, Some(bad_sig)))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ── Scenario: missing signature when secret set → 401 ────────────────────

    #[tokio::test]
    async fn missing_signature_when_secret_set_returns_401() {
        let state = test_state_with_secret(Some("test-secret")).await;
        let app = crate::build_router(state);

        let body = r#"{"action":"opened","issue":{"number":1,"labels":[]}}"#;

        let resp = app
            .oneshot(make_request("issues", body, None))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ── Scenario: no secret configured → any request accepted ────────────────

    #[tokio::test]
    async fn no_secret_configured_accepts_any_request() {
        let state = test_state_with_secret(None).await;
        let app = crate::build_router(state);

        let body = r#"{"action":"opened","issue":{"number":1,"labels":[]}}"#;

        let resp = app
            .oneshot(make_request("issues", body, None))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── Scenario: valid signature with issues.opened → 200, event emitted ────

    #[tokio::test]
    async fn valid_issues_opened_emits_created_event() {
        let state = test_state_with_secret(Some("my-secret")).await;
        state
            .db
            .create_issue(&make_issue_record("ab12", types::IssueStatus::Open))
            .await
            .unwrap();

        let mut rx = state.event_bus.subscribe();
        let app = crate::build_router(state);

        let body = serde_json::json!({
            "action": "opened",
            "issue": {
                "number": 42,
                "title": "Test",
                "body": "body",
                "labels": [{"name": "ns2-id:ab12"}],
            }
        })
        .to_string();
        let sig = sign_body("my-secret", body.as_bytes());

        let resp = app
            .oneshot(make_request("issues", &body, Some(&sig)))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let event = rx.try_recv().expect("should have received an event");
        assert!(
            matches!(
                event,
                events::SystemEvent::Issue(events::IssueEvent::Created(ref i)) if i.id == "ab12"
            ),
            "expected IssueEvent::Created for ab12, got: {event:?}"
        );
    }

    // ── Scenario: valid signature with issues.closed → 200 ───────────────────

    #[tokio::test]
    async fn valid_issues_closed_emits_status_changed_event() {
        let state = test_state_with_secret(Some("my-secret")).await;
        state
            .db
            .create_issue(&make_issue_record("ab12", types::IssueStatus::InProgress))
            .await
            .unwrap();

        let mut rx = state.event_bus.subscribe();
        let app = crate::build_router(state);

        let body = serde_json::json!({
            "action": "closed",
            "issue": {
                "number": 42,
                "title": "Test",
                "body": "body",
                "labels": [{"name": "ns2-id:ab12"}],
            }
        })
        .to_string();
        let sig = sign_body("my-secret", body.as_bytes());

        let resp = app
            .oneshot(make_request("issues", &body, Some(&sig)))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let event = rx.try_recv().expect("should have received an event");
        assert!(
            matches!(
                event,
                events::SystemEvent::Issue(events::IssueEvent::StatusChanged {
                    to: types::IssueStatus::Completed,
                    ..
                })
            ),
            "expected IssueEvent::StatusChanged to Completed, got: {event:?}"
        );
    }

    // ── Scenario: valid signature with issue_comment.created → 200 ────────────

    #[tokio::test]
    async fn valid_issue_comment_created_emits_comment_added_event() {
        let state = test_state_with_secret(Some("my-secret")).await;
        state
            .db
            .create_issue(&make_issue_record("ab12", types::IssueStatus::Open))
            .await
            .unwrap();

        let mut rx = state.event_bus.subscribe();
        let app = crate::build_router(state);

        let body = serde_json::json!({
            "action": "created",
            "issue": {
                "number": 42,
                "labels": [{"name": "ns2-id:ab12"}],
            },
            "comment": {
                "body": "Great work!",
                "user": {"login": "drufball"},
            }
        })
        .to_string();
        let sig = sign_body("my-secret", body.as_bytes());

        let resp = app
            .oneshot(make_request("issue_comment", &body, Some(&sig)))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let event = rx.try_recv().expect("should have received an event");
        assert!(
            matches!(
                event,
                events::SystemEvent::Issue(events::IssueEvent::CommentAdded {
                    ref comment,
                    ..
                }) if comment.author == "drufball" && comment.body == "Great work!"
            ),
            "expected IssueEvent::CommentAdded with correct comment, got: {event:?}"
        );
    }

    // ── Scenario: unknown event → 200, nothing emitted ────────────────────────

    #[tokio::test]
    async fn unknown_event_returns_200_without_emitting() {
        let state = test_state_with_secret(None).await;
        let mut rx = state.event_bus.subscribe();
        let app = crate::build_router(state);

        let body = r#"{"action":"labeled","issue":{"number":1,"labels":[]}}"#;

        let resp = app
            .oneshot(make_request("issues", body, None))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            rx.try_recv().is_err(),
            "expected no event to be emitted for unknown action"
        );
    }
}
