use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    Json,
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use types::{Session, SessionEvent, SessionStatus};
use uuid::Uuid;

use crate::harness_spawn::spawn_harness_sync;
use crate::state::AppState;
use super::Error;

// ─── Request / Response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub name: Option<String>,
    pub agent: Option<String>,
    pub initial_message: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ListSessionsQuery {
    pub(crate) status: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct HealthResponse {
    status: &'static str,
}

#[derive(Deserialize)]
pub(crate) struct SendMessageRequest {
    pub(crate) message: String,
}

#[derive(Deserialize)]
pub(crate) struct UpdateSessionStatusRequest {
    pub(crate) status: String,
}

#[derive(Deserialize)]
pub(crate) struct SessionEventsQuery {
    pub(crate) last_turns: Option<usize>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub(crate) async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> std::result::Result<(StatusCode, Json<Session>), Error> {
    let now = chrono::Utc::now();
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

pub(crate) async fn list_sessions(
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

pub(crate) async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Session>, Error> {
    let session = state.db.get_session(id).await?;
    Ok(Json(session))
}

pub(crate) fn event_from(ev: &SessionEvent) -> Event {
    Event::default().data(serde_json::to_string(ev).unwrap_or_default())
}

pub(crate) async fn session_events(
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
pub(crate) async fn send_message(
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

pub(crate) async fn update_session_status(
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

/// POST /sessions/:id/cancel — cancel a running or created session.
///
/// Drops the mpsc sender so the harness exits cleanly on its next recv() call,
/// marks the session `cancelled` in the DB, and marks any linked issue `failed`
/// (the issue wasn't explicitly cancelled — its session was, so the issue impl failed).
pub(crate) async fn cancel_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Session>, Error> {
    let session = state.db.get_session(id).await.map_err(|e| match e {
        db::Error::NotFound => Error::NotFound,
        other => Error::Db(other),
    })?;

    match session.status {
        SessionStatus::Completed | SessionStatus::Failed | SessionStatus::Cancelled => {
            return Err(Error::BadRequest(format!(
                "session is already in terminal state: {}",
                session.status
            )));
        }
        _ => {}
    }

    // Drop the msg sender — harness exits cleanly on next recv().
    {
        let mut senders = state.msg_senders.lock().await;
        senders.remove(&id);
    }

    state.db.update_session_status(id, SessionStatus::Cancelled).await?;

    // Mark any linked issue as failed (the issue wasn't explicitly cancelled).
    let linked_issues = state.db.list_issues_by_session_id(id).await.unwrap_or_default();
    for mut issue in linked_issues {
        use types::{IssueComment, IssueStatus};
        if matches!(issue.status, IssueStatus::Running | IssueStatus::Open) {
            issue.comments.push(IssueComment {
                author: "system".to_string(),
                created_at: chrono::Utc::now(),
                body: "session cancelled".to_string(),
            });
            issue.status = IssueStatus::Failed;
            issue.updated_at = chrono::Utc::now();
            let _ = state.db.update_issue(&issue).await;
        }
    }

    let updated = state.db.get_session(id).await?;
    Ok(Json(updated))
}

/// GET /sessions/:id/last_text — return the last assistant text content block for a session.
/// Returns JSON: {"text": "<content>"} or {"text": null} if no text content found.
pub(crate) async fn session_last_text(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<serde_json::Value>, Error> {
    // Verify the session exists (returns 404 if not)
    let _ = state.db.get_session(id).await?;

    let text = state.db.get_last_text_for_session(id).await?;
    Ok(Json(serde_json::json!({ "text": text })))
}
