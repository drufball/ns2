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

use routes::{events_route, hook as hook_route, issue, session};
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
                        .list_hooks(Some(true), None)
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
    let (db, hook_store) = db::connect(&db_url).await?;
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
    };

    // Spawn the hook evaluator background task.
    spawn_hook_evaluator(&state);

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

    /// Helper to build an in-memory hook store suitable for tests.
    async fn make_test_hook_store() -> Arc<dyn db::HookStore> {
        let (_db, hook_store) = db::connect("sqlite::memory:").await.unwrap();
        hook_store
    }

    pub async fn test_state() -> AppState {
        let (db, hook_store) = db::connect("sqlite::memory:").await.unwrap();
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
        }
    }

    async fn test_app() -> Router {
        let state = test_state().await;
        build_router(state)
    }

    async fn test_app_with_state() -> (Router, AppState) {
        let state = test_state().await;
        spawn_hook_evaluator(&state);
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
                    .uri("/sessions?status=completed")
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
    async fn test_completed_session_events_includes_session_done() {
        let (app, state) = test_app_with_state().await;

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
            .update_session_status(session.id, SessionStatus::Completed)
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
            "completed session events must include SessionDone"
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
            .update_session_status(session.id, SessionStatus::Completed)
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
            .update_session_status(session.id, SessionStatus::Completed)
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
            .update_session_status(session.id, SessionStatus::Completed)
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
            .update_session_status(session.id, SessionStatus::Completed)
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
    async fn test_completed_session_sender_still_alive() {
        let (app, state) = test_app_with_state().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(&serde_json::json!({
                "name": "completed-test",
                "initial_message": "run and complete"
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
            "completed session must accept follow-up messages and return 200"
        );
    }

    #[tokio::test]
    async fn test_send_message_to_completed_session_returns_200() {
        let (app, state) = test_app_with_state().await;

        let session = Session {
            id: Uuid::new_v4(),
            name: "completed-no-harness".into(),
            status: SessionStatus::Completed,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, SessionStatus::Completed)
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
            "completed session with no active sender must return 200 OK (fresh harness spawned)"
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

    /// Resuming a Waiting session via POST /sessions/:id/messages must spawn
    /// a harness with the linked issue's ID so the issue watcher is active.
    ///
    /// When the harness runs and emits Done (without a Stopped event), the
    /// linked issue should transition from Running → Waiting, proving the
    /// issue watcher was correctly re-spawned.
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
            status: types::IssueStatus::Running,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: Some(session.id),
            parent_id: None,
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

        let resp = patch_session_status(app, &id, serde_json::json!({"status": "completed"})).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], id, "response must contain the session id");
        assert_eq!(
            v["status"], "completed",
            "status must be updated to completed"
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
        for status in ["created", "running", "completed", "failed", "cancelled"] {
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
            status: types::IssueStatus::Running,
            branch: String::new(),
            assignee: None,
            session_id: Some(session.id),
            parent_id: None,
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
            SessionStatus::Completed,
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
        let id = create_issue_with_status(&state, IssueStatus::Running).await;

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
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let client = Arc::new(CapturingClient {
            captured: Arc::clone(&captured),
        }) as Arc<dyn anthropic::AnthropicClient>;
        let issue_service =
            issues::IssueService::with_event_bus(Arc::clone(&db), EventBus::new(1024));
        let event_bus = issue_service.event_bus().clone();
        let hook_store = make_test_hook_store().await;
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
        };
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
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let client = Arc::new(CapturingClient {
            captured: Arc::clone(&captured),
        }) as Arc<dyn anthropic::AnthropicClient>;
        let issue_service =
            issues::IssueService::with_event_bus(Arc::clone(&db), EventBus::new(1024));
        let event_bus = issue_service.event_bus().clone();
        let hook_store = make_test_hook_store().await;
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
        };
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

        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let client = Arc::new(ErrorClient) as Arc<dyn anthropic::AnthropicClient>;
        let issue_service =
            issues::IssueService::with_event_bus(Arc::clone(&db), EventBus::new(1024));
        let event_bus = issue_service.event_bus().clone();
        let hook_store = make_test_hook_store().await;
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
        };
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
            status: IssueStatus::Running,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: None,
            parent_id: None,
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
            status: IssueStatus::Running,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: None,
            parent_id: None,
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
                    "source": { "type": "internal", "event_types": ["issue.status_changed"] },
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
                    "source": { "type": "internal", "event_types": ["issue.created"] },
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
                    "source": { "type": "internal", "event_types": ["issue.created"] },
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
                    "source": { "type": "internal", "event_types": ["issue.created"] },
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
                    "source": { "type": "internal", "event_types": ["issue.created"] },
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
                    "source": { "type": "internal", "event_types": ["issue.created"] },
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
                    "source": { "type": "internal", "event_types": ["issue.created"] },
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
                "source": { "type": "internal", "event_types": ["issue.created"] },
                "action": { "type": "send_message", "target": { "type": "issue", "content": "w" }, "body": "hi" }
            })))
            .await.unwrap();

        let create_resp2 = app
            .clone()
            .oneshot(hook_req("POST", "/hooks", &serde_json::json!({
                "name": "disabled-hook",
                "enabled": false,
                "source": { "type": "internal", "event_types": ["issue.created"] },
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
                    "source": { "type": "internal", "event_types": ["issue.created"] },
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
                    "source": { "type": "internal", "event_types": ["issue.status_changed"] },
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
    async fn test_get_hook_executions_default_limit_is_20() {
        let (app, state) = test_app_with_state().await;

        // Create a hook
        let create_resp = app
            .clone()
            .oneshot(hook_req(
                "POST",
                "/hooks",
                &serde_json::json!({
                    "name": "limit-test-hook",
                    "source": { "type": "internal", "event_types": ["issue.created"] },
                    "action": {
                        "type": "send_message",
                        "target": { "type": "issue", "content": "some-issue-id" },
                        "body": "hi"
                    }
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let hook_id = created["id"].as_str().unwrap().to_string();

        // Insert 25 HookExecution records directly via the hook_store
        for _ in 0..25 {
            let exec = types::HookExecution {
                id: Uuid::new_v4().to_string(),
                hook_id: hook_id.clone(),
                triggered_at: chrono::Utc::now(),
                event_payload: serde_json::json!({}),
                status: types::ExecutionStatus::Completed,
                result: Some("ok".to_string()),
                completed_at: Some(chrono::Utc::now()),
            };
            state.hook_store.create_execution(&exec).await.unwrap();
        }

        // GET /hooks/:id/executions — should return exactly 20 (the default limit)
        let exec_resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/hooks/{hook_id}/executions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(exec_resp.status(), StatusCode::OK);
        let body = response_body_bytes(exec_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().expect("response should be a JSON array");
        assert_eq!(
            arr.len(),
            20,
            "default limit should be 20, got {}",
            arr.len()
        );
    }

    // ─── PATCH /issues/:id/status with in_progress auto-starts ──────────────

    async fn patch_issue_status(
        app: Router,
        id: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        app.oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/issues/{id}/status"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_set_in_progress_on_open_issue_auto_starts() {
        let (app, state) = test_app_with_state().await;

        // Create issue with assignee
        let create_resp = app
            .clone()
            .oneshot(issue_req(
                "POST",
                "/issues",
                &serde_json::json!({
                    "title": "In-progress test",
                    "body": "body",
                    "assignee": "test-agent"
                }),
            ))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        // Set status to in_progress
        let resp = patch_issue_status(
            app.clone(),
            &issue_id,
            serde_json::json!({"status": "in_progress"}),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "in_progress transition should return 200"
        );
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // The returned status should be "running" (harness was spawned)
        assert_eq!(
            v["status"], "running",
            "returned issue must have status=running, not in_progress"
        );
        assert!(
            !v["session_id"].is_null(),
            "session_id must be set after in_progress transition"
        );

        // Eventually should reach waiting (stub client)
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

        // Create an issue and manually put it in failed state with a session
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
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        // Set status to in_progress — should clear old failed session and create new one
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
            v["status"], "running",
            "failed issue with in_progress should become running"
        );
        // The session_id should NOT be the old failed one
        let new_session_id = v["session_id"].as_str().expect("session_id must be set");
        assert_ne!(
            new_session_id,
            old_session_id.to_string(),
            "a new session must be created, not the old failed one"
        );

        // Old session should be cancelled
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

        // Create a waiting session
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

        // Create issue in Waiting state with session
        let issue = types::Issue {
            id: "wi01".to_string(),
            title: "Waiting issue".to_string(),
            body: "body".to_string(),
            status: types::IssueStatus::Waiting,
            branch: String::new(),
            assignee: Some("test-agent".to_string()),
            session_id: Some(waiting_session_id),
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();

        // Set status to in_progress — should resume existing waiting session
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
            v["status"], "running",
            "waiting issue with in_progress should become running"
        );
        // Session id should be the SAME waiting session (reuse)
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

        // Create an issue first
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

        // POST /issues/:id/start should return 404 (route removed)
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
}
