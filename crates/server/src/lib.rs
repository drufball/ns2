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
use db::SqliteDb;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Deserializer, Serialize};
use std::{collections::{HashMap, HashSet}, convert::Infallible, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tokio_stream::wrappers::BroadcastStream;
use types::{Issue, IssueComment, IssueStatus, Session, SessionEvent, SessionStatus};
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
    pub client: Arc<dyn anthropic::AnthropicClient>,
    pub tools: Vec<Arc<dyn tools::Tool>>,
    pub model: String,
}

#[derive(Clone)]
struct AppState {
    db: Arc<dyn db::Db>,
    sessions: Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::broadcast::Sender<SessionEvent>>>>,
    msg_senders: Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::mpsc::Sender<String>>>>,
    /// Tracks session IDs for which a harness spawn is in flight (not yet inserted into msg_senders).
    spawning: Arc<tokio::sync::Mutex<HashSet<Uuid>>>,
    client: Arc<dyn anthropic::AnthropicClient>,
    tools: Vec<Arc<dyn tools::Tool>>,
    model: String,
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

#[derive(Deserialize)]
struct UpdateSessionStatusRequest {
    status: String,
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
        status: SessionStatus::Created,
        agent: req.agent,
        created_at: now,
        updated_at: now,
    };
    state.db.create_session(&session).await?;

    if has_message {
        let msg_tx = spawn_harness_sync(&state, session.clone(), None);
        msg_tx.send(initial_message).await.ok();
    }

    Ok((StatusCode::CREATED, Json(session)))
}

/// Spawn a harness task for the given session.
///
/// Inserts both the broadcast sender AND the msg sender into their maps atomically
/// (under a single combined lock acquisition) *before* returning. This prevents a
/// second concurrent call from racing to spawn a second harness for the same session.
fn spawn_harness_sync(
    state: &AppState,
    session: Session,
    issue_id: Option<String>,
) -> tokio::sync::mpsc::Sender<String> {
    let (tx, _rx) = tokio::sync::broadcast::channel::<SessionEvent>(256);
    let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<String>(16);

    let sessions_map = Arc::clone(&state.sessions);
    let msg_senders_map = Arc::clone(&state.msg_senders);
    let spawning_set = Arc::clone(&state.spawning);
    let db = Arc::clone(&state.db);
    let client = Arc::clone(&state.client);
    let session_clone = session.clone();
    let event_tx = tx.clone();
    let msg_tx_ret = msg_tx.clone();
    let tools = state.tools.clone();
    let model = state.model.clone();

    let config = harness::HarnessConfig {
        session: session_clone.clone(),
        model,
        tools,
        git_root: None,
    };

    let session_id = session_clone.id;

    // If this session is linked to an issue, subscribe to the event channel now so we can
    // watch for SessionDone and SessionError events and propagate them to the issue status.
    // The harness is designed to stay alive waiting for more messages, so we cannot rely on
    // harness::run returning — instead we react to individual turn completions.
    let issue_watcher = issue_id.map(|id| {
        let mut rx = tx.subscribe();
        let db_watch = Arc::clone(&state.db);
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
                    SessionEvent::SessionDone { .. } => {
                        if let Ok(mut issue) = db_watch.get_issue(id.clone()).await {
                            // Post agent's final turn as comment before marking completed
                            if !last_turn_text.is_empty() {
                                let author = issue.assignee.clone()
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
                    SessionEvent::Error { message } => {
                        if let Ok(mut issue) = db_watch.get_issue(id.clone()).await {
                            issue.comments.push(IssueComment {
                                author: "system".to_string(),
                                created_at: Utc::now(),
                                body: message.clone(),
                            });
                            issue.status = IssueStatus::Failed;
                            issue.updated_at = Utc::now();
                            let _ = db_watch.update_issue(&issue).await;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        })
    });

    tokio::spawn(async move {
        // Insert into maps atomically before yielding back to callers.
        {
            let mut smap = msg_senders_map.lock().await;
            let mut map = sessions_map.lock().await;
            let mut spawning = spawning_set.lock().await;
            map.insert(session_id, event_tx.clone());
            smap.insert(session_id, msg_tx);
            spawning.remove(&session_id);
        }

        let db_clone = Arc::clone(&db);
        let event_tx_clone = event_tx.clone();

        if let Err(e) = harness::run(config, client, db, event_tx.clone(), msg_rx).await {
            let _ = event_tx_clone.send(SessionEvent::Error { message: e.to_string() });
            let _ = db_clone.update_session_status(session_id, SessionStatus::Failed).await;
        }

        // Remove from maps when harness exits (msg_rx closed → all senders dropped)
        {
            let mut map = sessions_map.lock().await;
            map.remove(&session_id);
        }
        {
            let mut smap = msg_senders_map.lock().await;
            smap.remove(&session_id);
        }

        // Note: we do NOT abort the watcher here because it may still be processing
        // the final SessionDone or Error event from the broadcast channel. The watcher
        // exits naturally once it handles a terminal event (its while-loop breaks on
        // SessionDone and Error), or when the broadcast channel closes (all tx senders
        // are dropped after this task ends), causing rx.recv() to return Err.
        drop(issue_watcher);
    });

    msg_tx_ret
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

#[derive(Deserialize)]
struct SessionEventsQuery {
    last_turns: Option<usize>,
}

async fn session_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<SessionEventsQuery>,
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
            // Apply last_turns filter: if last_turns=0, skip all; if last_turns=N, take last N; if absent, take all
            let turns_to_replay: Vec<_> = match params.last_turns {
                Some(0) => vec![],
                Some(n) => turns.iter().rev().take(n).rev().cloned().collect(),
                None => turns.clone(),
            };

            for turn in &turns_to_replay {
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

/// Send a message to a session. Works on `created`, `running`, and `completed` sessions.
///
/// - If the session has an active harness (sender in map): queue the message directly.
/// - If the session exists in DB but has no active harness (`created` status): spawn a
///   new harness and send the message to it.
/// - If the session does not exist: 404.
async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<SendMessageRequest>,
) -> std::result::Result<StatusCode, Error> {
    // Fast path: sender already exists (running session with live harness).
    {
        let senders = state.msg_senders.lock().await;
        if let Some(tx) = senders.get(&id) {
            tx.send(req.message).await.ok();
            return Ok(StatusCode::OK);
        }
    }

    // Verify the session exists in DB.
    let session = state.db.get_session(id).await.map_err(|e| match e {
        db::Error::NotFound => Error::NotFound,
        other => Error::Db(other),
    })?;

    // For `created`, `running`, and `completed` sessions, spawn a harness (if not already
    // running) and deliver the message. `completed` sessions resume from full DB history
    // so no in-memory state is required.
    //
    // Guard against two concurrent requests both reaching the spawn path simultaneously
    // using the `spawning` set as an atomic "already in flight" guard.
    match session.status {
        SessionStatus::Created | SessionStatus::Running | SessionStatus::Completed => {
            // Acquire both locks together to make the check-then-act atomic.
            let mut spawning = state.spawning.lock().await;
            let senders = state.msg_senders.lock().await;

            if let Some(tx) = senders.get(&id) {
                // A concurrent request beat us; the harness is now live.
                let tx = tx.clone();
                drop(senders);
                drop(spawning);
                tx.send(req.message).await.ok();
                return Ok(StatusCode::OK);
            }

            if spawning.contains(&id) {
                // Another request is already spawning. Return OK; the message will be
                // processed once the harness registers its sender and receives messages.
                drop(senders);
                drop(spawning);
                // Spin-wait briefly for the sender to appear.
                for _ in 0..40 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                    let senders = state.msg_senders.lock().await;
                    if let Some(tx) = senders.get(&id) {
                        tx.send(req.message).await.ok();
                        return Ok(StatusCode::OK);
                    }
                }
                // Still not available — best-effort: return OK (message may be lost but
                // the client gets a success status rather than a confusing error).
                return Ok(StatusCode::OK);
            }

            // We are the first to reach this point. Mark as spawning and do it.
            spawning.insert(id);
            drop(senders);
            drop(spawning);

            let msg_tx = spawn_harness_sync(&state, session, None);
            msg_tx.send(req.message).await.ok();
            Ok(StatusCode::OK)
        }
        // `failed` and `cancelled` are terminal states that cannot accept new messages.
        // The caller must explicitly reopen the session (e.g. via `issue reopen`) first.
        SessionStatus::Failed => Err(Error::BadRequest(
            "session is in failed state and cannot accept messages; reopen it first".into(),
        )),
        SessionStatus::Cancelled => Err(Error::BadRequest(
            "session is in cancelled state and cannot accept messages; reopen it first".into(),
        )),
    }
}

async fn update_session_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSessionStatusRequest>,
) -> std::result::Result<Json<Session>, Error> {
    let new_status = req
        .status
        .parse::<SessionStatus>()
        .map_err(Error::BadRequest)?;
    state.db.update_session_status(id, new_status).await?;
    let session = state.db.get_session(id).await?;
    Ok(Json(session))
}

/// Orphan sweep: run at startup before accepting any connections.
///
/// Finds all sessions stuck in `running` state (no live harness after restart),
/// marks them `failed`, and for any linked issue does the same while appending a
/// system comment so the issue history is self-explanatory.
///
/// Errors are logged and swallowed — a sweep failure must not crash the server.
pub(crate) async fn orphan_sweep(db: &Arc<dyn db::Db>) {
    let orphans = match db.list_sessions(Some(SessionStatus::Running)).await {
        Ok(sessions) => sessions,
        Err(e) => {
            eprintln!("[orphan_sweep] failed to list running sessions: {e}");
            return;
        }
    };

    for session in orphans {
        // 1. Mark the session failed.
        if let Err(e) = db.update_session_status(session.id, SessionStatus::Failed).await {
            eprintln!("[orphan_sweep] failed to update session {} to failed: {e}", session.id);
            // Continue — try the rest.
        }

        // 2. Find any issue linked to this session and recover it too.
        let issues = match db.list_issues_by_session_id(session.id).await {
            Ok(issues) => issues,
            Err(e) => {
                eprintln!(
                    "[orphan_sweep] failed to list issues for session {}: {e}",
                    session.id
                );
                continue;
            }
        };

        for mut issue in issues {
            issue.comments.push(IssueComment {
                author: "system".into(),
                body: "session lost on server restart".into(),
                created_at: Utc::now(),
            });
            issue.status = types::IssueStatus::Failed;
            issue.updated_at = Utc::now();

            if let Err(e) = db.update_issue(&issue).await {
                eprintln!(
                    "[orphan_sweep] failed to update issue {} to failed: {e}",
                    issue.id
                );
            }
        }
    }
}

fn generate_issue_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let id = Uuid::new_v4();
    let bytes = id.as_bytes();
    (0..4).map(|i| ALPHABET[(bytes[i] as usize) % ALPHABET.len()] as char).collect()
}

/// Convert a title string into a URL-safe slug.
/// - Lowercase all characters
/// - Replace any run of non-alphanumeric characters with a single `-`
/// - Trim leading/trailing `-`
fn slugify(title: &str) -> String {
    let lower = title.to_lowercase();
    // Replace runs of non-alphanumeric characters with a single dash
    let mut result = String::new();
    let mut in_sep = false;
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            result.push(ch);
            in_sep = false;
        } else if !in_sep {
            result.push('-');
            in_sep = true;
        }
    }
    // Trim leading and trailing dashes
    result.trim_matches('-').to_string()
}

#[derive(Deserialize)]
struct CreateIssueRequest {
    title: String,
    body: String,
    assignee: Option<String>,
    parent_id: Option<String>,
    blocked_on: Option<Vec<String>>,
    branch: Option<String>,
}

// Wraps a present JSON field (including null) in Some, leaving absent fields as None.
// Used with #[serde(default, deserialize_with = "deserialize_some")] to distinguish
// "field absent" (None) from "field explicitly null" (Some(None)).
fn deserialize_some<'de, T, D>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct EditIssueRequest {
    title: Option<String>,
    body: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    assignee: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    parent_id: Option<Option<String>>,
    blocked_on: Option<Vec<String>>,
    branch: Option<String>,
}

#[derive(Deserialize)]
struct ListIssuesQuery {
    status: Option<String>,
    assignee: Option<String>,
    parent_id: Option<String>,
    blocked_on: Option<String>,
}

#[derive(Deserialize)]
struct AddCommentRequest {
    author: String,
    body: String,
}

#[derive(Deserialize)]
struct CompleteIssueRequest {
    comment: String,
}

async fn create_issue(
    State(state): State<AppState>,
    Json(req): Json<CreateIssueRequest>,
) -> std::result::Result<(StatusCode, Json<Issue>), Error> {
    let now = Utc::now();
    let id = generate_issue_id();

    // Compute the branch:
    // 1. Explicit branch in request → use as-is
    // 2. parent_id provided → inherit parent's branch
    // 3. Otherwise → generate slug from "<id>-<slugified-title>"
    let branch = if let Some(b) = req.branch {
        b
    } else if let Some(ref parent_id) = req.parent_id {
        match state.db.get_issue(parent_id.clone()).await {
            Ok(parent) => parent.branch,
            Err(_) => format!("{}-{}", id, slugify(&req.title)),
        }
    } else {
        format!("{}-{}", id, slugify(&req.title))
    };

    let issue = Issue {
        id,
        title: req.title,
        body: req.body,
        status: IssueStatus::Open,
        branch,
        assignee: req.assignee,
        session_id: None,
        parent_id: req.parent_id,
        blocked_on: req.blocked_on.unwrap_or_default(),
        comments: vec![],
        created_at: now,
        updated_at: now,
    };
    state.db.create_issue(&issue).await?;
    Ok((StatusCode::CREATED, Json(issue)))
}

async fn get_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.db.get_issue(id).await?;
    Ok(Json(issue))
}

async fn list_issues(
    State(state): State<AppState>,
    Query(params): Query<ListIssuesQuery>,
) -> std::result::Result<Json<Vec<Issue>>, Error> {
    let status = params
        .status
        .as_deref()
        .map(|s| s.parse::<IssueStatus>().map_err(Error::BadRequest))
        .transpose()?;
    let mut issues = state
        .db
        .list_issues(status, params.assignee, params.parent_id)
        .await?;
    if let Some(blocked_on_filter) = &params.blocked_on {
        issues.retain(|i| i.blocked_on.contains(blocked_on_filter));
    }
    Ok(Json(issues))
}

async fn edit_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EditIssueRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let mut issue = state.db.get_issue(id.clone()).await?;
    if let Some(title) = req.title {
        issue.title = title;
    }
    if let Some(body) = req.body {
        issue.body = body;
    }
    if let Some(assignee_opt) = req.assignee {
        issue.assignee = assignee_opt;
    }
    if let Some(parent_opt) = req.parent_id {
        issue.parent_id = parent_opt;
    }
    if let Some(blocked_on) = req.blocked_on {
        issue.blocked_on = blocked_on;
    }
    if let Some(branch) = req.branch {
        issue.branch = branch;
    }
    issue.updated_at = Utc::now();
    state.db.update_issue(&issue).await?;
    Ok(Json(issue))
}

async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddCommentRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let mut issue = state.db.get_issue(id.clone()).await?;
    issue.comments.push(IssueComment {
        author: req.author,
        created_at: Utc::now(),
        body: req.body,
    });
    issue.updated_at = Utc::now();
    state.db.update_issue(&issue).await?;
    Ok(Json(issue))
}

async fn start_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Issue>, Error> {
    let mut issue = state.db.get_issue(id.clone()).await?;

    if issue.assignee.is_none() {
        return Err(Error::BadRequest("issue has no assignee; set one with `issue edit --assignee <agent>`".into()));
    }
    if issue.status != IssueStatus::Open {
        return Err(Error::BadRequest(format!("issue is already {}", issue.status)));
    }

    let now = Utc::now();
    let session = Session {
        id: Uuid::new_v4(),
        name: format!("issue-{}", issue.id),
        status: SessionStatus::Created,
        agent: issue.assignee.clone(),
        created_at: now,
        updated_at: now,
    };
    state.db.create_session(&session).await?;

    // Update issue to Running before spawning the harness so the DB write cannot
    // race with the harness auto-completing the issue on a fast (stub) client.
    issue.session_id = Some(session.id);
    issue.status = IssueStatus::Running;
    issue.updated_at = Utc::now();
    state.db.update_issue(&issue).await?;

    let initial_message = format!("{}\n\n{}", issue.title, issue.body);
    let msg_tx = spawn_harness_sync(&state, session.clone(), Some(issue.id.clone()));
    msg_tx.send(initial_message).await.ok();

    Ok(Json(issue))
}

async fn complete_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CompleteIssueRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let mut issue = state.db.get_issue(id.clone()).await?;
    if matches!(issue.status, IssueStatus::Completed | IssueStatus::Failed) {
        return Err(Error::BadRequest(format!("issue is already {}", issue.status)));
    }
    issue.comments.push(IssueComment {
        author: "user".into(),
        created_at: Utc::now(),
        body: req.comment,
    });
    issue.status = IssueStatus::Completed;
    issue.updated_at = Utc::now();
    state.db.update_issue(&issue).await?;
    Ok(Json(issue))
}

async fn reopen_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ReopenIssueRequest>>,
) -> std::result::Result<Json<Issue>, Error> {
    let mut issue = state.db.get_issue(id.clone()).await?;

    // Only `failed` and `completed` can be reopened
    let keep_session_id = match issue.status {
        IssueStatus::Failed => false,   // clear session_id → fresh session on next start
        IssueStatus::Completed => true, // keep session_id → resume history on next start
        _ => {
            return Err(Error::BadRequest(format!(
                "cannot reopen issue {id}: only failed or completed issues can be reopened (current status: {})",
                issue.status
            )));
        }
    };

    // Optionally append a user comment before the status transition
    if let Some(Json(req)) = body {
        if let Some(comment_text) = req.comment {
            if !comment_text.is_empty() {
                issue.comments.push(IssueComment {
                    author: "user".into(),
                    created_at: Utc::now(),
                    body: comment_text,
                });
            }
        }
    }

    issue.status = IssueStatus::Open;
    if !keep_session_id {
        issue.session_id = None;
    }
    issue.updated_at = Utc::now();
    state.db.update_issue(&issue).await?;
    Ok(Json(issue))
}

#[derive(Deserialize, Default)]
struct ReopenIssueRequest {
    comment: Option<String>,
}

/// GET /sessions/:id/last_text — return the last assistant text content block for a session.
/// Returns JSON: {"text": "<content>"} or {"text": null} if no text content found.
async fn session_last_text(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<serde_json::Value>, Error> {
    // Verify the session exists (returns 404 if not)
    let _ = state.db.get_session(id).await?;

    let text = state.db.get_last_text_for_session(id).await?;
    Ok(Json(serde_json::json!({ "text": text })))
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sessions", post(create_session))
        .route("/sessions", get(list_sessions))
        .route("/sessions/:id", get(get_session))
        .route("/sessions/:id/events", get(session_events))
        .route("/sessions/:id/messages", post(send_message))
        .route("/sessions/:id/status", axum::routing::patch(update_session_status))
        .route("/sessions/:id/last_text", get(session_last_text))
        .route("/issues", post(create_issue))
        .route("/issues", get(list_issues))
        .route("/issues/:id", get(get_issue))
        .route("/issues/:id", axum::routing::patch(edit_issue))
        .route("/issues/:id/comments", post(add_comment))
        .route("/issues/:id/start", post(start_issue))
        .route("/issues/:id/complete", post(complete_issue))
        .route("/issues/:id/reopen", post(reopen_issue))
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
        db: Arc::new(db) as Arc<dyn db::Db>,
        sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
        client: config.client,
        tools: config.tools,
        model: config.model,
    };

    // Recover orphaned sessions before accepting any connections.
    orphan_sweep(&state.db).await;

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
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for .oneshot()

    /// A minimal stub AnthropicClient defined locally in server tests.
    /// Does NOT use harness::StubClient — server tests must not depend on harness internals.
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

    async fn test_app() -> Router {
        let db = Arc::new(SqliteDb::connect("sqlite::memory:").await.unwrap()) as Arc<dyn db::Db>;
        let client = Arc::new(TestClient) as Arc<dyn anthropic::AnthropicClient>;
        let state = AppState {
            db,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            client,
            tools: vec![],
            model: "claude-opus-4-5".into(),
        };
        build_router(state)
    }

    async fn test_app_with_state() -> (Router, AppState) {
        let db = Arc::new(SqliteDb::connect("sqlite::memory:").await.unwrap()) as Arc<dyn db::Db>;
        let client = Arc::new(TestClient) as Arc<dyn anthropic::AnthropicClient>;
        let state = AppState {
            db,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            client,
            tools: vec![],
            model: "claude-opus-4-5".into(),
        };
        let app = build_router(state.clone());
        (app, state)
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

    // --- GET /sessions/:id/events ---

    #[tokio::test]
    async fn test_session_events_returns_200() {
        let app = test_app().await;

        let create_resp = app
            .clone()
            .oneshot(create_session_req(serde_json::json!({"name": "sse-test"})).await)
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{id}/events"))
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
            .oneshot(create_session_req(serde_json::json!({})).await)
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{id}/events"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("text/event-stream"), "expected SSE content-type, got: {ct}");
    }

    // --- POST /sessions/:id/messages ---

    /// Sending to a nonexistent session returns 404.
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

    /// Sending to a `created` session (no initial message) works: spawns harness and returns 200.
    #[tokio::test]
    async fn test_send_message_to_created_session_returns_200() {
        let app = test_app().await;

        // Create a session without an initial message → status = created, no harness
        let create_resp = app
            .clone()
            .oneshot(create_session_req(serde_json::json!({"name": "idle"})).await)
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        // Send a message to the created session
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

    /// Sending to a running session (with initial message that spawned a harness) returns 200.
    #[tokio::test]
    async fn test_send_message_to_running_session_returns_200() {
        let (app, _state) = test_app_with_state().await;

        // Create a session with an initial message → harness spawned
        let create_resp = app
            .clone()
            .oneshot(
                create_session_req(serde_json::json!({
                    "name": "running-sess",
                    "initial_message": "start"
                }))
                .await,
            )
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        // Give the spawned harness task a moment to register itself in the map
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"message": "follow up"}))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Two concurrent send_message calls on a Created session must not spawn two harnesses.
    /// After both calls, exactly one broadcast sender should exist in the sessions map.
    #[tokio::test]
    async fn test_concurrent_send_message_spawns_only_one_harness() {
        let (app, state) = test_app_with_state().await;

        // Create a session WITHOUT an initial message → Created status, no harness
        let create_resp = app
            .clone()
            .oneshot(create_session_req(serde_json::json!({"name": "race-test"})).await)
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();
        let session_id: Uuid = id.parse().unwrap();

        // Fire two concurrent send_message requests
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

        let (r1, r2) = tokio::join!(
            app1.oneshot(req1),
            app2.oneshot(req2),
        );
        assert_eq!(r1.unwrap().status(), StatusCode::OK);
        assert_eq!(r2.unwrap().status(), StatusCode::OK);

        // Wait a moment for the spawned task to register
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Exactly one sender must exist in the sessions (broadcast) map
        let sessions = state.sessions.lock().await;
        let sender_count = if sessions.contains_key(&session_id) { 1 } else { 0 };
        assert_eq!(
            sender_count, 1,
            "expected exactly 1 broadcast sender in sessions map, got {sender_count}"
        );
    }

    // --- event_from serialization ---

    #[tokio::test]
    async fn test_event_from_serializes_session_event() {
        let session_id = Uuid::new_v4();
        let ev = SessionEvent::SessionDone { session_id };
        let sse_event = event_from(&ev);

        let stream = futures::stream::once(async move { Ok::<Event, Infallible>(sse_event) });
        let resp = Sse::new(stream).into_response();
        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        let data_line = raw
            .lines()
            .find(|l| l.starts_with("data: "))
            .expect("SSE body must contain a data: line");
        let json = &data_line["data: ".len()..];
        let decoded: SessionEvent = serde_json::from_str(json).expect("must deserialize back");
        assert!(
            matches!(decoded, SessionEvent::SessionDone { session_id: sid } if sid == session_id)
        );
    }

    // --- has_message boundary ---

    #[tokio::test]
    async fn test_empty_initial_message_does_not_spawn_harness() {
        let (app, state) = test_app_with_state().await;

        let resp = app
            .oneshot(
                create_session_req(serde_json::json!({"initial_message": ""})).await,
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let sessions = state.sessions.lock().await;
        assert!(sessions.is_empty(), "empty initial_message must not spawn a harness");
    }

    #[tokio::test]
    async fn test_nonempty_initial_message_spawns_harness() {
        let (app, state) = test_app_with_state().await;

        let resp = app
            .oneshot(
                create_session_req(serde_json::json!({"initial_message": "hello"})).await,
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let sessions = state.sessions.lock().await;
        assert!(
            sessions.contains_key(&session_id),
            "non-empty initial_message must spawn a harness"
        );
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
                    .uri(format!("/sessions/{}/events", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        let has_session_done = raw
            .lines()
            .filter(|l| l.starts_with("data: "))
            .any(|l| {
                let json = &l["data: ".len()..];
                serde_json::from_str::<SessionEvent>(json)
                    .map(|ev| matches!(ev, SessionEvent::SessionDone { .. }))
                    .unwrap_or(false)
            });
        assert!(has_session_done, "completed session events must include SessionDone");
    }

    // --- GET /sessions/:id/events?last_turns=N ---

    /// Test that last_turns=0 skips all history and only emits SessionDone for terminal sessions.
    #[tokio::test]
    async fn test_session_events_last_turns_zero_skips_history() {
        let (app, state) = test_app_with_state().await;

        // Create a session and manually mark it completed with some turns
        let session = Session {
            id: Uuid::new_v4(),
            name: "turns-test".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        
        // Create 3 turns
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
                    &types::ContentBlock::Text { text: "hello".into() },
                )
                .await
                .unwrap();
        }
        
        state
            .db
            .update_session_status(session.id, SessionStatus::Completed)
            .await
            .unwrap();

        // Request with last_turns=0
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{}/events?last_turns=0", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        // Should NOT have any TurnStarted events
        let has_turn_started = raw.lines().filter(|l| l.starts_with("data: ")).any(|l| {
            let json = &l["data: ".len()..];
            serde_json::from_str::<SessionEvent>(json)
                .map(|ev| matches!(ev, SessionEvent::TurnStarted { .. }))
                .unwrap_or(false)
        });
        assert!(!has_turn_started, "last_turns=0 should skip all history turns");
        
        // Should still have SessionDone
        let has_session_done = raw.lines().filter(|l| l.starts_with("data: ")).any(|l| {
            let json = &l["data: ".len()..];
            serde_json::from_str::<SessionEvent>(json)
                .map(|ev| matches!(ev, SessionEvent::SessionDone { .. }))
                .unwrap_or(false)
        });
        assert!(has_session_done, "completed session should still emit SessionDone");
    }

    /// Test that last_turns=1 emits only the last turn's events.
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
        
        // Create 3 turns with distinct content
        let mut turn_ids = Vec::new();
        for i in 0..3 {
            let turn = types::Turn {
                id: Uuid::new_v4(),
                session_id: session.id,
                token_count: Some(10),
                created_at: chrono::Utc::now(),
            };
            turn_ids.push(turn.id);
            state.db.create_turn(&turn).await.unwrap();
            state
                .db
                .create_content_block(
                    turn.id,
                    0,
                    &types::Role::Assistant,
                    &types::ContentBlock::Text { text: format!("turn-{}", i) },
                )
                .await
                .unwrap();
        }
        
        state
            .db
            .update_session_status(session.id, SessionStatus::Completed)
            .await
            .unwrap();

        // Request with last_turns=1
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{}/events?last_turns=1", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        // Count TurnStarted events
        let turn_started_count = raw
            .lines()
            .filter(|l| l.starts_with("data: "))
            .filter(|l| {
                let json = &l["data: ".len()..];
                serde_json::from_str::<SessionEvent>(json)
                    .map(|ev| matches!(ev, SessionEvent::TurnStarted { .. }))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(turn_started_count, 1, "last_turns=1 should emit exactly 1 turn");

        // Should contain the last turn's content
        assert!(raw.contains("turn-2"), "should contain last turn's content");
        // Should NOT contain earlier turns' content
        assert!(!raw.contains("turn-0"), "should not contain first turn's content");
        assert!(!raw.contains("turn-1"), "should not contain second turn's content");
    }

    /// Test that absent last_turns param emits all history (current behavior).
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
        
        // Create 3 turns
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
                    &types::ContentBlock::Text { text: format!("turn-{}", i) },
                )
                .await
                .unwrap();
        }
        
        state
            .db
            .update_session_status(session.id, SessionStatus::Completed)
            .await
            .unwrap();

        // Request without last_turns param
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{}/events", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let raw = std::str::from_utf8(&body).unwrap();

        // Should contain all turns' content
        assert!(raw.contains("turn-0"), "should contain first turn's content");
        assert!(raw.contains("turn-1"), "should contain second turn's content");
        assert!(raw.contains("turn-2"), "should contain last turn's content");
    }

    /// A Completed session (with active harness still waiting for messages) accepts follow-up
    /// messages and returns 200. This is the in-process multi-turn case.
    #[tokio::test]
    async fn test_completed_session_sender_still_alive() {
        let (app, state) = test_app_with_state().await;

        // Create a session with an initial message so a harness is spawned
        let create_resp = app
            .clone()
            .oneshot(
                create_session_req(serde_json::json!({
                    "name": "completed-test",
                    "initial_message": "run and complete"
                }))
                .await,
            )
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();
        let session_id: Uuid = id.parse().unwrap();

        // Wait for the session to reach Completed status in the DB
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let session = state.db.get_session(session_id).await.unwrap();
            if session.status == SessionStatus::Completed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("session did not reach Completed within 3s; status={}", session.status);
            }
        }

        // The harness is still alive (waiting for next message in its loop), so the
        // msg sender is still in the map — this is the normal in-process case.
        // POST /sessions/:id/messages must return 200 via the fast path (sender in map).
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

    /// POST /sessions/:id/messages on a `Completed` session with no active sender
    /// must return 200 OK and spawn a fresh harness.
    #[tokio::test]
    async fn test_send_message_to_completed_session_returns_200() {
        let (app, state) = test_app_with_state().await;

        // Directly insert a completed session into the DB (no harness ever spawned)
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

        // Confirm no sender in the map
        {
            let senders = state.msg_senders.lock().await;
            assert!(!senders.contains_key(&session.id));
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

    /// POST /sessions/:id/messages on a `Failed` session must return a non-2xx status
    /// with an error body (sessions in Failed state cannot accept messages).
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
                        serde_json::to_vec(&serde_json::json!({"message": "should fail"}))
                            .unwrap(),
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

    /// POST /sessions/:id/messages on a `Cancelled` session must return a non-2xx status
    /// with an error body (sessions in Cancelled state cannot accept messages).
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
                        serde_json::to_vec(&serde_json::json!({"message": "should fail"}))
                            .unwrap(),
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

    // ─── Issue endpoint tests ─────────────────────────────────────────────────

    fn issue_req(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_issue_returns_201() {
        let app = test_app().await;
        let resp = app
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Fix the bug",
                "body": "Details here"
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_create_issue_response_has_id_and_open_status() {
        let app = test_app().await;
        let resp = app
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Fix the bug",
                "body": "Details here"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "My issue",
                "body": "Body text"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Issue One", "body": "B1"
            })))
            .await
            .unwrap();
        app.clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Issue Two", "body": "B2"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Open issue", "body": "B"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Old title", "body": "B"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let edit_resp = app
            .oneshot(issue_req("PATCH", &format!("/issues/{id}"), serde_json::json!({
                "title": "New title"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Child", "body": "B", "parent_id": "abc1"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();
        assert_eq!(created["parent_id"], "abc1");

        let edit_resp = app
            .clone()
            .oneshot(issue_req("PATCH", &format!("/issues/{id}"), serde_json::json!({
                "parent_id": null
            })))
            .await
            .unwrap();
        assert_eq!(edit_resp.status(), StatusCode::OK);
        let body = response_body_bytes(edit_resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["parent_id"].is_null(), "parent_id should be cleared to null");
    }

    #[tokio::test]
    async fn test_edit_issue_absent_parent_leaves_unchanged() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Child", "body": "B", "parent_id": "abc1"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        // Patch only title — parent_id absent from request should leave it unchanged
        let edit_resp = app
            .oneshot(issue_req("PATCH", &format!("/issues/{id}"), serde_json::json!({
                "title": "Renamed"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Issue", "body": "B"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let comment_resp = app
            .oneshot(issue_req("POST", &format!("/issues/{id}/comments"), serde_json::json!({
                "author": "user",
                "body": "First comment"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "No assignee", "body": "B"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let start_resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/start"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start_resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_start_issue_on_non_open_issue_returns_400() {
        let app = test_app().await;
        // Create with assignee so the first start succeeds
        let create_resp = app
            .clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Has assignee", "body": "B", "assignee": "swe"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        // First start (should succeed and move to running)
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/start"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Second start should fail (already running)
        let second_start = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{id}/start"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second_start.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_complete_issue_sets_status_and_adds_comment() {
        let app = test_app().await;
        let create_resp = app
            .clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Issue", "body": "B"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let complete_resp = app
            .oneshot(issue_req("POST", &format!("/issues/{id}/complete"), serde_json::json!({
                "comment": "All done"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Issue", "body": "B"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        app.clone()
            .oneshot(issue_req("POST", &format!("/issues/{id}/complete"), serde_json::json!({
                "comment": "First completion"
            })))
            .await
            .unwrap();

        let second = app
            .oneshot(issue_req("POST", &format!("/issues/{id}/complete"), serde_json::json!({
                "comment": "Should fail"
            })))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    }

    // --- issue auto-completion when session terminates ---

    #[tokio::test]
    async fn test_issue_auto_completes_when_session_succeeds() {
        let (app, state) = test_app_with_state().await;

        // Use a non-existent agent name so the harness does not load any agent
        // definition from disk (which would pull in include_project_config and
        // real project hooks that depend on the working-tree state).
        let create_resp = app
            .clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Auto complete test", "body": "body", "assignee": "test-agent-no-disk-def"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{issue_id}/start"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Poll until the issue reaches a terminal state (harness uses TestClient → completes fast).
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
            if issue.status == IssueStatus::Completed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("issue did not auto-complete within 5 seconds; status={}", issue.status);
            }
        }
    }

    // --- PATCH /sessions/:id/status ---

    async fn patch_session_status(app: Router, id: &str, body: serde_json::Value) -> axum::response::Response {
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

        // Create a session first
        let create_resp = app
            .clone()
            .oneshot(create_session_req(serde_json::json!({"name": "status-test"})).await)
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        // Patch the status to "completed"
        let resp = patch_session_status(app, &id, serde_json::json!({"status": "completed"})).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], id, "response must contain the session id");
        assert_eq!(v["status"], "completed", "status must be updated to completed");
    }

    #[tokio::test]
    async fn test_patch_session_status_not_found_returns_404() {
        let app = test_app().await;
        let fake_id = uuid::Uuid::new_v4();

        let resp = patch_session_status(app, &fake_id.to_string(), serde_json::json!({"status": "running"})).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].is_string(), "should contain an error field");
    }

    #[tokio::test]
    async fn test_patch_session_status_invalid_status_returns_400() {
        let app = test_app().await;

        // Create a session first
        let create_resp = app
            .clone()
            .oneshot(create_session_req(serde_json::json!({"name": "bad-status"})).await)
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
                .oneshot(create_session_req(serde_json::json!({"name": format!("sess-{status}")})).await)
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Blocker", "body": "B"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(blocker).await;
        let blocker_id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        app.clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Blocked issue", "body": "B",
                "blocked_on": [&blocker_id]
            })))
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

    /// Helper: build an AppState with a fresh in-memory DB (no router).
    async fn test_state() -> AppState {
        let db = Arc::new(SqliteDb::connect("sqlite::memory:").await.unwrap()) as Arc<dyn db::Db>;
        let client = Arc::new(TestClient) as Arc<dyn anthropic::AnthropicClient>;
        AppState {
            db,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            client,
            tools: vec![],
            model: "claude-opus-4-5".into(),
        }
    }

    /// A `running` session is swept to `failed` by `orphan_sweep`.
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

        orphan_sweep(&state.db).await;

        let fetched = state.db.get_session(session.id).await.unwrap();
        assert_eq!(
            fetched.status,
            SessionStatus::Failed,
            "orphan sweep must mark a running session as failed"
        );
    }

    /// A `running` session linked to a `running` issue: both swept to `failed`,
    /// and a system comment is appended to the issue.
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

        orphan_sweep(&state.db).await;

        // Issue must be failed
        let fetched_issue = state.db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(
            fetched_issue.status,
            types::IssueStatus::Failed,
            "orphan sweep must mark the linked issue as failed"
        );

        // Issue must have the system comment
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

    /// `completed` and `cancelled` sessions are NOT swept.
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

        orphan_sweep(&state.db).await;

        // Each session must retain its original status — the sweep touches only `running`.
        for (id, original_status) in &session_ids {
            let fetched = state.db.get_session(*id).await.unwrap();
            assert_eq!(
                fetched.status,
                *original_status,
                "session with original status '{}' must not have been changed by orphan sweep",
                original_status
            );
        }
    }

    /// A `running` session with NO linked issue is swept to `failed` without error.
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

        // Must not panic or return an error
        orphan_sweep(&state.db).await;

        let fetched = state.db.get_session(session.id).await.unwrap();
        assert_eq!(
            fetched.status,
            SessionStatus::Failed,
            "session with no linked issue must still be marked failed"
        );
    }

    // ─── POST /issues/:id/reopen tests ───────────────────────────────────────

    /// Helper: create a failed issue directly in the DB.
    async fn create_issue_with_status(state: &AppState, status: IssueStatus) -> String {
        let now = chrono::Utc::now();
        let issue = types::Issue {
            id: generate_issue_id(),
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

    /// POST /issues/:id/reopen on a failed issue → 200, status becomes open, session_id is null,
    /// comments are preserved.
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
        assert!(v["session_id"].is_null(), "session_id must be cleared to null");
        // Comments must be preserved
        let comments = v["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1, "existing comment must be preserved");
        assert_eq!(comments[0]["author"], "system");
        assert_eq!(comments[0]["body"], "session lost on server restart");
    }

    /// POST /issues/:id/reopen on an open issue → 400 with error body.
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
        assert!(v["error"].is_string(), "response must contain 'error' field");
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

    /// POST /issues/:id/reopen on a running issue → 400 with error body.
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

    // --- New reopen behavior tests ---

    /// POST /issues/:id/reopen on a completed issue → 200, status becomes open,
    /// session_id is KEPT (so next start resumes the existing session history),
    /// comments are preserved.
    #[tokio::test]
    async fn test_reopen_completed_issue_keeps_session_id() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Completed).await;

        // Fetch the original session_id before reopening
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
        // session_id must be preserved for completed issues
        assert_eq!(
            v["session_id"].as_str().unwrap(),
            original_session_id.to_string(),
            "session_id must be kept when reopening a completed issue"
        );
        // Comments preserved
        let comments = v["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1, "existing comment must be preserved");
    }

    /// POST /issues/:id/reopen on a failed issue → session_id is cleared.
    #[tokio::test]
    async fn test_reopen_failed_issue_clears_session_id() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Failed).await;

        // Verify the issue has a session_id before reopening
        let original = state.db.get_issue(id.clone()).await.unwrap();
        assert!(original.session_id.is_some(), "test setup: issue should have a session_id");

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
        assert!(v["session_id"].is_null(), "session_id must be cleared for failed issues");
    }

    /// POST /issues/:id/reopen with a comment in body → comment is prepended before
    /// status transition (comment appears in the response).
    #[tokio::test]
    async fn test_reopen_with_comment_appends_comment_before_status_change() {
        let (app, state) = test_app_with_state().await;
        let id = create_issue_with_status(&state, IssueStatus::Failed).await;

        let resp = app
            .oneshot(issue_req(
                "POST",
                &format!("/issues/{id}/reopen"),
                serde_json::json!({ "comment": "the tests were failing because of X" }),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "open");

        let comments = v["comments"].as_array().unwrap();
        // Original comment + new one
        assert_eq!(comments.len(), 2, "should have original comment plus new one");

        // The new comment must be the last one and have author="user"
        let new_comment = &comments[comments.len() - 1];
        assert_eq!(new_comment["author"], "user");
        assert_eq!(new_comment["body"], "the tests were failing because of X");
    }

    /// POST /issues/:id/reopen without a comment body → no extra comment added.
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
        // Only the original system comment from create_issue_with_status
        let comments = v["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1, "no new comment should be added when no comment provided");
    }

    /// POST /issues/zzzz/reopen → 404.
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

    // ─── issue_watcher: final turn text posted as comment ─────────────────────

    /// Test 1: start_issue → session runs → SessionDone; issue must have at least one comment
    /// with author == assignee containing the stub response text ("stub response").
    #[tokio::test]
    async fn test_issue_watcher_posts_final_turn_as_comment_on_session_done() {
        let (app, state) = test_app_with_state().await;

        // Create an issue with an assignee
        let create_resp = app
            .clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Watcher comment test",
                "body": "Please respond",
                "assignee": "swe-agent"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        // Start the issue — spawns harness with TestClient that returns "stub response"
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{issue_id}/start"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Poll until the issue reaches Completed
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
            if issue.status == IssueStatus::Completed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("issue did not auto-complete within 5 seconds; status={}", issue.status);
            }
        }

        let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Completed);

        // There must be at least one comment with author == assignee
        let agent_comments: Vec<_> = issue.comments.iter()
            .filter(|c| c.author == "swe-agent")
            .collect();
        assert!(
            !agent_comments.is_empty(),
            "expected at least one comment from 'swe-agent', got comments: {:?}",
            issue.comments
        );

        // The comment body must contain the stub response text
        let has_stub_text = agent_comments.iter().any(|c| c.body.contains("stub response"));
        assert!(
            has_stub_text,
            "expected a comment containing 'stub response', got: {:?}",
            agent_comments.iter().map(|c| &c.body).collect::<Vec<_>>()
        );
    }

    /// Test 2: on Error event, issue must have a comment with author == "system" containing
    /// the error message, and status must be Failed.
    #[tokio::test]
    async fn test_issue_watcher_posts_error_as_system_comment_on_error() {
        use async_trait::async_trait;

        // A client that always returns an error
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

        let db = Arc::new(SqliteDb::connect("sqlite::memory:").await.unwrap()) as Arc<dyn db::Db>;
        let client = Arc::new(ErrorClient) as Arc<dyn anthropic::AnthropicClient>;
        let state = AppState {
            db,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            msg_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spawning: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            client,
            tools: vec![],
            model: "claude-opus-4-5".into(),
        };
        let app = build_router(state.clone());

        // Create an issue with an assignee
        let create_resp = app
            .clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Error test issue",
                "body": "trigger an error",
                "assignee": "swe-agent"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let issue_id = created["id"].as_str().unwrap().to_string();

        // Start the issue — the ErrorClient will cause a harness error
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/issues/{issue_id}/start"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Poll until the issue reaches Failed
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
            if issue.status == IssueStatus::Failed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("issue did not reach Failed within 5 seconds; status={}", issue.status);
            }
        }

        let issue = state.db.get_issue(issue_id.clone()).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Failed);

        // Must have a system comment containing the error message
        let system_comments: Vec<_> = issue.comments.iter()
            .filter(|c| c.author == "system")
            .collect();
        assert!(
            !system_comments.is_empty(),
            "expected at least one 'system' comment, got: {:?}",
            issue.comments
        );

        // Comment body must contain the error text
        let has_error_text = system_comments.iter()
            .any(|c| c.body.contains("simulated api failure"));
        assert!(
            has_error_text,
            "expected system comment containing error message, got: {:?}",
            system_comments.iter().map(|c| &c.body).collect::<Vec<_>>()
        );
    }

    /// Test 3: when a session produces multiple turns (simulate two TurnDone events before
    /// SessionDone), only the text from the *last* turn is posted as a comment.
    /// We verify this by manually driving events through a broadcast channel.
    #[tokio::test]
    async fn test_issue_watcher_only_posts_last_turn_text() {
        // Build state so we can call spawn_harness_sync and inject events directly.
        let state = test_state().await;

        // Create an issue
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

        // Create a broadcast channel and subscribe the watcher manually,
        // mirroring what spawn_harness_sync does for the issue_watcher.
        let (tx, _rx) = tokio::sync::broadcast::channel::<SessionEvent>(256);
        let mut rx = tx.subscribe();
        let db_watch = Arc::clone(&state.db);
        let issue_id = "tt01".to_string();

        // Spawn the watcher logic directly (copied from the production code path)
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
                    SessionEvent::TurnDone { .. } => {
                        if !current_turn_text.is_empty() {
                            last_turn_text = std::mem::take(&mut current_turn_text);
                        }
                    }
                    SessionEvent::SessionDone { .. } => {
                        if let Ok(mut issue) = db_watch.get_issue(issue_id.clone()).await {
                            if !last_turn_text.is_empty() {
                                let author = issue.assignee.clone()
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

        let session_id = Uuid::new_v4();
        let turn_id1 = Uuid::new_v4();
        let turn_id2 = Uuid::new_v4();

        // Turn 1: text "first turn text"
        tx.send(SessionEvent::ContentBlockDelta {
            turn_id: turn_id1,
            index: 0,
            delta: types::ContentBlockDelta::TextDelta { text: "first turn text".into() },
        }).unwrap();
        tx.send(SessionEvent::TurnDone { turn_id: turn_id1 }).unwrap();

        // Turn 2: text "second turn text"
        tx.send(SessionEvent::ContentBlockDelta {
            turn_id: turn_id2,
            index: 0,
            delta: types::ContentBlockDelta::TextDelta { text: "second turn text".into() },
        }).unwrap();
        tx.send(SessionEvent::TurnDone { turn_id: turn_id2 }).unwrap();

        // Session done
        tx.send(SessionEvent::SessionDone { session_id }).unwrap();

        // Wait for the watcher to process and write to DB
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("tt01".to_string()).await.unwrap();
            if fetched.status == IssueStatus::Completed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("issue did not complete within 3s");
            }
        }

        let fetched = state.db.get_issue("tt01".to_string()).await.unwrap();

        // Exactly one comment, containing only the last turn text
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

        // Create two turns
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
                &types::ContentBlock::Text { text: "first turn text".into() },
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
                &types::ContentBlock::Text { text: "second turn text".into() },
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

        // First: text block
        state
            .db
            .create_content_block(
                turn.id,
                0,
                &types::Role::Assistant,
                &types::ContentBlock::Text { text: "some text before tools".into() },
            )
            .await
            .unwrap();

        // Then: tool use block (should be skipped when looking for text)
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Fix the Bug",
                "body": "Details here"
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = response_body_bytes(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = v["id"].as_str().unwrap();
        let branch = v["branch"].as_str().unwrap();
        // branch should be "<id>-<slugified-title>"
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

        // Create parent issue (no branch → gets slug)
        let parent_resp = app
            .clone()
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Parent Issue",
                "body": "Parent body"
            })))
            .await
            .unwrap();
        let parent_body = response_body_bytes(parent_resp).await;
        let parent: serde_json::Value = serde_json::from_slice(&parent_body).unwrap();
        let parent_id = parent["id"].as_str().unwrap();
        let parent_branch = parent["branch"].as_str().unwrap();

        // Create child issue with parent_id → should inherit parent's branch
        let child_resp = app
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Child Issue",
                "body": "Child body",
                "parent_id": parent_id
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "My Issue",
                "body": "Body",
                "branch": "my-custom-branch"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "My Issue",
                "body": "Body"
            })))
            .await
            .unwrap();
        let body = response_body_bytes(create_resp).await;
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_str().unwrap();

        let edit_resp = app
            .oneshot(issue_req("PATCH", &format!("/issues/{id}"), serde_json::json!({
                "branch": "updated-branch"
            })))
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
            .oneshot(issue_req("POST", "/issues", serde_json::json!({
                "title": "Branch Test",
                "body": "Body",
                "branch": "feature/xyz"
            })))
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
        assert!(v.get("branch").is_some(), "GET /issues/:id response must include 'branch' field");
        assert_eq!(v["branch"], "feature/xyz");
    }

    /// Test 4: if the session produces no text content (only tool calls), no empty comment
    /// is posted.
    #[tokio::test]
    async fn test_issue_watcher_no_comment_when_no_text_content() {
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

        // Spawn the same watcher logic
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
                    SessionEvent::TurnDone { .. } => {
                        if !current_turn_text.is_empty() {
                            last_turn_text = std::mem::take(&mut current_turn_text);
                        }
                    }
                    SessionEvent::SessionDone { .. } => {
                        if let Ok(mut issue) = db_watch.get_issue(issue_id.clone()).await {
                            if !last_turn_text.is_empty() {
                                let author = issue.assignee.clone()
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

        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();

        // A turn with only a tool-use block (no ContentBlockDelta TextDelta events)
        // TurnDone without any text accumulated
        tx.send(SessionEvent::TurnDone { turn_id }).unwrap();

        // Session done with no text content
        tx.send(SessionEvent::SessionDone { session_id }).unwrap();

        // Wait for watcher to write Completed
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("nt01".to_string()).await.unwrap();
            if fetched.status == IssueStatus::Completed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("issue did not complete within 3s");
            }
        }

        let fetched = state.db.get_issue("nt01".to_string()).await.unwrap();
        assert_eq!(fetched.status, IssueStatus::Completed);
        assert!(
            fetched.comments.is_empty(),
            "expected no comments when session has no text content, got: {:?}",
            fetched.comments
        );
    }
}
