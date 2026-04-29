use std::sync::Arc;
use types::{SessionEvent, SessionStatus};

use crate::state::AppState;

/// Spawn a harness task for the given session and return the mpsc sender that
/// can be used to deliver messages to it.
///
/// This function registers the broadcast sender and mpsc sender in `AppState`
/// once the task starts, and removes them when the task finishes.
pub(crate) fn spawn_harness_sync(
    state: &AppState,
    session: types::Session,
    issue_id: Option<String>,
) -> tokio::sync::mpsc::Sender<String> {
    let (tx, _rx) = tokio::sync::broadcast::channel::<SessionEvent>(256);
    let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<String>(16);

    let sessions_map = Arc::clone(&state.sessions);
    let msg_senders_map = Arc::clone(&state.msg_senders);
    let spawning_set = Arc::clone(&state.spawning);
    let db = Arc::clone(&state.db);
    let issue_service = state.issue_service.clone();
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
        let svc = issue_service;
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
                        let summary = if last_turn_text.is_empty() { None } else { Some(last_turn_text) };
                        let _ = svc.finish_issue(&id, summary).await;
                        break;
                    }
                    SessionEvent::Error { message } => {
                        let _ = svc.fail_issue(&id, message).await;
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
