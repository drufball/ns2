use chrono::Utc;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use types::{IssueComment, IssueStatus, Session, SessionEvent, SessionStatus};
use uuid::Uuid;

/// Central application state shared across all request handlers.
///
/// Owns the session registry (broadcast channels for SSE streaming) and the
/// message-sender map (mpsc channels for delivering messages to live harness
/// tasks). This is the single source of truth for both maps; no other module
/// may hold a mutable reference to them.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Arc<dyn db::Db>,
    pub(crate) issue_service: issues::IssueService,
    /// Maps session id → broadcast sender for SSE streaming.
    pub(crate) sessions:
        Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::broadcast::Sender<SessionEvent>>>>,
    /// Maps session id → mpsc sender for delivering messages to the live harness.
    pub(crate) msg_senders:
        Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::mpsc::Sender<String>>>>,
    /// Set of session ids for which a harness spawn is currently in flight.
    pub(crate) spawning: Arc<tokio::sync::Mutex<HashSet<Uuid>>>,
    pub(crate) client: Arc<dyn anthropic::AnthropicClient>,
    pub(crate) tools: Vec<Arc<dyn tools::Tool>>,
    pub(crate) model: String,
}

/// Spawn a harness task for the given session and return the mpsc sender that
/// can be used to deliver messages to it.
///
/// This function registers the broadcast sender and mpsc sender in `AppState`
/// once the task starts, and removes them when the task finishes.
pub(crate) fn spawn_harness_sync(
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

    let issue_watcher = issue_id.map(|id| {
        let mut rx = tx.subscribe();
        let db_watch = Arc::clone(&db);
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
            let _ = event_tx_clone.send(SessionEvent::Error {
                message: e.to_string(),
            });
            let _ = db_clone
                .update_session_status(session_id, SessionStatus::Failed)
                .await;
        }

        {
            let mut map = sessions_map.lock().await;
            map.remove(&session_id);
        }
        {
            let mut smap = msg_senders_map.lock().await;
            smap.remove(&session_id);
        }

        drop(issue_watcher);
    });

    msg_tx_ret
}
