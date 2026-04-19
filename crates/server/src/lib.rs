use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use db::{ContentBlockDb, SessionDb, SqliteDb, TurnDb};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, convert::Infallible, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tokio_stream::wrappers::BroadcastStream;
use types::{Session, SessionEvent, SessionStatus};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db error: {0}")]
    Db(#[from] db::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match &self {
            Error::NotFound | Error::Db(db::Error::NotFound) => {
                (StatusCode::NOT_FOUND, self.to_string())
            }
            Error::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

pub struct ServerConfig {
    pub port: u16,
    pub data_dir: PathBuf,
    pub pid_file: PathBuf,
    pub api_key: Option<String>,
}

#[derive(Clone)]
struct AppState {
    db: Arc<SqliteDb>,
    sessions: Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::broadcast::Sender<SessionEvent>>>>,
    msg_senders: Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::mpsc::Sender<String>>>>,
    api_key: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub name: Option<String>,
    pub agent: Option<String>,
    pub initial_message: Option<String>,
}

#[derive(Deserialize)]
struct ListSessionsQuery {
    status: Option<String>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Deserialize)]
struct SendMessageRequest {
    message: String,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> std::result::Result<(StatusCode, Json<Session>), Error> {
    let now = Utc::now();
    let has_message = req
        .initial_message
        .as_deref()
        .map(|m| !m.is_empty())
        .unwrap_or(false);

    let initial_message = req.initial_message.unwrap_or_default();

    let session = Session {
        id: Uuid::new_v4(),
        name: req.name.unwrap_or_else(|| "unnamed".to_string()),
        status: if has_message {
            SessionStatus::Running
        } else {
            SessionStatus::Created
        },
        agent: req.agent,
        created_at: now,
        updated_at: now,
    };
    state.db.create_session(&session).await?;

    if has_message {
        let (tx, _rx) = tokio::sync::broadcast::channel::<SessionEvent>(256);
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<String>(16);

        {
            let mut map = state.sessions.lock().await;
            map.insert(session.id, tx.clone());
        }
        {
            let mut map = state.msg_senders.lock().await;
            map.insert(session.id, msg_tx.clone());
        }

        // Queue the initial message before spawning
        msg_tx.send(initial_message).await.ok();

        let db = Arc::clone(&state.db);
        let sessions_map = Arc::clone(&state.sessions);
        let msg_senders_map = Arc::clone(&state.msg_senders);
        let session_clone = session.clone();
        let event_tx = tx.clone();

        let config = harness::HarnessConfig {
            session: session_clone.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
        };
        let client: Arc<dyn harness::AnthropicClient> = if let Some(key) = &state.api_key {
            Arc::new(anthropic::Client::new(key.clone()))
        } else {
            Arc::new(harness::StubClient)
        };

        tokio::spawn(async move {
            let _ = harness::run(config, client, db, event_tx, msg_rx).await;

            let mut map = sessions_map.lock().await;
            map.remove(&session_clone.id);
            drop(map);

            let mut smap = msg_senders_map.lock().await;
            smap.remove(&session_clone.id);
        });
    }

    Ok((StatusCode::CREATED, Json(session)))
}

async fn list_sessions(
    State(state): State<AppState>,
    Query(params): Query<ListSessionsQuery>,
) -> std::result::Result<Json<Vec<Session>>, Error> {
    let status = params
        .status
        .as_deref()
        .map(|s| s.parse::<SessionStatus>().map_err(Error::BadRequest))
        .transpose()?;
    let sessions = state.db.list_sessions(status).await?;
    Ok(Json(sessions))
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Session>, Error> {
    let session = state.db.get_session(id).await?;
    Ok(Json(session))
}

fn event_from(ev: &SessionEvent) -> Event {
    Event::default().data(serde_json::to_string(ev).unwrap_or_default())
}

async fn session_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Sse<impl futures::Stream<Item = std::result::Result<Event, Infallible>>> {
    // Subscribe BEFORE reading history to avoid race
    let live_rx = {
        let map = state.sessions.lock().await;
        map.get(&id).map(|tx| tx.subscribe())
    };

    // Fetch session to check status
    let session = state.db.get_session(id).await;

    // Build historical events
    let mut history: Vec<SessionEvent> = Vec::new();

    if let Ok(ref sess) = session {
        if let Ok(turns) = state.db.list_turns(id).await {
            for turn in &turns {
                history.push(SessionEvent::TurnStarted { turn: turn.clone() });
                if let Ok(blocks) = state.db.list_content_blocks(turn.id).await {
                    for (i, (_role, block)) in blocks.into_iter().enumerate() {
                        let index = i as u32;
                        if let types::ContentBlock::Text { ref text } = block {
                            history.push(SessionEvent::ContentBlockDelta {
                                turn_id: turn.id,
                                index,
                                delta: types::ContentBlockDelta::TextDelta { text: text.clone() },
                            });
                        }
                        history.push(SessionEvent::ContentBlockDone {
                            turn_id: turn.id,
                            index,
                            block,
                        });
                    }
                }
                history.push(SessionEvent::TurnDone { turn_id: turn.id });
            }
        }

        match sess.status {
            SessionStatus::Completed | SessionStatus::Failed | SessionStatus::Cancelled => {
                history.push(SessionEvent::SessionDone { session_id: id });
            }
            _ => {}
        }
    }

    let history_stream = stream::iter(history)
        .map(|event| Ok::<Event, Infallible>(event_from(&event)));

    let live_stream = if let Some(rx) = live_rx {
        futures::future::Either::Left(
            BroadcastStream::new(rx)
                .filter_map(|result| async move {
                    match result {
                        Ok(event) => Some(event),
                        Err(_lagged) => Some(SessionEvent::Error {
                            message: "stream lagged".into(),
                        }),
                    }
                })
                .map(|event| Ok::<Event, Infallible>(event_from(&event))),
        )
    } else {
        futures::future::Either::Right(stream::empty::<std::result::Result<Event, Infallible>>())
    };

    Sse::new(history_stream.chain(live_stream))
}

async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<SendMessageRequest>,
) -> std::result::Result<StatusCode, Error> {
    let senders = state.msg_senders.lock().await;
    if let Some(tx) = senders.get(&id) {
        tx.send(req.message).await.ok();
        Ok(StatusCode::OK)
    } else {
        Err(Error::NotFound)
    }
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sessions", post(create_session))
        .route("/sessions", get(list_sessions))
        .route("/sessions/:id", get(get_session))
        .route("/sessions/:id/events", get(session_events))
        .route("/sessions/:id/messages", post(send_message))
        .with_state(state)
}

pub async fn run(config: ServerConfig) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir)?;

    let pid = std::process::id();
    std::fs::write(&config.pid_file, format!("{pid}\n"))?;

    let db_path = config.data_dir.join("ns2.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let db = SqliteDb::connect(&db_url).await?;
    let state = AppState {
        db: Arc::new(db),
        sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        api_key: config.api_key,
    };

    let app = build_router(state);

    let addr = format!("127.0.0.1:{}", config.port);
    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on {addr}");

    let pid_file = config.pid_file.clone();
    let server = axum::serve(listener, app);

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    )?;

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
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for .oneshot()

    async fn test_app() -> Router {
        let db = SqliteDb::connect("sqlite::memory:").await.unwrap();
        let state = AppState {
            db: Arc::new(db),
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            api_key: None,
        };
        build_router(state)
    }

    async fn response_body_bytes(resp: axum::response::Response) -> bytes::Bytes {
        axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()
    }

    // --- /health ---

    #[tokio::test]
    async fn test_health_returns_200() {
        let app = test_app().await;
        let resp = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_health_body() {
        let app = test_app().await;
        let resp = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
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

    async fn create_session_req(body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/sessions")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_session_returns_201() {
        let app = test_app().await;
        let resp = app
            .oneshot(create_session_req(serde_json::json!({})).await)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_create_session_no_message_status_is_created() {
        let app = test_app().await;
        let resp = app
            .oneshot(create_session_req(serde_json::json!({})).await)
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
            .oneshot(
                create_session_req(serde_json::json!({
                    "name": "my-session"
                }))
                .await,
            )
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
            .oneshot(create_session_req(serde_json::json!({})).await)
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

        // Create a session
        let create_resp = app
            .clone()
            .oneshot(
                create_session_req(serde_json::json!({"name": "sess-1"})).await,
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);

        // List sessions
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

        // Create a session and capture its id
        let create_resp = app
            .clone()
            .oneshot(
                create_session_req(serde_json::json!({"name": "by-id"})).await,
            )
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        // Fetch by id
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

        // Create a session (no initial_message -> Created status)
        app.clone()
            .oneshot(
                create_session_req(serde_json::json!({"name": "created-sess"})).await,
            )
            .await
            .unwrap();

        // Filter by "created" — should find it
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

        // Create a session with Created status
        app.clone()
            .oneshot(create_session_req(serde_json::json!({})).await)
            .await
            .unwrap();

        // Filter by "completed" — should be empty
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
}
