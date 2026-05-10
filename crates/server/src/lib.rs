use axum::{
    routing::{delete, get, patch, post},
    Router,
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use tokio::net::TcpListener;

mod harness_spawn;
mod routes;
mod state;

pub use routes::session::CreateSessionRequest;
pub use routes::{Error, Result};

use routes::{events_route, hook as hook_route, issue, named_event, session, webhook};
use state::AppState;

use events::EventBus;

pub struct ServerConfig {
    pub port: u16,
    pub data_dir: PathBuf,
    pub pid_file: PathBuf,
    pub model: String,
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(session::health))
        .route("/events", get(events_route::events))
        .route("/sessions", post(session::create_session))
        .route("/sessions", get(session::list_sessions))
        .route("/sessions/:id", get(session::get_session))
        .route("/sessions/:id/messages", post(session::send_message))
        .route("/sessions/:id/cancel", post(session::cancel_session))
        .route(
            "/sessions/:id/status",
            axum::routing::patch(session::update_session_status),
        )
        .route("/sessions/:id/last_text", get(session::session_last_text))
        .route("/issues", post(issue::create_issue))
        .route("/issues", get(issue::list_issues))
        .route("/issues/:id", get(issue::get_issue))
        .route("/issues/:id", axum::routing::patch(issue::edit_issue))
        .route("/issues/:id/comments", post(issue::add_comment))
        .route("/issues/:id/complete", post(issue::complete_issue))
        .route("/issues/:id/cancel", post(issue::cancel_issue))
        .route("/issues/:id/reopen", post(issue::reopen_issue))
        .route(
            "/issues/:id/status",
            axum::routing::patch(issue::update_issue_status),
        )
        // Hook CRUD
        .route("/hooks", post(hook_route::create_hook))
        .route("/hooks", get(hook_route::list_hooks))
        .route("/hooks/:id", get(hook_route::get_hook))
        .route("/hooks/:id", patch(hook_route::update_hook))
        .route("/hooks/:id", delete(hook_route::delete_hook))
        .route("/hooks/:id/executions", get(hook_route::list_executions))
        // External webhook receiver
        .route("/webhooks/:event_id", post(webhook::receive_webhook))
        // Named event CRUD
        .route("/named-events", post(named_event::create_event))
        .route("/named-events", get(named_event::list_events))
        .route("/named-events/:id", get(named_event::get_event))
        .route("/named-events/:id", delete(named_event::delete_event))
        .with_state(state)
}

/// Spawn the hook evaluator background task for the given state.
fn spawn_hook_evaluator(state: &AppState) {
    let mut rx = state.event_bus.subscribe();
    let hook_store_eval = Arc::clone(&state.hook_store);
    let issue_svc_eval = state.issue_service.clone();
    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let hooks = hook_store_eval
                        .list_hooks(Some(true))
                        .await
                        .unwrap_or_default();
                    for hook in hooks {
                        if hooks::evaluate::matches_event(&hook, &event) {
                            let event_clone = event.clone();
                            let hook_clone = hook.clone();
                            let issue_svc = issue_svc_eval.clone();
                            let hook_store_clone = Arc::clone(&hook_store_eval);
                            tokio::spawn(async move {
                                hooks::execute::run_action(
                                    &hook_clone,
                                    &event_clone,
                                    &issue_svc,
                                    hook_store_clone.as_ref(),
                                )
                                .await;
                            });
                        }
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("Hook evaluator lagged {n} messages");
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Handle `IssueEvent::StatusChanged { to: InProgress }` — resume or start a harness.
async fn handle_in_progress(state: &AppState, issue: types::Issue) {
    if issue.assignee.is_none() {
        return;
    }
    if let Some(session_id) = issue.session_id {
        if let Ok(session) = state.db.get_session(session_id).await {
            if session.status == types::SessionStatus::Waiting {
                let msg_tx = crate::harness_spawn::spawn_harness_sync(state, session);
                msg_tx.send("Please continue.".to_string()).await.ok();
                return;
            }
            if session.status == types::SessionStatus::Created {
                let initial_message = issues::build_initial_message(&issue);
                let msg_tx = crate::harness_spawn::spawn_harness_sync(state, session);
                msg_tx.send(initial_message).await.ok();
                return;
            }
            // Session exists but is in a state where we cannot (or need not)
            // spawn a harness (e.g. already Running, Completed, Failed,
            // Cancelled). Log so operators can diagnose unexpected paths.
            tracing::debug!(
                issue_id = %issue.id,
                session_id = %session_id,
                session_status = %session.status,
                "handle_in_progress: session in unexpected state, skipping harness spawn"
            );
        }
    }
}

/// Spawn the global issue lifecycle subscriber.
///
/// This single long-lived task handles all cross-cutting session→issue and
/// issue→session lifecycle transitions:
///
/// **Session events → issue state:**
/// - `SessionEvent::Stopped` — stores stop info (status + comment) keyed by `session_id`
/// - `SessionEvent::Done`    — looks up the linked issue and parks it (Completed or Waiting)
/// - `SessionEvent::Error`   — looks up the linked issue and fails it
///
/// **Issue state changes → session actions:**
/// - `IssueEvent::StatusChanged { to: InProgress }` — start or resume harness
/// - `IssueEvent::StatusChanged { to: Cancelled }`  — drop msg sender to kill harness
pub fn spawn_issue_lifecycle_subscriber(state: &AppState) {
    use events::{IssueEvent, SessionEvent, StopEventStatus, SystemEvent};
    use std::collections::HashMap;
    use tokio::sync::broadcast::error::RecvError;
    use types::IssueStatus;

    let mut rx = state.event_bus.subscribe();
    let state = state.clone();

    tokio::spawn(async move {
        // Per-session stop info: session_id → (status, optional comment).
        //
        // Entries are inserted on `Stopped` and removed on `Done` or `Error`.
        // If a harness terminates abnormally without emitting either terminal
        // event (e.g. OS kill, tokio runtime shutdown), the entry is never
        // removed.  This is a known bounded leak: the server process is
        // typically short-lived relative to session count, and each entry is
        // tiny (~50 bytes).  A periodic sweep could be added if this becomes
        // a concern.
        let mut stop_map: HashMap<uuid::Uuid, (StopEventStatus, Option<String>)> = HashMap::new();

        loop {
            match rx.recv().await {
                Ok(event) => match event {
                    // ── Session events → issue state ─────────────────────────

                    SystemEvent::Session {
                        session_id,
                        event: SessionEvent::Stopped { status, comment },
                    } => {
                        stop_map.insert(session_id, (status, comment));
                    }

                    SystemEvent::Session {
                        session_id,
                        event: SessionEvent::Done,
                    } => {
                        let issues = state
                            .db
                            .list_issues_by_session_id(session_id)
                            .await
                            .unwrap_or_default();
                        for issue in issues {
                            let (park_status, comment) =
                                if let Some((stop_status, stop_comment)) =
                                    stop_map.remove(&session_id)
                                {
                                    let ps = if matches!(stop_status, StopEventStatus::Complete) {
                                        IssueStatus::Completed
                                    } else {
                                        IssueStatus::Waiting
                                    };
                                    (ps, stop_comment)
                                } else {
                                    (IssueStatus::Waiting, None)
                                };
                            let _ = state
                                .issue_service
                                .park_issue(&issue.id, park_status, comment, None)
                                .await;
                        }
                    }

                    SystemEvent::Session {
                        session_id,
                        event: SessionEvent::Error { message },
                    } => {
                        stop_map.remove(&session_id);
                        let issues = state
                            .db
                            .list_issues_by_session_id(session_id)
                            .await
                            .unwrap_or_default();
                        for issue in issues {
                            let _ = state
                                .issue_service
                                .fail_issue(&issue.id, message.clone())
                                .await;
                        }
                    }

                    // ── Issue state → session actions ─────────────────────────

                    SystemEvent::Issue(IssueEvent::StatusChanged {
                        issue,
                        to: IssueStatus::InProgress,
                        ..
                    }) => {
                        handle_in_progress(&state, issue).await;
                    }

                    SystemEvent::Issue(IssueEvent::StatusChanged {
                        issue,
                        to: IssueStatus::Cancelled,
                        ..
                    }) => {
                        // Kill the active session if any
                        if let Some(session_id) = issue.session_id {
                            let mut senders = state.msg_senders.lock().await;
                            senders.remove(&session_id);
                            drop(senders);
                            let _ = state
                                .db
                                .update_session_status(session_id, types::SessionStatus::Cancelled)
                                .await;
                        }
                    }

                    _ => {}
                },
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("Issue lifecycle subscriber lagged by {n} messages");
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// # Errors
///
/// Returns an error if the data directory cannot be created, the PID file cannot
/// be written, the database cannot be opened, or the TCP listener cannot bind.
pub async fn run(config: ServerConfig) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir)?;

    let pid = std::process::id();
    std::fs::write(&config.pid_file, format!("{pid}\n"))?;

    let client: Arc<dyn anthropic::AnthropicClient> =
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            Arc::new(anthropic::Client::new(key))
        } else {
            eprintln!(
                "Warning: ANTHROPIC_API_KEY not set — using stub client (responses will be fake)"
            );
            Arc::new(anthropic::StubClient)
        };
    let tools: Vec<Arc<dyn tools::Tool>> = vec![
        Arc::new(tools::ReadTool),
        Arc::new(tools::BashTool),
        Arc::new(tools::WriteTool),
        Arc::new(tools::EditTool),
    ];

    let db_path = config.data_dir.join("ns2.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let (db, hook_store, event_store) = db::connect(&db_url).await?;
    let issue_service = issues::IssueService::with_event_bus(Arc::clone(&db), EventBus::new(1024));
    let event_bus = issue_service.event_bus().clone();

    let state = AppState {
        db,
        issue_service,
        msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
        client,
        tools,
        model: config.model,
        event_bus,
        hook_store,
        event_store,
    };

    // Spawn the hook evaluator background task.
    spawn_hook_evaluator(&state);

    // Spawn the global issue lifecycle subscriber.
    spawn_issue_lifecycle_subscriber(&state);

    // Spawn the timer scheduler background task.
    hooks::timer::spawn_timer_scheduler(&state.event_store, &state.event_bus);

    // Recover orphaned sessions before accepting any connections.
    state.issue_service.orphan_sweep().await;

    let app = build_router(state);

    let addr = format!("127.0.0.1:{}", config.port);
    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on {addr}");

    let pid_file = config.pid_file.clone();
    let server = axum::serve(listener, app);

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    #[cfg(unix)]
    let result = tokio::select! {
        res = server => res.map_err(Error::Io),
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = sigterm.recv() => Ok(()),
    };

    #[cfg(not(unix))]
    let result = tokio::select! {
        res = server => res.map_err(Error::Io),
        _ = tokio::signal::ctrl_c() => Ok(()),
    };

    let _ = std::fs::remove_file(&pid_file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use events::SessionEvent;
    #[allow(unused_imports)]
    use routes::events_route;
    use routes::issue::slugify;
    use routes::session::event_from;
    use std::sync::Arc;
    use tower::ServiceExt;
    use types::{IssueComment, IssueStatus, Session, SessionStatus};
    use uuid::Uuid;

    /// A minimal stub `AnthropicClient` defined locally in server tests.
    /// Does NOT use `harness::StubClient` — server tests must not depend on harness internals.
    struct TestClient;

    #[async_trait]
    impl anthropic::AnthropicClient for TestClient {
        async fn complete(
            &self,
            _request: anthropic::MessageRequest,
        ) -> anthropic::Result<anthropic::MessageResponse> {
            Ok(anthropic::MessageResponse {
                content: vec![types::ContentBlock::Text {
                    text: "stub response".into(),
                }],
                stop_reason: "end_turn".into(),
                input_tokens: 5,
                output_tokens: 4,
            })
        }
    }

    pub async fn test_state() -> AppState {
        let (db, hook_store, event_store) = db::connect("sqlite::memory:").await.unwrap();
        let client = Arc::new(TestClient) as Arc<dyn anthropic::AnthropicClient>;
        let issue_service =
            issues::IssueService::with_event_bus(Arc::clone(&db), EventBus::new(1024));
        let event_bus = issue_service.event_bus().clone();
        AppState {
            db,
            issue_service,
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            client,
            tools: vec![],
            model: "claude-opus-4-5".into(),
            event_bus,
            hook_store,
            event_store,
        }
    }

    async fn test_app() -> Router {
        let state = test_state().await;
        build_router(state)
    }

    async fn test_app_with_state() -> (Router, AppState) {
        let state = test_state().await;
        spawn_hook_evaluator(&state);
        spawn_issue_lifecycle_subscriber(&state);
        let app = build_router(state.clone());
        (app, state)
    }

    async fn response_body_bytes(resp: axum::response::Response) -> bytes::Bytes {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
    }

    // --- /health ---

    #[tokio::test]
    async fn test_health_returns_200() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_health_body() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "ok");
    }

    // --- GET /sessions ---

    #[tokio::test]
    async fn test_list_sessions_empty() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    // --- POST /sessions ---

    fn create_session_req(body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/sessions")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_session_returns_201() {
        let app = test_app().await;
        let resp = app
            .oneshot(create_session_req(&serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_create_session_no_message_status_is_created() {
        let app = test_app().await;
        let resp = app
            .oneshot(create_session_req(&serde_json::json!({})))
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "created");
    }

    #[tokio::test]
    async fn test_create_session_response_has_required_fields() {
        let app = test_app().await;
        let resp = app
            .oneshot(create_session_req(&serde_json::json!({
                "name": "my-session"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["id"].is_string(), "id should be a string");
        assert_eq!(v["name"], "my-session");
        assert!(v["status"].is_string(), "status should be a string");
        assert!(v["created_at"].is_string(), "created_at should be a string");
        assert!(v["updated_at"].is_string(), "updated_at should be a string");
    }

    #[tokio::test]
    async fn test_create_session_default_name() {
        let app = test_app().await;
        let resp = app
            .oneshot(create_session_req(&serde_json::json!({})))
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["name"], "unnamed");
    }

    // --- GET /sessions after creating some ---

    #[tokio::test]
    async fn test_list_sessions_after_create() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(&serde_json::json!({"name": "sess-1"})))
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);

        let list_resp = app
            .oneshot(
                Request::builder()
                    .uri("/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_body_bytes(list_resp).await;
        let sessions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["name"], "sess-1");
    }

    // --- GET /sessions/:id ---

    #[tokio::test]
    async fn test_get_session_by_id() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(&serde_json::json!({"name": "by-id"})))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let get_resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let body = response_body_bytes(get_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], id);
        assert_eq!(v["name"], "by-id");
    }

    #[tokio::test]
    async fn test_get_session_not_found() {
        let app = test_app().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{fake_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- GET /sessions?status=... ---

    #[tokio::test]
    async fn test_list_sessions_filter_by_status() {
        let app = test_app().await;

        app.clone()
            .oneshot(create_session_req(
                &serde_json::json!({"name": "created-sess"}),
            ))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions?status=created")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let sessions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["status"], "created");
    }

    #[tokio::test]
    async fn test_list_sessions_filter_by_status_no_match() {
        let app = test_app().await;

        app.clone()
            .oneshot(create_session_req(&serde_json::json!({})))
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sessions?status=waiting")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let sessions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn test_list_sessions_invalid_status_returns_error() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sessions?status=bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // --- GET /sessions/:id/events ---

    #[tokio::test]
    async fn test_session_events_returns_200() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(&serde_json::json!({"name": "sse-test"})))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_session_events_content_type_is_event_stream() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(&serde_json::json!({})))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.contains("text/event-stream"),
            "expected SSE content-type, got: {ct}"
        );
    }

    // --- POST /sessions/:id/messages ---

    #[tokio::test]
    async fn test_send_message_to_nonexistent_session_returns_404() {
        let app = test_app().await;
        let fake_id = uuid::Uuid::new_v4();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{fake_id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "hello"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_send_message_to_created_session_returns_200() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(&serde_json::json!({"name": "idle"})))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "hello"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_send_message_to_running_session_returns_200() {
        let (app, _state) = test_app_with_state().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(&serde_json::json!({
                "name": "running-sess",
                "initial_message": "start"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "follow up"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_concurrent_send_message_spawns_only_one_harness() {
        let (app, state) = test_app_with_state().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(
                &serde_json::json!({"name": "race-test"}),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();
        let session_id: Uuid = id.parse().unwrap();

        let app1 = app.clone();
        let app2 = app.clone();
        let id1 = id.clone();
        let id2 = id.clone();

        let req1 = Request::builder()
            .method("POST")
            .uri(format!("/sessions/{id1}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({"message": "msg1"})).unwrap(),
            ))
            .unwrap();

        let req2 = Request::builder()
            .method("POST")
            .uri(format!("/sessions/{id2}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({"message": "msg2"})).unwrap(),
            ))
            .unwrap();

        let (r1, r2) = tokio::join!(app1.oneshot(req1), app2.oneshot(req2),);
        assert_eq!(r1.unwrap().status(), StatusCode::OK);
        assert_eq!(r2.unwrap().status(), StatusCode::OK);

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let senders = state.msg_senders.lock().await;
        let sender_count = i32::from(senders.contains_key(&session_id));
        drop(senders);
        assert_eq!(
            sender_count, 1,
            "expected exactly 1 msg sender in msg_senders map, got {sender_count}"
        );
    }

    // --- event_from serialization ---

    #[tokio::test]
    async fn test_event_from_serializes_session_event() {
        use axum::response::{sse::Sse, IntoResponse};
        use std::convert::Infallible;

        let ev = SessionEvent::Done;
        let sse_event = event_from(&ev);

        let stream = futures::stream::once(async move { Ok::<_, Infallible>(sse_event) });
        let resp = Sse::new(stream).into_response();
        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        let data_line = raw
            .lines()
            .find(|l| l.starts_with("data: "))
            .expect("SSE body must contain a data: line");
        let json = &data_line["data: ".len()..];
        let decoded: SessionEvent = serde_json::from_str(json).expect("must deserialize back");
        assert!(matches!(decoded, SessionEvent::Done));
    }

    // --- has_message boundary ---

    #[tokio::test]
    async fn test_empty_initial_message_does_not_spawn_harness() {
        let (app, state) = test_app_with_state().await;

        let resp = app
            .oneshot(create_session_req(
                &serde_json::json!({"initial_message": ""}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let senders = state.msg_senders.lock().await;
        let is_empty = senders.is_empty();
        drop(senders);
        assert!(is_empty, "empty initial_message must not spawn a harness");
    }

    #[tokio::test]
    async fn test_nonempty_initial_message_spawns_harness() {
        let (app, state) = test_app_with_state().await;

        let resp = app
            .oneshot(create_session_req(
                &serde_json::json!({"initial_message": "hello"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let senders = state.msg_senders.lock().await;
        let has_key = senders.contains_key(&session_id);
        drop(senders);
        assert!(has_key, "non-empty initial_message must spawn a harness");
    }

    // --- terminal session history includes SessionDone ---

    #[tokio::test]
    async fn test_waiting_session_events_includes_session_done() {
        let (app, state) = test_app_with_state().await;

        // A session in Waiting status (previously ended a turn) should have a
        // SessionDone event in its history so clients can observe the turn boundary.
        let session = Session {
            id: Uuid::new_v4(),
            name: "done-test".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, SessionStatus::Waiting)
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        let has_session_done = raw.lines().filter(|l| l.starts_with("data: ")).any(|l| {
            let json = &l["data: ".len()..];
            serde_json::from_str::<events::SystemEvent>(json).is_ok_and(|ev| {
                matches!(
                    ev,
                    events::SystemEvent::Session {
                        event: SessionEvent::Done,
                        ..
                    }
                )
            })
        });
        assert!(
            has_session_done,
            "waiting session events must include SessionDone"
        );
    }

    // --- GET /sessions/:id/events?last_turns=N ---

    #[tokio::test]
    async fn test_session_events_last_turns_zero_skips_history() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "turns-test".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        for _ in 0..3 {
            let turn = types::Turn {
                id: Uuid::new_v4(),
                session_id: session.id,
                token_count: Some(10),
                created_at: chrono::Utc::now(),
            };
            state.db.create_turn(&turn).await.unwrap();
            state
                .db
                .create_content_block(
                    turn.id,
                    0,
                    &types::Role::Assistant,
                    &types::ContentBlock::Text {
                        text: "hello".into(),
                    },
                )
                .await
                .unwrap();
        }

        state
            .db
            .update_session_status(session.id, SessionStatus::Waiting)
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={}&last_turns=0", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        let has_turn_started = raw.lines().filter(|l| l.starts_with("data: ")).any(|l| {
            let json = &l["data: ".len()..];
            serde_json::from_str::<events::SystemEvent>(json).is_ok_and(|ev| {
                matches!(
                    ev,
                    events::SystemEvent::Session {
                        event: SessionEvent::TurnStarted { .. },
                        ..
                    }
                )
            })
        });
        assert!(
            !has_turn_started,
            "last_turns=0 should skip all history turns"
        );

        let has_session_done = raw.lines().filter(|l| l.starts_with("data: ")).any(|l| {
            let json = &l["data: ".len()..];
            serde_json::from_str::<events::SystemEvent>(json).is_ok_and(|ev| {
                matches!(
                    ev,
                    events::SystemEvent::Session {
                        event: SessionEvent::Done,
                        ..
                    }
                )
            })
        });
        assert!(
            has_session_done,
            "completed session should still emit SessionDone"
        );
    }

    #[tokio::test]
    async fn test_session_events_last_turns_one_emits_last_turn() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "turns-test-1".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        for i in 0..3 {
            let turn = types::Turn {
                id: Uuid::new_v4(),
                session_id: session.id,
                token_count: Some(10),
                created_at: chrono::Utc::now(),
            };
            state.db.create_turn(&turn).await.unwrap();
            state
                .db
                .create_content_block(
                    turn.id,
                    0,
                    &types::Role::Assistant,
                    &types::ContentBlock::Text {
                        text: format!("turn-{i}"),
                    },
                )
                .await
                .unwrap();
        }

        state
            .db
            .update_session_status(session.id, SessionStatus::Waiting)
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={}&last_turns=1", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        let turn_started_count = raw
            .lines()
            .filter(|l| l.starts_with("data: "))
            .filter(|l| {
                let json = &l["data: ".len()..];
                serde_json::from_str::<events::SystemEvent>(json).is_ok_and(|ev| {
                    matches!(
                        ev,
                        events::SystemEvent::Session {
                            event: SessionEvent::TurnStarted { .. },
                            ..
                        }
                    )
                })
            })
            .count();
        assert_eq!(
            turn_started_count, 1,
            "last_turns=1 should emit exactly 1 turn"
        );

        assert!(raw.contains("turn-2"), "should contain last turn's content");
        assert!(
            !raw.contains("turn-0"),
            "should not contain first turn's content"
        );
        assert!(
            !raw.contains("turn-1"),
            "should not contain second turn's content"
        );
    }

    #[tokio::test]
    async fn test_session_events_no_last_turns_emits_all_history() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "turns-test-all".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        for i in 0..3 {
            let turn = types::Turn {
                id: Uuid::new_v4(),
                session_id: session.id,
                token_count: Some(10),
                created_at: chrono::Utc::now(),
            };
            state.db.create_turn(&turn).await.unwrap();
            state
                .db
                .create_content_block(
                    turn.id,
                    0,
                    &types::Role::Assistant,
                    &types::ContentBlock::Text {
                        text: format!("turn-{i}"),
                    },
                )
                .await
                .unwrap();
        }

        state
            .db
            .update_session_status(session.id, SessionStatus::Waiting)
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        assert!(
            raw.contains("turn-0"),
            "should contain first turn's content"
        );
        assert!(
            raw.contains("turn-1"),
            "should contain second turn's content"
        );
        assert!(raw.contains("turn-2"), "should contain last turn's content");
    }

    // --- GET /events ---

    #[tokio::test]
    async fn test_get_events_returns_200() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_get_events_content_type_is_event_stream() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.contains("text/event-stream"),
            "expected SSE content-type, got: {ct}"
        );
    }

    #[tokio::test]
    async fn test_get_events_session_id_returns_200_and_history() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "events-history-test".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        for i in 0..2 {
            let turn = types::Turn {
                id: Uuid::new_v4(),
                session_id: session.id,
                token_count: Some(10),
                created_at: chrono::Utc::now(),
            };
            state.db.create_turn(&turn).await.unwrap();
            state
                .db
                .create_content_block(
                    turn.id,
                    0,
                    &types::Role::Assistant,
                    &types::ContentBlock::Text {
                        text: format!("turn-{i}"),
                    },
                )
                .await
                .unwrap();
        }

        state
            .db
            .update_session_status(session.id, SessionStatus::Waiting)
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        assert!(raw.contains("turn-0"), "history replay must include turn-0");
        assert!(raw.contains("turn-1"), "history replay must include turn-1");

        // Must include a SystemEvent::Session{...Done} event
        let has_done = raw.lines().filter(|l| l.starts_with("data: ")).any(|l| {
            let json = &l["data: ".len()..];
            serde_json::from_str::<events::SystemEvent>(json).is_ok_and(|ev| {
                matches!(
                    ev,
                    events::SystemEvent::Session {
                        event: events::SessionEvent::Done,
                        ..
                    }
                )
            })
        });
        assert!(
            has_done,
            "/events?session_id must include Done for completed session"
        );
    }

    #[tokio::test]
    async fn test_get_old_session_events_route_returns_404() {
        let app = test_app().await;
        let fake_id = Uuid::new_v4();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{fake_id}/events"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "old /sessions/:id/events route must return 404"
        );
    }

    #[tokio::test]
    async fn test_get_events_types_session_filters_issue_events_live() {
        let (_app, state) = test_app_with_state().await;

        // First subscribe to the event bus
        let mut rx = state.event_bus.subscribe();

        // create_issue will emit IssueEvent::Created
        state
            .issue_service
            .create_issue(issues::CreateIssueInput {
                title: "Extra issue".into(),
                body: "body".into(),
                assignee: None,
                parent_id: None,
                blocked_on: vec![],
                branch: None,
            })
            .await
            .unwrap();

        // Emit a Session event
        let sid = Uuid::new_v4();
        state.event_bus.send(events::SystemEvent::Session {
            session_id: sid,
            event: events::SessionEvent::Done,
        });

        let q = routes::events_route::EventsQuery {
            session_id: None,
            issue_id: None,
            types: Some("session".into()),
            last_turns: None,
        };

        let mut received = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if q.matches(&ev) {
                received.push(ev);
            }
        }

        // Only the session event should be in received (not the issue event)
        assert_eq!(
            received.len(),
            1,
            "only 1 event should pass the types=session filter"
        );
        assert!(
            matches!(received[0], events::SystemEvent::Session { session_id: s, .. } if s == sid),
            "the received event must be the session event"
        );
    }

    #[tokio::test]
    async fn test_waiting_session_sender_still_alive() {
        let (app, state) = test_app_with_state().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(&serde_json::json!({
                "name": "waiting-test",
                "initial_message": "run and wait"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();
        let session_id: Uuid = id.parse().unwrap();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let session = state.db.get_session(session_id).await.unwrap();
            if session.status == SessionStatus::Waiting {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "session did not reach Waiting within 3s; status={}",
                session.status
            );
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "follow-up msg"}))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "waiting session must accept follow-up messages and return 200"
        );
    }

    #[tokio::test]
    async fn test_send_message_to_waiting_session_returns_200() {
        let (app, state) = test_app_with_state().await;

        // A Waiting session has no active harness but can accept new messages —
        // the server should spawn a fresh harness and return 200.
        let session = Session {
            id: Uuid::new_v4(),
            name: "waiting-no-harness".into(),
            status: SessionStatus::Waiting,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, SessionStatus::Waiting)
            .await
            .unwrap();

        {
            let senders = state.msg_senders.lock().await;
            let not_present = !senders.contains_key(&session.id);
            drop(senders);
            assert!(not_present);
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{}/messages", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "resume me"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "waiting session with no active sender must return 200 OK (fresh harness spawned)"
        );
    }

    #[tokio::test]
    async fn test_send_message_to_failed_session_returns_bad_request() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "failed-session".into(),
            status: SessionStatus::Failed,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, SessionStatus::Failed)
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{}/messages", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "should fail"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = resp.status();
        assert!(
            !status.is_success(),
            "failed session must not accept messages; expected non-2xx, got {status}"
        );

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v["error"].is_string(),
            "response body must contain an 'error' field, got: {v}"
        );
    }

    #[tokio::test]
    async fn test_send_message_to_cancelled_session_returns_bad_request() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "cancelled-session".into(),
            status: SessionStatus::Cancelled,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, SessionStatus::Cancelled)
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{}/messages", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "should fail"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = resp.status();
        assert!(
            !status.is_success(),
            "cancelled session must not accept messages; expected non-2xx, got {status}"
        );

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v["error"].is_string(),
            "response body must contain an 'error' field, got: {v}"
        );
    }

    // --- cancel_session on already-terminal sessions returns 400 ---

    #[tokio::test]
    async fn test_cancel_already_failed_session_returns_400() {
        let (app, state) = test_app_with_state().await;

        // Create a session and set it to Failed — a terminal state.
        let session = Session {
            id: Uuid::new_v4(),
            name: "failed-cancel-test".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, SessionStatus::Failed)
            .await
            .unwrap();

        // POST /sessions/:id/cancel on an already-Failed session must return 400.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{}/cancel", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "cancelling an already-failed session must return 400"
        );
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v["error"].is_string(),
            "response body must contain an 'error' field, got: {v}"
        );
    }

    #[tokio::test]
    async fn test_cancel_already_cancelled_session_returns_400() {
        let (app, state) = test_app_with_state().await;

        // Create a session and set it to Cancelled — a terminal state.
        let session = Session {
            id: Uuid::new_v4(),
            name: "cancelled-cancel-test".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, SessionStatus::Cancelled)
            .await
            .unwrap();

        // POST /sessions/:id/cancel on an already-Cancelled session must return 400.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{}/cancel", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "cancelling an already-cancelled session must return 400"
        );
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v["error"].is_string(),
            "response body must contain an 'error' field, got: {v}"
        );
    }

    /// Resuming a Waiting session via POST /sessions/:id/messages must spawn
    /// a harness with the linked issue's ID so the issue watcher is active.
    ///
    /// When the harness runs and emits Done (without a Stopped event), the
    /// linked issue should transition from Running → Waiting, proving the
    /// global issue lifecycle subscriber correctly reacts to session events.
    #[tokio::test]
    async fn test_waiting_session_resume_via_message_spawns_issue_watcher() {
        let (app, state) = test_app_with_state().await;

        // 1. Create a Waiting session.
        let session = Session {
            id: Uuid::new_v4(),
            name: "waiting-resume-test".into(),
            status: SessionStatus::Waiting,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, SessionStatus::Waiting)
            .await
            .unwrap();

        // 2. Create a Running issue linked to this session.
        //    We use Running (not Waiting) so the state change to Waiting is
        //    detectable: harness → Done without Stopped → park_issue(Waiting).
        let issue = types::Issue {
            id: "wt01".to_string(),
            title: "Waiting issue".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: Some(session.id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        // 3. POST a message to the Waiting session — this must spawn the harness
        //    with the linked issue's ID.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{}/messages", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "continue please"}))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "Waiting session must accept messages"
        );

        // 4. The TestClient stub returns immediately, causing the harness to emit Done
        //    (no Stopped event). The issue watcher (if correctly spawned) will park
        //    the issue as Waiting.  The issue should transition Running → Waiting,
        //    proving the watcher was set up.
        let deadline =
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("wt01".to_string()).await.unwrap();
            if fetched.status == types::IssueStatus::Waiting {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "linked issue did not become Waiting within 5s; status={} — \
                 this indicates the issue watcher was not spawned",
                fetched.status
            );
        }

        let issue_final = state.db.get_issue("wt01".to_string()).await.unwrap();
        assert_eq!(
            issue_final.status,
            types::IssueStatus::Waiting,
            "linked issue must be Waiting after harness emits Done (without Stopped), \
             proving the issue watcher was correctly spawned when Waiting session resumed"
        );
    }

    // ─── Issue endpoint tests ─────────────────────────────────────────────────

    fn issue_req(method: &str, uri: &str, body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_issue_returns_201() {
        let app = test_app().await;
        let resp = app
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Fix the bug",
                    "body": "Details here"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_create_issue_response_has_id_and_open_status() {
        let app = test_app().await;
        let resp = app
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Fix the bug",
                    "body": "Details here"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["id"].is_string());
        assert_eq!(v["id"].as_str().unwrap().len(), 4);
        assert_eq!(v["status"], "open");
        assert_eq!(v["title"], "Fix the bug");
    }

    #[tokio::test]
    async fn test_get_issue_not_found_returns_404() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/issues/xxxx")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_issue_returns_created_issue() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "My issue",
                    "body": "Body text"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let get_resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/issues/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let body = response_body_bytes(get_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["title"], "My issue");
        assert_eq!(v["id"], id);
    }

    #[tokio::test]
    async fn test_list_issues_empty() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/issues")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_list_issues_returns_created_issues() {
        let app = test_app().await;
        app.clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Issue One", "body": "B1"
                }),
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Issue Two", "body": "B2"
                }),
            ))
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/issues")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_list_issues_filter_by_status() {
        let app = test_app().await;
        app.clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Open issue", "body": "B"
                }),
            ))
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/issues?status=open")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["status"], "open");
    }

    #[tokio::test]
    async fn test_list_issues_invalid_status_returns_400() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/issues?status=bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_edit_issue_updates_title() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Old title", "body": "B"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let edit_resp = app
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{id}"),
                &serde_json::json!({
                    "title": "New title"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(edit_resp.status(), StatusCode::OK);
        let body = response_body_bytes(edit_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["title"], "New title");
    }

    #[tokio::test]
    async fn test_edit_issue_clears_parent_with_null() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Child", "body": "B", "parent_id": "abc1"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();
        assert_eq!(created["parent_id"], "abc1");

        let edit_resp = app
            .clone()
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{id}"),
                &serde_json::json!({
                    "parent_id": null
                }),
            ))
            .await
            .unwrap();
        assert_eq!(edit_resp.status(), StatusCode::OK);
        let body = response_body_bytes(edit_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v["parent_id"].is_null(),
            "parent_id should be cleared to null"
        );
    }

    #[tokio::test]
    async fn test_edit_issue_absent_parent_leaves_unchanged() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Child", "body": "B", "parent_id": "abc1"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let edit_resp = app
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{id}"),
                &serde_json::json!({
                    "title": "Renamed"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(edit_resp.status(), StatusCode::OK);
        let body = response_body_bytes(edit_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["parent_id"], "abc1", "parent_id should be unchanged");
    }

    #[tokio::test]
    async fn test_add_comment_appends_to_issue() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Issue", "body": "B"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let comment_resp = app
            .oneshot(issue_req(
                "POST",
                &format!("/issues/{id}/comments"),
                &serde_json::json!({
                    "author": "user",
                    "body": "First comment"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(comment_resp.status(), StatusCode::OK);
        let body = response_body_bytes(comment_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let comments = v["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["author"], "user");
        assert_eq!(comments[0]["body"], "First comment");
    }

    #[tokio::test]
    async fn test_start_issue_without_assignee_returns_400() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "No assignee", "body": "B"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let start_resp = app
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();
        assert_eq!(start_resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_start_issue_on_non_open_issue_returns_400() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Has assignee", "body": "B", "assignee": "swe"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        app.clone()
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();

        let second_start = app
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();
        assert_eq!(second_start.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_complete_issue_sets_status_and_adds_comment() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Issue", "body": "B"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let complete_resp = app
            .oneshot(issue_req(
                "POST",
                &format!("/issues/{id}/complete"),
                &serde_json::json!({
                    "comment": "All done"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(complete_resp.status(), StatusCode::OK);
        let body = response_body_bytes(complete_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "completed");
        let comments = v["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["body"], "All done");
    }

    #[tokio::test]
    async fn test_complete_already_completed_issue_returns_400() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Issue", "body": "B"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        app.clone()
            .oneshot(issue_req(
                "POST",
                &format!("/issues/{id}/complete"),
                &serde_json::json!({
                    "comment": "First completion"
                }),
            ))
            .await
            .unwrap();

        let second = app
            .oneshot(issue_req(
                "POST",
                &format!("/issues/{id}/complete"),
                &serde_json::json!({
                    "comment": "Should fail"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    }

    // --- issue auto-completion when session terminates ---

    #[tokio::test]
    async fn test_issue_auto_completes_when_session_succeeds() {
        let (app, state) = test_app_with_state().await;

        let create_resp = app
            .clone()
            .oneshot(issue_req("POST", "/issues", &serde_json::json!({
                "title": "Auto complete test", "body": "body", "assignee": "test-agent-no-disk-def"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{issue_id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
            if issue.status == IssueStatus::Waiting {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue did not transition to waiting within 5 seconds; status={}",
                issue.status
            );
        }
    }

    // --- PATCH /sessions/:id/status ---

    async fn patch_session_status(
        app: Router,
        id: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        app.oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/sessions/{id}/status"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    async fn patch_issue_status(
        app: Router,
        id: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        app.oneshot(issue_req("PATCH", &format!("/issues/{id}/status"), &body))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_patch_session_status_happy_path() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(
                &serde_json::json!({"name": "status-test"}),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let resp = patch_session_status(app, &id, serde_json::json!({"status": "waiting"})).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], id, "response must contain the session id");
        assert_eq!(
            v["status"], "waiting",
            "status must be updated to waiting"
        );
    }

    #[tokio::test]
    async fn test_patch_session_status_not_found_returns_404() {
        let app = test_app().await;
        let fake_id = uuid::Uuid::new_v4();

        let resp = patch_session_status(
            app,
            &fake_id.to_string(),
            serde_json::json!({"status": "running"}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].is_string(), "should contain an error field");
    }

    #[tokio::test]
    async fn test_patch_session_status_invalid_status_returns_400() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(
                &serde_json::json!({"name": "bad-status"}),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let resp = patch_session_status(app, &id, serde_json::json!({"status": "bogus"})).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].is_string(), "should contain an error field");
    }

    #[tokio::test]
    async fn test_patch_session_status_all_valid_statuses() {
        for status in ["created", "running", "waiting", "failed", "cancelled"] {
            let app = test_app().await;

            let create_resp = app
                .clone()
                .oneshot(create_session_req(
                    &serde_json::json!({"name": format!("sess-{status}")}),
                ))
                .await
                .unwrap();
            let body = response_body_bytes(create_resp).await;
            let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let id = created["id"].as_str().unwrap().to_owned();

            let resp = patch_session_status(app, &id, serde_json::json!({"status": status})).await;
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "status '{status}' should be accepted"
            );
            let body = response_body_bytes(resp).await;
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["status"], status);
        }
    }

    #[tokio::test]
    async fn test_list_issues_filter_by_blocked_on() {
        let app = test_app().await;
        let blocker = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Blocker", "body": "B"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(blocker).await;
        let blocker_id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        app.clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Blocked issue", "body": "B",
                    "blocked_on": [&blocker_id]
                }),
            ))
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/issues?blocked_on={blocker_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["title"], "Blocked issue");
    }

    // ─── Orphan sweep tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_orphan_sweep_marks_running_session_failed() {
        let state = test_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "orphan".into(),
            status: SessionStatus::Running,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        state.issue_service.orphan_sweep().await;

        let fetched = state.db.get_session(session.id).await.unwrap();
        assert_eq!(
            fetched.status,
            SessionStatus::Failed,
            "orphan sweep must mark a running session as failed"
        );
    }

    #[tokio::test]
    async fn test_orphan_sweep_marks_linked_issue_failed_with_comment() {
        let state = test_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "orphan-with-issue".into(),
            status: SessionStatus::Running,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        let issue = types::Issue {
            id: "ab12".into(),
            title: "Test issue".into(),
            body: "body".into(),
            status: types::IssueStatus::InProgress,
            branch: String::new(),
            assignee: None,
            session_id: Some(session.id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        state.issue_service.orphan_sweep().await;

        let fetched_issue = state.db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(
            fetched_issue.status,
            types::IssueStatus::Failed,
            "orphan sweep must mark the linked issue as failed"
        );

        assert_eq!(
            fetched_issue.comments.len(),
            1,
            "orphan sweep must append exactly one comment"
        );
        let comment = &fetched_issue.comments[0];
        assert_eq!(comment.author, "system");
        assert!(
            comment.body.contains("session lost on server restart"),
            "comment body must contain 'session lost on server restart', got: '{}'",
            comment.body
        );
    }

    #[tokio::test]
    async fn test_orphan_sweep_ignores_non_running_sessions() {
        let state = test_state().await;

        let statuses = [
            SessionStatus::Waiting,
            SessionStatus::Cancelled,
            SessionStatus::Created,
            SessionStatus::Failed,
        ];

        let mut session_ids = Vec::new();
        for (i, status) in statuses.iter().enumerate() {
            let session = Session {
                id: Uuid::new_v4(),
                name: format!("sess-{i}"),
                status: status.clone(),
                agent: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            session_ids.push((session.id, status.clone()));
            state.db.create_session(&session).await.unwrap();
        }

        state.issue_service.orphan_sweep().await;

        for (id, original_status) in &session_ids {
            let fetched = state.db.get_session(*id).await.unwrap();
            assert_eq!(
                fetched.status,
                *original_status,
                "session with original status '{original_status}' must not have been changed by orphan sweep"
            );
        }
    }

    #[tokio::test]
    async fn test_orphan_sweep_no_linked_issue_does_not_error() {
        let state = test_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "orphan-no-issue".into(),
            status: SessionStatus::Running,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        state.issue_service.orphan_sweep().await;

        let fetched = state.db.get_session(session.id).await.unwrap();
        assert_eq!(
            fetched.status,
            SessionStatus::Failed,
            "session with no linked issue must still be marked failed"
        );
    }

    // ─── P2: multi-issue-per-session ─────────────────────────────────────────

    /// When two issues are linked to the same session and the session emits Done
    /// (without a Stopped event), both issues must transition from Running to Waiting.
    #[tokio::test]
    async fn test_multiple_issues_per_session_all_transition_on_done() {
        use types::{IssueStatus, Session, SessionStatus};

        let state = test_state().await;

        // Create a single session in Running status — orphan_sweep only picks up Running sessions.
        let session = Session {
            id: Uuid::new_v4(),
            name: "multi-issue-session".into(),
            status: SessionStatus::Running,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        // Create two issues, both linked to the same session.
        let issue1 = types::Issue {
            id: "mi01".to_string(),
            title: "Issue A".to_string(),
            body: "body".to_string(),
            status: IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: Some(session.id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let issue2 = types::Issue {
            id: "mi02".to_string(),
            title: "Issue B".to_string(),
            body: "body".to_string(),
            status: IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: Some(session.id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue1).await.unwrap();
        state.db.create_issue(&issue2).await.unwrap();

        // Spawn harness for issue1; we pass issue1's id.
        // issue2 is also linked to the same session via session_id but we don't
        // pass its id to spawn_harness_sync — that's the realistic scenario where
        // orphan_sweep is used. Here we verify the subscriber loop in orphan_sweep
        // (list_issues_by_session_id) rather than the harness watcher.
        //
        // We test the orphan_sweep path: after a server restart, both issues linked
        // to the same Running session must transition to Failed.
        state.issue_service.orphan_sweep().await;

        let fetched1 = state.db.get_issue("mi01".to_string()).await.unwrap();
        let fetched2 = state.db.get_issue("mi02".to_string()).await.unwrap();

        assert_eq!(
            fetched1.status,
            IssueStatus::Failed,
            "issue1 must be Failed after orphan sweep"
        );
        assert_eq!(
            fetched2.status,
            IssueStatus::Failed,
            "issue2 (also linked to the same session) must also be Failed after orphan sweep"
        );

        // Both must have the system comment.
        assert!(
            fetched1.comments.iter().any(|c| c.body.contains("session lost on server restart")),
            "issue1 must have 'session lost' comment"
        );
        assert!(
            fetched2.comments.iter().any(|c| c.body.contains("session lost on server restart")),
            "issue2 must have 'session lost' comment"
        );
    }

    // ─── P3: handle_in_progress silent-skip when session is Running ───────────

    /// When an issue already has a session in Running state, emitting a Done event
    /// that would normally spawn a new harness must NOT increase the `msg_senders` count.
    /// (The issue watcher is already active for the running session.)
    #[tokio::test]
    async fn test_handle_in_progress_does_not_spawn_when_session_already_running() {
        use types::{IssueStatus, Session, SessionStatus};

        let (app, state) = test_app_with_state().await;

        // Create a Running session.
        let running_session_id = Uuid::new_v4();
        let running_session = Session {
            id: running_session_id,
            name: "already-running".into(),
            status: SessionStatus::Running,
            agent: Some("test-agent".into()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&running_session).await.unwrap();

        // Create an issue in Running state with this session.
        let issue = types::Issue {
            id: "ip01".to_string(),
            title: "Already Running Issue".to_string(),
            body: "body".to_string(),
            status: IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("test-agent".to_string()),
            session_id: Some(running_session_id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        // Record the current msg_senders count (should be 0 for this session).
        let initial_count = {
            let senders = state.msg_senders.lock().await;
            let count = senders.len();
            drop(senders);
            count
        };

        // Attempting PATCH /issues/ip01/status with {"status": "in_progress"}
        // while the session is Running must return 400 (bad request) and must NOT
        // spawn a new harness.
        let resp = app
            .oneshot(issue_req(
                "PATCH",
                "/issues/ip01/status",
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "in_progress on a Running session issue must return 400"
        );

        // msg_senders must not have grown.
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        let final_count = {
            let senders = state.msg_senders.lock().await;
            let count = senders.len();
            drop(senders);
            count
        };

        assert_eq!(
            final_count, initial_count,
            "msg_senders must not increase when in_progress is rejected for a Running-session issue"
        );
    }

    // ─── P4: stop_map not cleared on Error ───────────────────────────────────

    /// After `SessionEvent::Error`, the global subscriber removes the session's
    /// entry from `stop_map`. If Done arrives later for the same session,
    /// the issue must transition to Waiting (not Completed), proving
    /// the `stop_map` was cleared by the Error handler.
    #[tokio::test]
    async fn test_error_clears_stop_map_so_done_produces_waiting() {
        use events::{SessionEvent, StopEventStatus, SystemEvent};
        use types::{IssueStatus, Session, SessionStatus};

        let state = test_state().await;
        spawn_issue_lifecycle_subscriber(&state);

        // Create a session and an issue linked to it.
        let session = Session {
            id: Uuid::new_v4(),
            name: "error-clears-stop-map".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        let issue = types::Issue {
            id: "ecsm01".to_string(),
            title: "Error clears stop map".to_string(),
            body: "body".to_string(),
            status: IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: Some(session.id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Stopped{Complete} — would mark the issue Completed on Done
        // if the stop_map entry were retained.
        state.event_bus.send(SystemEvent::Session {
            session_id: session.id,
            event: SessionEvent::Stopped {
                status: StopEventStatus::Complete,
                comment: None,
            },
        });

        // Emit Error — global subscriber should fail the issue AND clear the
        // stop_map entry for this session.
        state.event_bus.send(SystemEvent::Session {
            session_id: session.id,
            event: SessionEvent::Error {
                message: "simulated error".into(),
            },
        });

        // Wait for the issue to become Failed.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("ecsm01".to_string()).await.unwrap();
            if fetched.status == IssueStatus::Failed {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue did not become Failed within 3s (status={})",
                fetched.status
            );
        }

        // Reset issue back to InProgress so it can transition again.
        let mut reset_issue = state.db.get_issue("ecsm01".to_string()).await.unwrap();
        reset_issue.status = IssueStatus::InProgress;
        state.db.update_issue(&reset_issue).await.unwrap();

        // Now emit Done WITHOUT a preceding Stopped for this session.
        // Because Error already cleared the stop_map entry, the subscriber
        // must park the issue as Waiting (not Completed).
        state.event_bus.send(SystemEvent::Session {
            session_id: session.id,
            event: SessionEvent::Done,
        });

        let deadline2 = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("ecsm01".to_string()).await.unwrap();
            if fetched.status == IssueStatus::Waiting {
                break;
            }
            assert_ne!(
                fetched.status,
                IssueStatus::Completed,
                "issue became Completed — stop_map entry was not cleared by Error"
            );
            assert!(
                tokio::time::Instant::now() <= deadline2,
                "issue did not become Waiting within 3s after Done (status={})",
                fetched.status
            );
        }

        let final_issue = state.db.get_issue("ecsm01".to_string()).await.unwrap();
        assert_eq!(
            final_issue.status,
            IssueStatus::Waiting,
            "Done after Error must produce Waiting, not Completed — stop_map must be cleared"
        );
    }

    // ─── POST /issues/:id/reopen tests ───────────────────────────────────────

    async fn create_issue_with_status(state: &AppState, status: IssueStatus) -> String {
        let now = chrono::Utc::now();
        let issue = types::Issue {
            id: routes::issue::generate_issue_id_for_test(),
            title: "Test issue".into(),
            body: "body".into(),
            status,
            branch: String::new(),
            assignee: None,
            session_id: Some(Uuid::new_v4()),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![IssueComment {
                author: "system".into(),
                body: "session lost on server restart".into(),
                created_at: now,
            }],
            created_at: now,
            updated_at: now,
        };
        state.db.create_issue(&issue).await.unwrap();
        issue.id
    }

    #[tokio::test]
    async fn test_reopen_failed_issue_returns_200_and_transitions_to_open() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Failed).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/reopen"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "open", "status must become open");
        assert!(
            v["session_id"].is_null(),
            "session_id must be cleared to null"
        );
        let comments = v["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1, "existing comment must be preserved");
        assert_eq!(comments[0]["author"], "system");
        assert_eq!(comments[0]["body"], "session lost on server restart");
    }

    #[tokio::test]
    async fn test_reopen_open_issue_returns_400() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Open).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/reopen"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v["error"].is_string(),
            "response must contain 'error' field"
        );
        let err_msg = v["error"].as_str().unwrap();
        assert!(
            err_msg.contains(&id),
            "error message must contain the issue id, got: {err_msg}"
        );
        assert!(
            err_msg.contains("cannot reopen"),
            "error message must mention 'cannot reopen', got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_reopen_running_issue_returns_400() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::InProgress).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/reopen"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].is_string());
        let err_msg = v["error"].as_str().unwrap();
        assert!(
            err_msg.contains("cannot reopen"),
            "error must mention 'cannot reopen', got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_reopen_completed_issue_keeps_session_id() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Completed).await;

        let original = state.db.get_issue(id.clone()).await.unwrap();
        let original_session_id = original.session_id.expect("should have a session_id");

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/reopen"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "open", "status must become open");
        assert_eq!(
            v["session_id"].as_str().unwrap(),
            original_session_id.to_string(),
            "session_id must be kept when reopening a completed issue"
        );
        let comments = v["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1, "existing comment must be preserved");
    }

    #[tokio::test]
    async fn test_reopen_failed_issue_clears_session_id() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Failed).await;

        let original = state.db.get_issue(id.clone()).await.unwrap();
        assert!(
            original.session_id.is_some(),
            "test setup: issue should have a session_id"
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/reopen"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "open", "status must become open");
        assert!(
            v["session_id"].is_null(),
            "session_id must be cleared for failed issues"
        );
    }

    #[tokio::test]
    async fn test_reopen_with_comment_appends_comment_before_status_change() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Failed).await;

        let resp = app
            .oneshot(issue_req(
                "POST",
                &format!("/issues/{id}/reopen"),
                &serde_json::json!({ "comment": "the tests were failing because of X" }),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "open");

        let comments = v["comments"].as_array().unwrap();
        assert_eq!(
            comments.len(),
            2,
            "should have original comment plus new one"
        );

        let new_comment = &comments[comments.len() - 1];
        assert_eq!(new_comment["author"], "user");
        assert_eq!(new_comment["body"], "the tests were failing because of X");
    }

    #[tokio::test]
    async fn test_reopen_without_comment_adds_no_comment() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Failed).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/reopen"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let comments = v["comments"].as_array().unwrap();
        assert_eq!(
            comments.len(),
            1,
            "no new comment should be added when no comment provided"
        );
    }

    // ─── start_issue initial message includes comment history ────────────────

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_start_issue_initial_message_no_comments() {
        use std::sync::Mutex;

        struct CapturingClient {
            captured: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl anthropic::AnthropicClient for CapturingClient {
            async fn complete(
                &self,
                request: anthropic::MessageRequest,
            ) -> anthropic::Result<anthropic::MessageResponse> {
                if let Some((_, blocks)) = request
                    .messages
                    .iter()
                    .find(|(role, _)| matches!(role, types::Role::User))
                {
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| {
                            if let types::ContentBlock::Text { text } = b {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    self.captured.lock().unwrap().push(text);
                }
                Ok(anthropic::MessageResponse {
                    content: vec![types::ContentBlock::Text { text: "ok".into() }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 1,
                    output_tokens: 1,
                })
            }
        }

        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let (db, hook_store, event_store) = db::connect("sqlite::memory:").await.unwrap();
        let client = Arc::new(CapturingClient {
            captured: Arc::clone(&captured),
        }) as Arc<dyn anthropic::AnthropicClient>;
        let issue_service =
            issues::IssueService::with_event_bus(Arc::clone(&db), EventBus::new(1024));
        let event_bus = issue_service.event_bus().clone();
        let state = AppState {
            db,
            issue_service,
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            client,
            tools: vec![],
            model: "claude-opus-4-5".into(),
            event_bus,
            hook_store,
            event_store,
        };
        spawn_issue_lifecycle_subscriber(&state);
        let app = build_router(state.clone());

        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "My Title",
                    "body": "My Body",
                    "assignee": "test-agent"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{issue_id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            if !captured.lock().unwrap().is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "capturing client was never called"
            );
        }

        let msgs = captured.lock().unwrap().clone();
        let first_msg = &msgs[0];
        assert_eq!(
            first_msg, "My Title\n\nMy Body",
            "with no comments, initial message should be exactly 'title\\n\\nbody', got: {first_msg:?}"
        );
        assert!(
            !first_msg.contains("Issue History"),
            "no comments → no '# Issue History' section"
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_start_issue_initial_message_includes_comments() {
        use std::sync::Mutex;

        struct CapturingClient {
            captured: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl anthropic::AnthropicClient for CapturingClient {
            async fn complete(
                &self,
                request: anthropic::MessageRequest,
            ) -> anthropic::Result<anthropic::MessageResponse> {
                if let Some((_, blocks)) = request
                    .messages
                    .iter()
                    .find(|(role, _)| matches!(role, types::Role::User))
                {
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| {
                            if let types::ContentBlock::Text { text } = b {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    self.captured.lock().unwrap().push(text);
                }
                Ok(anthropic::MessageResponse {
                    content: vec![types::ContentBlock::Text { text: "ok".into() }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 1,
                    output_tokens: 1,
                })
            }
        }

        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let (db, hook_store, event_store) = db::connect("sqlite::memory:").await.unwrap();
        let client = Arc::new(CapturingClient {
            captured: Arc::clone(&captured),
        }) as Arc<dyn anthropic::AnthropicClient>;
        let issue_service =
            issues::IssueService::with_event_bus(Arc::clone(&db), EventBus::new(1024));
        let event_bus = issue_service.event_bus().clone();
        let state = AppState {
            db,
            issue_service,
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            client,
            tools: vec![],
            model: "claude-opus-4-5".into(),
            event_bus,
            hook_store,
            event_store,
        };
        spawn_issue_lifecycle_subscriber(&state);
        let app = build_router(state.clone());

        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "PM Task",
                    "body": "Do the thing",
                    "assignee": "product-manager"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(issue_req(
                "POST",
                &format!("/issues/{issue_id}/comments"),
                &serde_json::json!({
                    "author": "swe",
                    "body": "Slice 1 done"
                }),
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(issue_req(
                "POST",
                &format!("/issues/{issue_id}/comments"),
                &serde_json::json!({
                    "author": "system",
                    "body": "session lost on server restart"
                }),
            ))
            .await
            .unwrap();

        app.clone()
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{issue_id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            if !captured.lock().unwrap().is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "capturing client was never called"
            );
        }

        let msgs = captured.lock().unwrap().clone();
        let first_msg = &msgs[0];

        assert!(
            first_msg.starts_with("PM Task\n\nDo the thing"),
            "initial message must start with title\\n\\nbody, got: {first_msg:?}"
        );
        assert!(
            first_msg.contains("# Issue History"),
            "initial message must contain '# Issue History', got: {first_msg:?}"
        );
        assert!(
            first_msg.contains("Slice 1 done"),
            "initial message must contain first comment body, got: {first_msg:?}"
        );
        assert!(
            first_msg.contains("session lost on server restart"),
            "initial message must contain second comment body, got: {first_msg:?}"
        );
        assert!(
            first_msg.contains("swe"),
            "initial message must include comment author 'swe', got: {first_msg:?}"
        );
        assert!(
            first_msg.contains("system"),
            "initial message must include comment author 'system', got: {first_msg:?}"
        );
    }

    #[tokio::test]
    async fn test_reopen_nonexistent_issue_returns_404() {
        let app = test_app().await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/issues/zzzz/reopen")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ─── issue_watcher: session done transitions issue to Waiting ────────────

    #[tokio::test]
    async fn test_issue_watcher_posts_final_turn_as_comment_on_session_done() {
        // Previously this test verified auto-comment behaviour. Now the issue
        // watcher only posts a comment when the agent explicitly calls the
        // `stop` tool with a comment. Without a stop signal, the issue
        // transitions to `Waiting` with no new comment.
        let (app, state) = test_app_with_state().await;

        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Watcher waiting test",
                    "body": "Please respond",
                    "assignee": "swe-agent"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{issue_id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
            if issue.status == IssueStatus::Waiting {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue did not transition to waiting within 5 seconds; status={}",
                issue.status
            );
        }

        let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Waiting);
        // No auto-comment added — only the agent calling stop with a comment produces a comment.
        let agent_comments: Vec<_> = issue
            .comments
            .iter()
            .filter(|c| c.author == "swe-agent")
            .collect();
        assert!(
            agent_comments.is_empty(),
            "expected no auto-comment when stop tool is not called, got: {agent_comments:?}",
        );
    }

    #[tokio::test]
    async fn test_issue_watcher_posts_error_as_system_comment_on_error() {
        use async_trait::async_trait;

        struct ErrorClient;

        #[async_trait]
        impl anthropic::AnthropicClient for ErrorClient {
            async fn complete(
                &self,
                _request: anthropic::MessageRequest,
            ) -> anthropic::Result<anthropic::MessageResponse> {
                Err(anthropic::Error::Api {
                    status: 500,
                    message: "simulated api failure".into(),
                })
            }
        }

        let (db, hook_store, event_store) = db::connect("sqlite::memory:").await.unwrap();
        let client = Arc::new(ErrorClient) as Arc<dyn anthropic::AnthropicClient>;
        let issue_service =
            issues::IssueService::with_event_bus(Arc::clone(&db), EventBus::new(1024));
        let event_bus = issue_service.event_bus().clone();
        let state = AppState {
            db,
            issue_service,
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            client,
            tools: vec![],
            model: "claude-opus-4-5".into(),
            event_bus,
            hook_store,
            event_store,
        };
        spawn_issue_lifecycle_subscriber(&state);
        let app = build_router(state.clone());

        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Error test issue",
                    "body": "trigger an error",
                    "assignee": "swe-agent"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{issue_id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
            if issue.status == IssueStatus::Failed {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue did not reach Failed within 5 seconds; status={}",
                issue.status
            );
        }

        let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Failed);

        let system_comments: Vec<_> = issue
            .comments
            .iter()
            .filter(|c| c.author == "system")
            .collect();
        assert!(
            !system_comments.is_empty(),
            "expected at least one 'system' comment, got: {:?}",
            issue.comments
        );

        let has_error_text = system_comments
            .iter()
            .any(|c| c.body.contains("simulated api failure"));
        assert!(
            has_error_text,
            "expected system comment containing error message, got: {:?}",
            system_comments.iter().map(|c| &c.body).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_issue_watcher_only_posts_last_turn_text() {
        use chrono::Utc;

        let state = test_state().await;

        let now = chrono::Utc::now();
        let issue = types::Issue {
            id: "tt01".to_string(),
            title: "Multi-turn test".to_string(),
            body: "body".to_string(),
            status: IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: None,
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: now,
            updated_at: now,
        };
        state.db.create_issue(&issue).await.unwrap();

        let (tx, _rx) = tokio::sync::broadcast::channel::<SessionEvent>(256);
        let mut rx = tx.subscribe();
        let db_watch = Arc::clone(&state.db);
        let issue_id = "tt01".to_string();

        tokio::spawn(async move {
            let mut current_turn_text = String::new();
            let mut last_turn_text = String::new();
            while let Ok(event) = rx.recv().await {
                match event {
                    SessionEvent::ContentBlockDelta {
                        delta: types::ContentBlockDelta::TextDelta { text },
                        ..
                    } => {
                        current_turn_text.push_str(&text);
                    }
                    SessionEvent::TurnDone { .. } if !current_turn_text.is_empty() => {
                        last_turn_text = std::mem::take(&mut current_turn_text);
                    }
                    SessionEvent::Done => {
                        if let Ok(mut issue) = db_watch.get_issue(issue_id.clone()).await {
                            if !last_turn_text.is_empty() {
                                let author = issue
                                    .assignee
                                    .clone()
                                    .unwrap_or_else(|| "agent".to_string());
                                issue.comments.push(IssueComment {
                                    author,
                                    created_at: Utc::now(),
                                    body: last_turn_text.clone(),
                                });
                            }
                            issue.status = IssueStatus::Completed;
                            issue.updated_at = Utc::now();
                            let _ = db_watch.update_issue(&issue).await;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        });

        let _session_id = Uuid::new_v4();
        let turn_id1 = Uuid::new_v4();
        let turn_id2 = Uuid::new_v4();

        tx.send(SessionEvent::ContentBlockDelta {
            turn_id: turn_id1,
            index: 0,
            delta: types::ContentBlockDelta::TextDelta {
                text: "first turn text".into(),
            },
        })
        .unwrap();
        tx.send(SessionEvent::TurnDone { turn_id: turn_id1 })
            .unwrap();

        tx.send(SessionEvent::ContentBlockDelta {
            turn_id: turn_id2,
            index: 0,
            delta: types::ContentBlockDelta::TextDelta {
                text: "second turn text".into(),
            },
        })
        .unwrap();
        tx.send(SessionEvent::TurnDone { turn_id: turn_id2 })
            .unwrap();

        tx.send(SessionEvent::Done).unwrap();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("tt01".to_string()).await.unwrap();
            if fetched.status == IssueStatus::Completed {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue did not complete within 3s"
            );
        }

        let fetched = state.db.get_issue("tt01".to_string()).await.unwrap();

        assert_eq!(fetched.comments.len(), 1, "expected exactly 1 comment");
        let comment = &fetched.comments[0];
        assert_eq!(comment.author, "bot");
        assert_eq!(
            comment.body, "second turn text",
            "comment must contain only the last turn text; got: '{}'",
            comment.body
        );
        assert!(
            !comment.body.contains("first turn text"),
            "comment must NOT contain first turn text"
        );
    }

    // ─── GET /sessions/:id/last_text tests ───────────────────────────────────

    #[tokio::test]
    async fn test_session_last_text_no_turns_returns_null() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "last-text-test".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{}/last_text", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["text"].is_null(), "no turns should return null text");
    }

    #[tokio::test]
    async fn test_session_last_text_returns_last_assistant_text() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "last-text-test-2".into(),
            status: SessionStatus::Running,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        let turn1 = types::Turn {
            id: Uuid::new_v4(),
            session_id: session.id,
            token_count: Some(10),
            created_at: chrono::Utc::now(),
        };
        state.db.create_turn(&turn1).await.unwrap();
        state
            .db
            .create_content_block(
                turn1.id,
                0,
                &types::Role::Assistant,
                &types::ContentBlock::Text {
                    text: "first turn text".into(),
                },
            )
            .await
            .unwrap();

        let turn2 = types::Turn {
            id: Uuid::new_v4(),
            session_id: session.id,
            token_count: Some(10),
            created_at: chrono::Utc::now(),
        };
        state.db.create_turn(&turn2).await.unwrap();
        state
            .db
            .create_content_block(
                turn2.id,
                0,
                &types::Role::Assistant,
                &types::ContentBlock::Text {
                    text: "second turn text".into(),
                },
            )
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{}/last_text", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["text"].as_str().unwrap(),
            "second turn text",
            "should return the last assistant text block"
        );
    }

    #[tokio::test]
    async fn test_session_last_text_skips_tool_use_blocks() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "last-text-tool".into(),
            status: SessionStatus::Running,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        let turn = types::Turn {
            id: Uuid::new_v4(),
            session_id: session.id,
            token_count: Some(10),
            created_at: chrono::Utc::now(),
        };
        state.db.create_turn(&turn).await.unwrap();

        state
            .db
            .create_content_block(
                turn.id,
                0,
                &types::Role::Assistant,
                &types::ContentBlock::Text {
                    text: "some text before tools".into(),
                },
            )
            .await
            .unwrap();

        state
            .db
            .create_content_block(
                turn.id,
                1,
                &types::Role::Assistant,
                &types::ContentBlock::ToolUse {
                    id: "tool-1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"cmd": "ls"}),
                },
            )
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{}/last_text", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["text"].as_str().unwrap(),
            "some text before tools",
            "should return text block even when followed by tool use"
        );
    }

    #[tokio::test]
    async fn test_session_last_text_not_found_returns_404() {
        let app = test_app().await;
        let fake_id = Uuid::new_v4();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{fake_id}/last_text"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ─── Slug function tests ─────────────────────────────────────────────────

    #[test]
    fn test_slugify_simple_title() {
        assert_eq!(slugify("Fix the bug"), "fix-the-bug");
    }

    #[test]
    fn test_slugify_capitals() {
        assert_eq!(slugify("My Feature Request"), "my-feature-request");
    }

    #[test]
    fn test_slugify_slashes() {
        assert_eq!(slugify("feat/new-feature"), "feat-new-feature");
    }

    #[test]
    fn test_slugify_consecutive_specials() {
        assert_eq!(slugify("foo--bar"), "foo-bar");
        assert_eq!(slugify("foo  bar"), "foo-bar");
        assert_eq!(slugify("a!@#b"), "a-b");
    }

    #[test]
    fn test_slugify_leading_trailing() {
        assert_eq!(slugify("  leading and trailing  "), "leading-and-trailing");
        assert_eq!(slugify("--leading--"), "leading");
    }

    #[test]
    fn test_slugify_numbers() {
        assert_eq!(slugify("issue 42 fix"), "issue-42-fix");
    }

    #[test]
    fn test_slugify_mixed_specials() {
        assert_eq!(slugify("Hello, World! (2024)"), "hello-world-2024");
    }

    // ─── Branch auto-assignment integration tests ─────────────────────────────

    #[tokio::test]
    async fn test_create_issue_no_parent_no_branch_generates_slug() {
        let app = test_app().await;
        let resp = app
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Fix the Bug",
                    "body": "Details here"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = v["id"].as_str().unwrap();
        let branch = v["branch"].as_str().unwrap();
        assert!(
            branch.starts_with(id),
            "branch '{branch}' should start with id '{id}'"
        );
        assert!(
            branch.contains("fix-the-bug"),
            "branch '{branch}' should contain slugified title"
        );
        assert_eq!(branch, format!("{id}-fix-the-bug"));
    }

    #[tokio::test]
    async fn test_create_child_issue_inherits_parent_branch() {
        let app = test_app().await;

        let parent_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Parent Issue",
                    "body": "Parent body"
                }),
            ))
            .await
            .unwrap();
        let parent_body = response_body_bytes(parent_resp).await;
        let parent: serde_json::Value = serde_json::from_slice(&parent_body).unwrap();
        let parent_id = parent["id"].as_str().unwrap();
        let parent_branch = parent["branch"].as_str().unwrap();

        let child_resp = app
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Child Issue",
                    "body": "Child body",
                    "parent_id": parent_id
                }),
            ))
            .await
            .unwrap();
        assert_eq!(child_resp.status(), StatusCode::CREATED);
        let child_body = response_body_bytes(child_resp).await;
        let child: serde_json::Value = serde_json::from_slice(&child_body).unwrap();
        let child_branch = child["branch"].as_str().unwrap();
        assert_eq!(
            child_branch, parent_branch,
            "child branch '{child_branch}' should equal parent branch '{parent_branch}'"
        );
    }

    #[tokio::test]
    async fn test_create_issue_explicit_branch_stored_as_is() {
        let app = test_app().await;
        let resp = app
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "My Issue",
                    "body": "Body",
                    "branch": "my-custom-branch"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["branch"], "my-custom-branch");
    }

    #[tokio::test]
    async fn test_edit_issue_branch_updates_branch() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "My Issue",
                    "body": "Body"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let edit_resp = app
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{id}"),
                &serde_json::json!({
                    "branch": "updated-branch"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(edit_resp.status(), StatusCode::OK);
        let body = response_body_bytes(edit_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["branch"], "updated-branch");
    }

    #[tokio::test]
    async fn test_get_issue_includes_branch_in_json() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Branch Test",
                    "body": "Body",
                    "branch": "feature/xyz"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let get_resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/issues/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let body = response_body_bytes(get_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v.get("branch").is_some(),
            "GET /issues/:id response must include 'branch' field"
        );
        assert_eq!(v["branch"], "feature/xyz");
    }

    #[tokio::test]
    async fn test_issue_watcher_no_comment_when_no_text_content() {
        use chrono::Utc;

        let state = test_state().await;

        let now = chrono::Utc::now();
        let issue = types::Issue {
            id: "nt01".to_string(),
            title: "No text test".to_string(),
            body: "body".to_string(),
            status: IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: None,
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: now,
            updated_at: now,
        };
        state.db.create_issue(&issue).await.unwrap();

        let (tx, _rx) = tokio::sync::broadcast::channel::<SessionEvent>(256);
        let mut rx = tx.subscribe();
        let db_watch = Arc::clone(&state.db);
        let issue_id = "nt01".to_string();

        tokio::spawn(async move {
            let mut current_turn_text = String::new();
            let mut last_turn_text = String::new();
            while let Ok(event) = rx.recv().await {
                match event {
                    SessionEvent::ContentBlockDelta {
                        delta: types::ContentBlockDelta::TextDelta { text },
                        ..
                    } => {
                        current_turn_text.push_str(&text);
                    }
                    SessionEvent::TurnDone { .. } if !current_turn_text.is_empty() => {
                        last_turn_text = std::mem::take(&mut current_turn_text);
                    }
                    SessionEvent::Done => {
                        if let Ok(mut issue) = db_watch.get_issue(issue_id.clone()).await {
                            if !last_turn_text.is_empty() {
                                let author = issue
                                    .assignee
                                    .clone()
                                    .unwrap_or_else(|| "agent".to_string());
                                issue.comments.push(IssueComment {
                                    author,
                                    created_at: Utc::now(),
                                    body: last_turn_text.clone(),
                                });
                            }
                            issue.status = IssueStatus::Completed;
                            issue.updated_at = Utc::now();
                            let _ = db_watch.update_issue(&issue).await;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        });

        let _session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();

        tx.send(SessionEvent::TurnDone { turn_id }).unwrap();
        tx.send(SessionEvent::Done).unwrap();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("nt01".to_string()).await.unwrap();
            if fetched.status == IssueStatus::Completed {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue did not complete within 3s"
            );
        }

        let fetched = state.db.get_issue("nt01".to_string()).await.unwrap();
        assert_eq!(fetched.status, IssueStatus::Completed);
        assert!(
            fetched.comments.is_empty(),
            "expected no comments when session has no text content, got: {:?}",
            fetched.comments
        );
    }

    // ─── /hooks CRUD integration tests ───────────────────────────────────────

    fn hook_req(method: &str, uri: &str, body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_hook_returns_201() {
        let app = test_app().await;
        let resp = app
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "test-hook",
                    "event_names": ["issue.status_changed"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "abc1" },
                        "body": "Status changed"
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_create_hook_response_has_id_and_name() {
        let app = test_app().await;
        let resp = app
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "my-hook",
                    "event_names": ["issue.created"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "watcher" },
                        "body": "Created"
                    }
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["id"].is_string());
        assert_eq!(v["id"].as_str().unwrap().len(), 4);
        assert_eq!(v["name"], "my-hook");
        assert_eq!(v["enabled"], true);
    }

    #[tokio::test]
    async fn test_list_hooks_empty() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/hooks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_list_hooks_after_create() {
        let app = test_app().await;
        app.clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "hook-one",
                    "event_names": ["issue.created"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "w" },
                        "body": "hi"
                    }
                }),
            ))
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/hooks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["name"], "hook-one");
    }

    #[tokio::test]
    async fn test_get_hook_by_id() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "get-hook",
                    "event_names": ["issue.created"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "w" },
                        "body": "hi"
                    }
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let get_resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/hooks/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let body = response_body_bytes(get_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], id);
        assert_eq!(v["name"], "get-hook");
    }

    #[tokio::test]
    async fn test_get_hook_not_found() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/hooks/xxxx")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_update_hook_name() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "old-name",
                    "event_names": ["issue.created"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "w" },
                        "body": "hi"
                    }
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let patch_resp = app
            .oneshot(hook_req(
                "PATCH",
                &format!("/hooks/{id}"),
                &serde_json::json!({
                    "name": "new-name"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(patch_resp.status(), StatusCode::OK);
        let body = response_body_bytes(patch_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["name"], "new-name");
    }

    #[tokio::test]
    async fn test_update_hook_enable_disable() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "toggle-hook",
                    "event_names": ["issue.created"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "w" },
                        "body": "hi"
                    }
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        // Disable
        let patch_resp = app
            .clone()
            .oneshot(hook_req(
                "PATCH",
                &format!("/hooks/{id}"),
                &serde_json::json!({
                    "enabled": false
                }),
            ))
            .await
            .unwrap();
        assert_eq!(patch_resp.status(), StatusCode::OK);
        let body = response_body_bytes(patch_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enabled"], false);

        // Re-enable
        let patch_resp = app
            .oneshot(hook_req(
                "PATCH",
                &format!("/hooks/{id}"),
                &serde_json::json!({
                    "enabled": true
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(patch_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enabled"], true);
    }

    #[tokio::test]
    async fn test_delete_hook() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "del-hook",
                    "event_names": ["issue.created"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "w" },
                        "body": "hi"
                    }
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        // Delete
        let del_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/hooks/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);

        // Now it should be gone
        let get_resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/hooks/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_list_hooks_enabled_filter() {
        let app = test_app().await;
        // Create two hooks
        app.clone()
            .oneshot(hook_req("POST", "/hooks", &serde_json::json!({
                "name": "enabled-hook",
                "enabled": true,
                "event_names": ["issue.created"],
                "action": { "type": "send_message", "target": { "type": "issue", "content": "w" }, "body": "hi" }
            })))
            .await.unwrap();

        let create_resp2 = app
            .clone()
            .oneshot(hook_req("POST", "/hooks", &serde_json::json!({
                "name": "disabled-hook",
                "enabled": false,
                "event_names": ["issue.created"],
                "action": { "type": "send_message", "target": { "type": "issue", "content": "w" }, "body": "hi" }
            })))
            .await.unwrap();
        // Wait to ensure it was created
        let _ = response_body_bytes(create_resp2).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/hooks?enabled=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "enabled-hook");
    }

    #[tokio::test]
    async fn test_list_hook_executions() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "exec-hook",
                    "event_names": ["issue.created"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "w" },
                        "body": "hi"
                    }
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        // Executions should be empty initially
        let exec_resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/hooks/{id}/executions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(exec_resp.status(), StatusCode::OK);
        let body = response_body_bytes(exec_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_hook_evaluator_fires_send_message_on_status_changed() {
        let (app, state) = test_app_with_state().await;

        // Create a watcher issue
        let watcher_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Watcher", "body": "watch"
                }),
            ))
            .await
            .unwrap();
        let watcher_body = response_body_bytes(watcher_resp).await;
        let watcher: serde_json::Value = serde_json::from_slice(&watcher_body).unwrap();
        let watcher_id = watcher["id"].as_str().unwrap().to_string();

        // Create a hook that sends a message to the watcher on status_changed
        app.clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "notify",
                    "event_names": ["issue.status_changed"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": watcher_id.clone() },
                        "body": "status changed"
                    }
                }),
            ))
            .await
            .unwrap();

        // Create a work issue with an assignee
        let work_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Work", "body": "do work", "assignee": "test-agent"
                }),
            ))
            .await
            .unwrap();
        let work_body = response_body_bytes(work_resp).await;
        let work: serde_json::Value = serde_json::from_slice(&work_body).unwrap();
        let work_id = work["id"].as_str().unwrap().to_string();

        // Start the work issue (emits StatusChanged → Running)
        app.clone()
            .oneshot(issue_req(
                "PATCH",
                &format!("/issues/{work_id}/status"),
                &serde_json::json!({"status": "in_progress"}),
            ))
            .await
            .unwrap();

        // Wait a bit for the hook evaluator to fire
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let watcher_issue = state.db.get_issue(watcher_id.clone()).await.unwrap();
            if !watcher_issue.comments.is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "hook evaluator never fired a comment on watcher within 3s"
            );
        }

        let watcher_issue = state.db.get_issue(watcher_id.clone()).await.unwrap();
        assert!(
            !watcher_issue.comments.is_empty(),
            "watcher should have received a comment"
        );
        let comment = &watcher_issue.comments[0];
        assert_eq!(comment.author, "ns2-hook");
        assert!(comment.body.contains("status changed"));
    }

    #[tokio::test]
    async fn test_create_hook_with_timer_event_name_returns_201() {
        let app = test_app().await;
        let resp = app
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "timer-hook",
                    "event_names": ["timer.heartbeat"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "abc1" },
                        "body": "tick"
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["event_names"][0], "timer.heartbeat");
    }

    #[tokio::test]
    async fn test_create_hook_with_external_event_name_returns_201() {
        let app = test_app().await;
        let resp = app
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "ci-hook",
                    "event_names": ["external.ci-complete"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "abc1" },
                        "body": "CI done"
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["event_names"][0], "external.ci-complete");
    }

    #[tokio::test]
    async fn test_set_in_progress_spawns_harness_and_reaches_waiting() {
        let (app, state) = test_app_with_state().await;

        // Create an issue with an assignee.
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Lifecycle test",
                    "body": "body",
                    "assignee": "test-agent"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        // Set to in_progress — should spawn harness.
        let resp = patch_issue_status(
            app.clone(),
            &issue_id,
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["status"], "in_progress",
            "returned issue must have status=in_progress"
        );
        assert!(
            !v["session_id"].is_null(),
            "session_id must be set after in_progress transition"
        );

        // Eventually should reach waiting (stub client).
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
            if issue.status == IssueStatus::Waiting {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue did not reach waiting within 5s; status={}",
                issue.status
            );
        }
    }

    #[tokio::test]
    async fn test_create_hook_with_multiple_event_names_returns_201() {
        let app = test_app().await;
        let resp = app
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "multi-hook",
                    "event_names": ["issue.status_changed", "issue.comment_added"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "abc1" },
                        "body": "update"
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["event_names"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_set_in_progress_without_assignee_returns_400() {
        let app = test_app().await;

        // Create issue WITHOUT assignee
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "No assignee",
                    "body": "body"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        let resp = patch_issue_status(
            app,
            &issue_id,
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "in_progress without assignee must return 400"
        );
    }

    #[tokio::test]
    async fn test_set_in_progress_on_failed_session_issue_creates_fresh_session() {
        let (app, state) = test_app_with_state().await;

        let old_session_id = Uuid::new_v4();
        let old_session = types::Session {
            id: old_session_id,
            name: "old-session".into(),
            status: types::SessionStatus::Failed,
            agent: Some("test-agent".into()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&old_session).await.unwrap();

        let issue = types::Issue {
            id: "fi01".to_string(),
            title: "Failed issue".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::Failed,
            branch: String::new(),
            assignee: Some("test-agent".to_string()),
            session_id: Some(old_session_id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        let resp = patch_issue_status(
            app.clone(),
            "fi01",
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "in_progress on failed issue must return 200"
        );
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["status"], "in_progress",
            "failed issue with in_progress should become in_progress"
        );
        let new_session_id = v["session_id"].as_str().expect("session_id must be set");
        assert_ne!(
            new_session_id,
            old_session_id.to_string(),
            "a new session must be created, not the old failed one"
        );

        let old_sess = state.db.get_session(old_session_id).await.unwrap();
        assert_eq!(
            old_sess.status,
            types::SessionStatus::Cancelled,
            "old failed session should be marked cancelled"
        );
    }

    #[tokio::test]
    async fn test_set_in_progress_on_waiting_issue_resumes_session() {
        let (app, state) = test_app_with_state().await;

        let waiting_session_id = Uuid::new_v4();
        let waiting_session = types::Session {
            id: waiting_session_id,
            name: "waiting-session".into(),
            status: types::SessionStatus::Waiting,
            agent: Some("test-agent".into()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&waiting_session).await.unwrap();
        state
            .db
            .update_session_status(waiting_session_id, types::SessionStatus::Waiting)
            .await
            .unwrap();

        let issue = types::Issue {
            id: "wi01".to_string(),
            title: "Waiting issue".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::Waiting,
            branch: String::new(),
            assignee: Some("test-agent".to_string()),
            session_id: Some(waiting_session_id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        let resp = patch_issue_status(
            app.clone(),
            "wi01",
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "in_progress on waiting issue must return 200"
        );
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["status"], "in_progress",
            "waiting issue with in_progress should become in_progress"
        );
        let returned_session_id = v["session_id"].as_str().expect("session_id must be set");
        assert_eq!(
            returned_session_id,
            waiting_session_id.to_string(),
            "should reuse the existing waiting session, not create a new one"
        );
    }

    #[tokio::test]
    async fn test_post_issues_id_start_returns_404_route_removed() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Test",
                    "body": "body",
                    "assignee": "test-agent"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{issue_id}/start"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "POST /issues/:id/start must return 404 after route removal"
        );
    }

    #[tokio::test]
    async fn test_set_in_progress_on_cancelled_issue_hints_reopen() {
        let (app, state) = test_app_with_state().await;

        let issue = types::Issue {
            id: "ci01".to_string(),
            title: "Cancelled issue".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::Cancelled,
            branch: String::new(),
            assignee: Some("test-agent".to_string()),
            session_id: None,
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        let resp = patch_issue_status(
            app.clone(),
            "ci01",
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "in_progress on cancelled issue must return 400"
        );
        let body = response_body_bytes(resp).await;
        let err_msg = String::from_utf8_lossy(&body);
        assert!(
            err_msg.contains("reopen"),
            "error message must hint at `reopen`, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_set_in_progress_on_cancelled_issue_with_session_hints_reopen() {
        let (app, state) = test_app_with_state().await;

        let cancelled_session_id = Uuid::new_v4();
        let cancelled_session = types::Session {
            id: cancelled_session_id,
            name: "cancelled-session".into(),
            status: types::SessionStatus::Cancelled,
            agent: Some("test-agent".into()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&cancelled_session).await.unwrap();

        let issue = types::Issue {
            id: "ci02".to_string(),
            title: "Cancelled issue with session".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::Cancelled,
            branch: String::new(),
            assignee: Some("test-agent".to_string()),
            session_id: Some(cancelled_session_id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        let resp = patch_issue_status(
            app.clone(),
            "ci02",
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "in_progress on cancelled issue with session must return 400"
        );
        let body = response_body_bytes(resp).await;
        let err_msg = String::from_utf8_lossy(&body);
        assert!(
            err_msg.contains("reopen"),
            "error message must hint at `reopen`, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_set_in_progress_on_running_issue_with_session_returns_400() {
        let (app, state) = test_app_with_state().await;

        let running_session_id = Uuid::new_v4();
        let running_session = types::Session {
            id: running_session_id,
            name: "running-session".into(),
            status: types::SessionStatus::Running,
            agent: Some("test-agent".into()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&running_session).await.unwrap();

        let issue = types::Issue {
            id: "ri01".to_string(),
            title: "Running issue".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("test-agent".to_string()),
            session_id: Some(running_session_id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        let resp = patch_issue_status(
            app.clone(),
            "ri01",
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "in_progress on running issue must return 400"
        );
        let body = response_body_bytes(resp).await;
        let err_msg = String::from_utf8_lossy(&body);
        assert!(
            err_msg.contains("in_progress"),
            "error message must mention 'in_progress', got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_set_in_progress_on_waiting_issue_without_session_returns_400() {
        let (app, state) = test_app_with_state().await;

        let issue = types::Issue {
            id: "wi02".to_string(),
            title: "Waiting no-session issue".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::Waiting,
            branch: String::new(),
            assignee: Some("test-agent".to_string()),
            session_id: None,
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        let resp = patch_issue_status(
            app.clone(),
            "wi02",
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "in_progress on waiting issue with no session_id must return 400"
        );
    }

    // ─── POST /webhooks/:event_id tests ─────────────────────────────────────

    /// Helper: create a Webhook Event directly in the `event_store` and return its id.
    async fn create_webhook_event(state: &AppState, name: &str, secret: Option<&str>) -> String {
        let event = types::Event {
            id: format!("evt-{}", uuid::Uuid::new_v4().simple()),
            name: name.into(),
            kind: types::EventKind::Webhook {
                secret: secret.map(std::string::ToString::to_string),
            },
            description: None,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let event_id = event.id.clone();
        state.event_store.create_event(&event).await.unwrap();
        event_id
    }

    /// Compute HMAC-SHA256 of `body` using `secret` and return "sha256=<hex>".
    fn compute_hmac_sig(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize().into_bytes();
        format!("sha256={}", hex::encode(result))
    }

    #[tokio::test]
    async fn test_webhook_nonexistent_event_returns_404() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/xxxx")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"event":"test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_webhook_timer_event_returns_404() {
        let (_, state) = test_app_with_state().await;
        // Timer events cannot receive webhooks
        let event = types::Event {
            id: "timer-evt-1".into(),
            name: "heartbeat".into(),
            kind: types::EventKind::Timer { schedule: "* * * * *".into() },
            description: None,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.event_store.create_event(&event).await.unwrap();

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/timer-evt-1")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"event":"test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_webhook_event_no_secret_no_signature_returns_200() {
        let (app, state) = test_app_with_state().await;
        let event_id = create_webhook_event(&state, "ci-complete", None).await;
        let body = r#"{"event":"push","repo":"ns2"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webhooks/{event_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn test_webhook_event_with_secret_correct_signature_returns_200() {
        let (app, state) = test_app_with_state().await;
        let event_id = create_webhook_event(&state, "ci-complete-secret", Some("test-secret")).await;
        let body = r#"{"event":"push"}"#;
        let sig = compute_hmac_sig("test-secret", body.as_bytes());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webhooks/{event_id}"))
                    .header("content-type", "application/json")
                    .header("x-hub-signature-256", sig)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn test_webhook_event_with_secret_missing_signature_returns_401() {
        let (app, state) = test_app_with_state().await;
        let event_id = create_webhook_event(&state, "ci-complete-secret2", Some("test-secret")).await;
        let body = r#"{"event":"push"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webhooks/{event_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_webhook_event_with_secret_wrong_signature_returns_401() {
        let (app, state) = test_app_with_state().await;
        let event_id = create_webhook_event(&state, "ci-complete-secret3", Some("test-secret")).await;
        let body = r#"{"event":"push"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webhooks/{event_id}"))
                    .header("content-type", "application/json")
                    .header("x-hub-signature-256", "sha256=badhash")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_webhook_invalid_json_returns_400() {
        let (app, state) = test_app_with_state().await;
        let event_id = create_webhook_event(&state, "ci-bad-json", None).await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webhooks/{event_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_webhook_disabled_event_returns_404() {
        let (_, state) = test_app_with_state().await;
        let event = types::Event {
            id: "disabled-evt-1".into(),
            name: "disabled-event".into(),
            kind: types::EventKind::Webhook { secret: None },
            description: None,
            enabled: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.event_store.create_event(&event).await.unwrap();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/disabled-evt-1")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"event":"push"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_webhook_emits_system_event_external() {
        let (_, state) = test_app_with_state().await;
        let mut rx = state.event_bus.subscribe();
        let event_id = create_webhook_event(&state, "ci-complete", None).await;
        let app = build_router(state);
        let body = r#"{"event":"push","ref":"main"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webhooks/{event_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Drain events to find SystemEvent::External
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(2);
        loop {
            match rx.try_recv() {
                Ok(events::SystemEvent::External { event_id: eid, event_name: ename, payload }) => {
                    assert_eq!(eid, event_id);
                    assert_eq!(ename, "ci-complete");
                    assert_eq!(payload["event"], "push");
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    assert!(
                        tokio::time::Instant::now() <= deadline,
                        "SystemEvent::External not received within 2s"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
            }
        }
    }

    #[tokio::test]
    async fn test_webhook_evaluator_dispatches_action_for_external_event() {
        let (app, state) = test_app_with_state().await;

        // Create a watcher issue
        let watcher_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({ "title": "Watcher", "body": "watch" }),
            ))
            .await
            .unwrap();
        let watcher_body = response_body_bytes(watcher_resp).await;
        let watcher: serde_json::Value = serde_json::from_slice(&watcher_body).unwrap();
        let watcher_id = watcher["id"].as_str().unwrap().to_string();

        // Create a webhook Event in the event_store
        let event_id = create_webhook_event(&state, "ci-deploy", None).await;

        // Create a hook that listens for "external.ci-deploy" events
        let create_resp = app
            .clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "external-ci-hook",
                    "event_names": ["external.ci-deploy"],
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": watcher_id.clone() },
                        "body": "webhook received"
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);

        // POST a valid webhook to the event
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webhooks/{event_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"event":"push"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Wait for the evaluator to post the comment
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let watcher_issue = state.db.get_issue(watcher_id.clone()).await.unwrap();
            if !watcher_issue.comments.is_empty() {
                let comment = &watcher_issue.comments[0];
                assert_eq!(comment.author, "ns2-hook");
                assert!(comment.body.contains("webhook received"));
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "hook evaluator never dispatched action for external event within 3s"
            );
        }
    }


    #[tokio::test]
    async fn test_patch_issue_status_non_in_progress_updates_field() {
        let (app, state) = test_app_with_state().await;

        // Create an open issue.
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "Status update test",
                    "body": "body"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        // PATCH /issues/{id}/status with {"status": "waiting"}.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/issues/{issue_id}/status"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&serde_json::json!({"status": "waiting"})).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "PATCH with non-in_progress status must return 200"
        );
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["status"], "waiting",
            "returned issue must have the new status"
        );

        // Confirm persistence: fetch from DB directly.
        let persisted = state.db.get_issue(issue_id).await.unwrap();
        assert_eq!(
            persisted.status,
            types::IssueStatus::Waiting,
            "DB must persist the new status"
        );
    }

    // ── lifecycle subscriber Cancelled arm test ───────────────────────────────

    /// When `IssueEvent::StatusChanged { to: Cancelled }` is received, the lifecycle
    /// subscriber must drop the session's `msg_sender` and mark the session Cancelled.
    #[tokio::test]
    async fn test_lifecycle_subscriber_cancel_kills_harness() {
        let state = test_state().await;
        spawn_issue_lifecycle_subscriber(&state);

        // Create a session and a running issue linked to it.
        let session = Session {
            id: Uuid::new_v4(),
            name: "cancel-test".into(),
            status: types::SessionStatus::Running,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();

        let issue = types::Issue {
            id: "lc-c1".to_string(),
            title: "Cancel test".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: Some(session.id),
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        // Manually insert a msg_sender for the session so we can detect removal.
        let (tx, _rx) = tokio::sync::mpsc::channel::<String>(1);
        {
            let mut senders = state.msg_senders.lock().await;
            senders.insert(session.id, tx);
        }

        // Confirm the sender is present.
        {
            let senders = state.msg_senders.lock().await;
            let has_key = senders.contains_key(&session.id);
            drop(senders);
            assert!(has_key, "msg_sender must be present before cancel");
        }

        // Build the Cancelled issue (with session_id) and emit the event.
        let mut cancelled_issue = issue.clone();
        cancelled_issue.status = types::IssueStatus::Cancelled;

        state.event_bus.send(events::SystemEvent::Issue(
            events::IssueEvent::StatusChanged {
                from: types::IssueStatus::InProgress,
                to: types::IssueStatus::Cancelled,
                issue: cancelled_issue,
            },
        ));

        // Wait for the subscriber to process the event.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let senders = state.msg_senders.lock().await;
            let still_present = senders.contains_key(&session.id);
            drop(senders);
            if !still_present {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "msg_sender was not removed within 3s after Cancelled event"
            );
        }

        // Verify the sender was removed.
        let senders = state.msg_senders.lock().await;
        assert!(
            !senders.contains_key(&session.id),
            "msg_sender must be removed after Cancelled event"
        );
        drop(senders);

        // Verify the session is now Cancelled in DB.
        let db_session = state.db.get_session(session.id).await.unwrap();
        assert_eq!(
            db_session.status,
            types::SessionStatus::Cancelled,
            "session must be marked Cancelled in DB"
        );
    }

    // ─── /named-events route integration tests ────────────────────────────────

    fn named_event_req(method: &str, uri: &str, body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_named_event_returns_201() {
        let (app, _state) = test_app_with_state().await;
        let resp = app
            .oneshot(named_event_req(
                "POST",
                "/named-events",
                &serde_json::json!({
                    "name": "ci-complete",
                    "kind": { "type": "webhook" }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["name"], "ci-complete");
        // id should be 4 characters
        let id = v["id"].as_str().unwrap();
        assert_eq!(id.len(), 4, "event id should be 4 chars, got: {id}");
    }

    #[tokio::test]
    async fn test_create_named_event_timer_invalid_schedule_returns_400() {
        let (app, _state) = test_app_with_state().await;
        let resp = app
            .oneshot(named_event_req(
                "POST",
                "/named-events",
                &serde_json::json!({
                    "name": "bad-timer",
                    "kind": { "type": "timer", "schedule": "not-valid" }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "invalid cron schedule should return 400"
        );
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v["error"].as_str().unwrap_or("").contains("invalid cron schedule"),
            "error message should mention invalid cron schedule, got: {v}"
        );
    }

    #[tokio::test]
    async fn test_list_named_events_returns_created_event() {
        let (app, _state) = test_app_with_state().await;

        // Create an event
        app.clone()
            .oneshot(named_event_req(
                "POST",
                "/named-events",
                &serde_json::json!({
                    "name": "deploy-done",
                    "kind": { "type": "webhook" }
                }),
            ))
            .await
            .unwrap();

        // List events
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/named-events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let events: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = events.as_array().unwrap();
        assert_eq!(arr.len(), 1, "should have exactly 1 event");
        assert_eq!(arr[0]["name"], "deploy-done");
    }

    #[tokio::test]
    async fn test_get_named_event_by_id() {
        let (app, _state) = test_app_with_state().await;

        // Create an event
        let create_resp = app
            .clone()
            .oneshot(named_event_req(
                "POST",
                "/named-events",
                &serde_json::json!({
                    "name": "push-event",
                    "kind": { "type": "webhook" }
                }),
            ))
            .await
            .unwrap();
        let create_body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&create_body).unwrap();
        let event_id = created["id"].as_str().unwrap().to_string();

        // GET /named-events/:id
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/named-events/{event_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "GET by id should return 200"
        );
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], event_id.as_str());
        assert_eq!(v["name"], "push-event");
    }

    #[tokio::test]
    async fn test_get_named_event_not_found() {
        let (app, _state) = test_app_with_state().await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/named-events/no-such-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "GET on nonexistent id should return 404"
        );
    }

    #[tokio::test]
    async fn test_delete_named_event() {
        let (app, _state) = test_app_with_state().await;

        // Create an event
        let create_resp = app
            .clone()
            .oneshot(named_event_req(
                "POST",
                "/named-events",
                &serde_json::json!({
                    "name": "to-be-deleted",
                    "kind": { "type": "webhook" }
                }),
            ))
            .await
            .unwrap();
        let create_body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&create_body).unwrap();
        let event_id = created["id"].as_str().unwrap().to_string();

        // DELETE /named-events/:id
        let del_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/named-events/{event_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            del_resp.status(),
            StatusCode::NO_CONTENT,
            "DELETE should return 204"
        );

        // GET /named-events should return empty list
        let list_resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/named-events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let list_body = response_body_bytes(list_resp).await;
        let events: serde_json::Value = serde_json::from_slice(&list_body).unwrap();
        let arr = events.as_array().unwrap();
        assert!(arr.is_empty(), "list should be empty after delete");
    }

    #[tokio::test]
    async fn test_create_named_event_duplicate_name_returns_error() {
        let (app, _state) = test_app_with_state().await;

        let payload = serde_json::json!({
            "name": "duplicate-name",
            "kind": { "type": "webhook" }
        });

        // First creation should succeed
        let first_resp = app
            .clone()
            .oneshot(named_event_req("POST", "/named-events", &payload))
            .await
            .unwrap();
        assert_eq!(first_resp.status(), StatusCode::CREATED);

        // Second creation with the same name should fail
        let second_resp = app
            .oneshot(named_event_req("POST", "/named-events", &payload))
            .await
            .unwrap();
        assert!(
            second_resp.status().is_server_error() || second_resp.status().is_client_error(),
            "duplicate name should return an error status, got: {}",
            second_resp.status()
        );
    }
}

