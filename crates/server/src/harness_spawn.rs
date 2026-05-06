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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::{IssueStatus, Session, SessionStatus};
    use uuid::Uuid;

    /// Create a minimal `Session` and persist it to the DB.
    async fn make_session(state: &crate::state::AppState) -> Session {
        let session = Session {
            id: Uuid::new_v4(),
            name: "test-session".into(),
            status: SessionStatus::Created,
            agent: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        session
    }

    /// Create a minimal issue with `Running` status in the DB.
    async fn make_running_issue(state: &crate::state::AppState, id: &str, title: &str) {
        let issue = types::Issue {
            id: id.to_string(),
            title: title.to_string(),
            body: "body".to_string(),
            status: IssueStatus::Running,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();
    }

    /// A Done event for session2 must NOT finish issue1 (whose watcher is bound
    /// to session1).  Only a Done event for session1 should transition issue1 to
    /// Completed.
    ///
    /// This test calls `spawn_harness_sync` directly so the production
    /// session-id filter guard in `harness_spawn.rs` is exercised — not a
    /// hand-rolled re-implementation.
    #[tokio::test]
    async fn test_harness_spawn_done_only_finishes_own_issue() {
        let state = crate::tests::test_state().await;

        // Create two sessions and two running issues.
        let session1 = make_session(&state).await;
        let session2 = make_session(&state).await;
        make_running_issue(&state, "hs-d-i1", "Issue 1").await;
        make_running_issue(&state, "hs-d-i2", "Issue 2").await;

        // Register watchers via the production spawn_harness_sync.
        // The harness task starts but idles (no message sent to _tx1/_tx2) —
        // only the issue-watcher task matters for this test.
        let _tx1 = spawn_harness_sync(&state, session1.clone(), Some("hs-d-i1".into()));
        let _tx2 = spawn_harness_sync(&state, session2.clone(), Some("hs-d-i2".into()));

        // Give the watcher tasks a moment to subscribe to the event bus.
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Done for session2 — issue1's watcher must ignore it because the
        // production filter guards on `sid == session_id`.
        state.event_bus.send(SystemEvent::Session {
            session_id: session2.id,
            event: SessionEvent::Done,
        });

        // Brief pause to let the event propagate.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // issue1 must still be Running: the session2 Done should have been ignored.
        let issue1 = state.db.get_issue("hs-d-i1".into()).await.unwrap();
        assert_eq!(
            issue1.status,
            IssueStatus::Running,
            "issue1 must remain Running after a Done event for a different session"
        );

        // Emit Done for session1 — now issue1's watcher should respond.
        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Done,
        });

        // Wait for issue1 to become Completed.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("hs-d-i1".into()).await.unwrap();
            if fetched.status == IssueStatus::Completed {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue1 did not become Completed within 3s"
            );
        }

        let issue1_final = state.db.get_issue("hs-d-i1".into()).await.unwrap();
        assert_eq!(
            issue1_final.status,
            IssueStatus::Completed,
            "issue1 must be Completed after its own session's Done event"
        );

        // issue2 should also be Completed (its watcher received session2's Done above).
        let issue2_final = state.db.get_issue("hs-d-i2".into()).await.unwrap();
        assert_eq!(
            issue2_final.status,
            IssueStatus::Completed,
            "issue2 must be Completed after its own session's Done event"
        );
    }

    /// An Error event for a different session must NOT fail issue1.  Only an
    /// Error event for issue1's own session should mark it Failed.
    ///
    /// This test calls `spawn_harness_sync` directly so the production
    /// session-id filter guard in `harness_spawn.rs` is exercised — not a
    /// hand-rolled re-implementation.
    #[tokio::test]
    async fn test_harness_spawn_error_only_fails_own_issue() {
        let state = crate::tests::test_state().await;

        let session1 = make_session(&state).await;
        let session2 = make_session(&state).await;
        make_running_issue(&state, "hs-e-i1", "Error Filter Issue").await;

        // Spawn watcher for issue1 → session1 via the real spawn_harness_sync.
        let _tx1 = spawn_harness_sync(&state, session1.clone(), Some("hs-e-i1".into()));

        // Give the watcher a moment to subscribe.
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Error for the unrelated session2 — watcher must ignore it.
        state.event_bus.send(SystemEvent::Session {
            session_id: session2.id,
            event: SessionEvent::Error {
                message: "error from unrelated session".into(),
            },
        });

        // Brief pause to let the event propagate.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // issue1 must still be Running.
        let issue1 = state.db.get_issue("hs-e-i1".into()).await.unwrap();
        assert_eq!(
            issue1.status,
            IssueStatus::Running,
            "issue1 must remain Running after an Error event for a different session"
        );

        // Emit Error for session1 — watcher should now fail issue1.
        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Error {
                message: "real failure".into(),
            },
        });

        // Wait for issue1 to become Failed.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue("hs-e-i1".into()).await.unwrap();
            if fetched.status == IssueStatus::Failed {
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue1 did not become Failed within 3s"
            );
        }

        let issue1_final = state.db.get_issue("hs-e-i1".into()).await.unwrap();
        assert_eq!(
            issue1_final.status,
            IssueStatus::Failed,
            "issue1 must be Failed after its own session's Error event"
        );
    }
}
