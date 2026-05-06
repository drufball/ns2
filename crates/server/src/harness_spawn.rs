use std::sync::Arc;
use events::{SessionEvent, SystemEvent};
use types::SessionStatus;

use crate::state::AppState;

/// Spawn a harness task for the given session and return the mpsc sender that
/// can be used to deliver messages to it.
///
/// All events are published to the global `EventBus` wrapped in
/// `SystemEvent::Session { session_id, event }`.
#[allow(clippy::too_many_lines)]
pub fn spawn_harness_sync(
    state: &AppState,
    session: types::Session,
    issue_id: Option<String>,
) -> tokio::sync::mpsc::Sender<String> {
    let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<String>(16);

    let msg_senders_map = Arc::clone(&state.msg_senders);
    let spawning_set = Arc::clone(&state.spawning);
    let db = Arc::clone(&state.db);
    let issue_service = state.issue_service.clone();
    let client = Arc::clone(&state.client);
    let session_clone = session;
    let msg_tx_ret = msg_tx.clone();
    let tools = state.tools.clone();
    let model = state.model.clone();
    let event_bus = state.event_bus.clone();

    let config = harness::HarnessConfig {
        session: session_clone.clone(),
        model,
        tools,
        git_root: None,
        cwd: None,
    };

    let session_id = session_clone.id;

    // Issue watcher: subscribe to the global bus and filter by session_id.
    let issue_watcher = issue_id.map(|id| {
        let mut rx = event_bus.subscribe();
        let svc = issue_service;
        tokio::spawn(async move {
            let mut current_turn_text = String::new();
            let mut last_turn_text = String::new();

            while let Ok(event) = rx.recv().await {
                match event {
                    SystemEvent::Session {
                        session_id: sid,
                        event: SessionEvent::ContentBlockDelta {
                            delta: types::ContentBlockDelta::TextDelta { text },
                            ..
                        },
                    } if sid == session_id => {
                        current_turn_text.push_str(&text);
                    }
                    SystemEvent::Session {
                        session_id: sid,
                        event: SessionEvent::TurnDone { .. },
                    } if sid == session_id && !current_turn_text.is_empty() => {
                        last_turn_text = std::mem::take(&mut current_turn_text);
                    }
                    SystemEvent::Session {
                        session_id: sid,
                        event: SessionEvent::Done,
                    } if sid == session_id => {
                        let summary = if last_turn_text.is_empty() {
                            None
                        } else {
                            Some(last_turn_text)
                        };
                        let _ = svc.finish_issue(&id, summary).await;
                        break;
                    }
                    SystemEvent::Session {
                        session_id: sid,
                        event: SessionEvent::Error { message },
                    } if sid == session_id => {
                        let _ = svc.fail_issue(&id, message).await;
                        break;
                    }
                    _ => {}
                }
            }
        })
    });

    // A one-shot broadcast channel (capacity = 1) acts as a signal channel
    // for the harness event bridge.  The harness emits SessionEvents via its
    // legacy broadcast::Sender; we subscribe here and forward them to the
    // global EventBus.
    let (event_tx, _) = tokio::sync::broadcast::channel::<SessionEvent>(256);
    let event_tx_for_harness = event_tx.clone();

    // Forward per-session broadcast → global EventBus.
    let bus_forwarder = {
        let mut rx = event_tx.subscribe();
        let bus = event_bus;
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        bus.send(SystemEvent::Session {
                            session_id,
                            event: ev,
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            "global event bus forwarder lagged by {n} messages for session {session_id}"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        })
    };

    tokio::spawn(async move {
        {
            msg_senders_map.lock().await.insert(session_id, msg_tx);
            let mut spawning = spawning_set.lock().await;
            spawning.remove(&session_id);
        }

        let db_clone = Arc::clone(&db);
        let event_tx_clone = event_tx_for_harness.clone();

        if let Err(e) = harness::run(config, client, db, event_tx_for_harness, msg_rx).await {
            let _ = event_tx_clone.send(SessionEvent::Error {
                message: e.to_string(),
            });
            let _ = db_clone
                .update_session_status(session_id, SessionStatus::Failed)
                .await;
        }

        {
            let mut smap = msg_senders_map.lock().await;
            smap.remove(&session_id);
        }

        drop(issue_watcher);
        drop(bus_forwarder);
    });

    msg_tx_ret
}
