use events::{SessionEvent, SystemEvent};
use std::sync::Arc;
use types::SessionStatus;

use crate::state::AppState;

/// Spawn a harness task for the given session and return the mpsc sender that
/// can be used to deliver messages to it.
///
/// All events are published to the global `EventBus` wrapped in
/// `SystemEvent::Session { session_id, event }`.
///
/// Issue lifecycle transitions (session done/error → issue waiting/failed) are
/// handled by the global issue lifecycle subscriber started in `server/lib.rs`,
/// not here.
#[allow(clippy::too_many_lines)]
pub fn spawn_harness_sync(
    state: &AppState,
    session: types::Session,
) -> tokio::sync::mpsc::Sender<String> {
    let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<String>(16);

    let msg_senders_map = Arc::clone(&state.msg_senders);
    let spawning_set = Arc::clone(&state.spawning);
    let db = Arc::clone(&state.db);
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

    // A broadcast channel for per-session events emitted by the harness.
    // We subscribe here and forward them to the global EventBus.
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

        drop(bus_forwarder);
    });

    msg_tx_ret
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use events::{SessionEvent, StopEventStatus, SystemEvent};
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

    /// Create a minimal issue linked to `session_id` with `InProgress` status.
    async fn make_linked_issue(
        state: &crate::state::AppState,
        id: &str,
        session_id: Uuid,
    ) -> types::Issue {
        let issue = types::Issue {
            id: id.to_string(),
            title: "Test issue".to_string(),
            body: "body".to_string(),
            status: IssueStatus::InProgress,
            branch: String::new(),
            assignee: Some("bot".to_string()),
            session_id: Some(session_id),
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.db.create_issue(&issue).await.unwrap();
        issue
    }

    /// Helper: wait for an issue to reach `target_status` within 3 seconds.
    async fn wait_for_status(
        state: &crate::state::AppState,
        issue_id: &str,
        target_status: IssueStatus,
    ) {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let fetched = state.db.get_issue(issue_id.to_string()).await.unwrap();
            if fetched.status == target_status {
                return;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "issue {issue_id} did not become {target_status} within 3s (current: {})",
                fetched.status
            );
        }
    }

    /// Scenario 7a: Stopped{Complete, comment} then Done → issue Completed with comment.
    /// The global subscriber (`spawn_issue_lifecycle_subscriber`) handles the transition.
    #[tokio::test]
    async fn test_stopped_complete_with_comment_marks_issue_completed() {
        let state = crate::tests::test_state().await;
        // Start the global subscriber
        crate::spawn_issue_lifecycle_subscriber(&state);

        let session1 = make_session(&state).await;
        make_linked_issue(&state, "sw-c1", session1.id).await;

        let _tx = spawn_harness_sync(&state, session1.clone());
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Stopped{Complete, "done"} then Done
        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Stopped {
                status: StopEventStatus::Complete,
                comment: Some("done".into()),
            },
        });
        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Done,
        });

        wait_for_status(&state, "sw-c1", IssueStatus::Completed).await;

        let issue = state.db.get_issue("sw-c1".into()).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Completed);
        assert!(
            issue.comments.iter().any(|c| c.body == "done"),
            "expected comment 'done', got: {:?}",
            issue.comments
        );
    }

    /// Scenario 7b: Stopped{Waiting, None} then Done → issue Waiting, no new comment.
    #[tokio::test]
    async fn test_stopped_waiting_marks_issue_waiting_no_comment() {
        let state = crate::tests::test_state().await;
        crate::spawn_issue_lifecycle_subscriber(&state);

        let session1 = make_session(&state).await;
        make_linked_issue(&state, "sw-w1", session1.id).await;

        let _tx = spawn_harness_sync(&state, session1.clone());
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Stopped {
                status: StopEventStatus::Waiting,
                comment: None,
            },
        });
        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Done,
        });

        wait_for_status(&state, "sw-w1", IssueStatus::Waiting).await;

        let issue = state.db.get_issue("sw-w1".into()).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Waiting);
        assert!(issue.comments.is_empty(), "no comment expected");
    }

    /// Scenario 7c: Done without Stopped → issue Waiting, no new comment.
    #[tokio::test]
    async fn test_done_without_stopped_marks_issue_waiting() {
        let state = crate::tests::test_state().await;
        crate::spawn_issue_lifecycle_subscriber(&state);

        let session1 = make_session(&state).await;
        make_linked_issue(&state, "sw-d1", session1.id).await;

        let _tx = spawn_harness_sync(&state, session1.clone());
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Done,
        });

        wait_for_status(&state, "sw-d1", IssueStatus::Waiting).await;

        let issue = state.db.get_issue("sw-d1".into()).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Waiting);
        assert!(issue.comments.is_empty(), "no comment expected");
    }

    /// Done event for session2 must NOT finish issue1 linked to session1.
    #[tokio::test]
    async fn test_harness_spawn_done_only_finishes_own_issue() {
        let state = crate::tests::test_state().await;
        crate::spawn_issue_lifecycle_subscriber(&state);

        // Create two sessions and two issues linked to each session.
        let session1 = make_session(&state).await;
        let session2 = make_session(&state).await;
        make_linked_issue(&state, "hs-d-i1", session1.id).await;
        make_linked_issue(&state, "hs-d-i2", session2.id).await;

        let _tx1 = spawn_harness_sync(&state, session1.clone());
        let _tx2 = spawn_harness_sync(&state, session2.clone());

        // Give the subscriber a moment.
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Done for session2 — issue1 must NOT be affected.
        state.event_bus.send(SystemEvent::Session {
            session_id: session2.id,
            event: SessionEvent::Done,
        });

        // Brief pause to let the event propagate.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // issue1 must still be InProgress.
        let issue1 = state.db.get_issue("hs-d-i1".into()).await.unwrap();
        assert_eq!(
            issue1.status,
            IssueStatus::InProgress,
            "issue1 must remain InProgress after a Done event for a different session"
        );

        // Emit Done for session1 — now issue1 should transition to Waiting.
        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Done,
        });

        wait_for_status(&state, "hs-d-i1", IssueStatus::Waiting).await;

        let issue1_final = state.db.get_issue("hs-d-i1".into()).await.unwrap();
        assert_eq!(
            issue1_final.status,
            IssueStatus::Waiting,
            "issue1 must be Waiting after its own session's Done event"
        );

        // issue2 should also be Waiting.
        let issue2_final = state.db.get_issue("hs-d-i2".into()).await.unwrap();
        assert_eq!(
            issue2_final.status,
            IssueStatus::Waiting,
            "issue2 must be Waiting after its own session's Done event"
        );
    }

    /// Error event for a different session must NOT fail issue1.
    #[tokio::test]
    async fn test_harness_spawn_error_only_fails_own_issue() {
        let state = crate::tests::test_state().await;
        crate::spawn_issue_lifecycle_subscriber(&state);

        let session1 = make_session(&state).await;
        let session2 = make_session(&state).await;
        make_linked_issue(&state, "hs-e-i1", session1.id).await;

        let _tx1 = spawn_harness_sync(&state, session1.clone());

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Error for the unrelated session2 — issue1 must NOT be affected.
        state.event_bus.send(SystemEvent::Session {
            session_id: session2.id,
            event: SessionEvent::Error {
                message: "error from unrelated session".into(),
            },
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let issue1 = state.db.get_issue("hs-e-i1".into()).await.unwrap();
        assert_eq!(
            issue1.status,
            IssueStatus::InProgress,
            "issue1 must remain InProgress after an Error event for a different session"
        );

        // Emit Error for session1 — issue1 should fail.
        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Error {
                message: "real failure".into(),
            },
        });

        wait_for_status(&state, "hs-e-i1", IssueStatus::Failed).await;

        let issue1_final = state.db.get_issue("hs-e-i1".into()).await.unwrap();
        assert_eq!(
            issue1_final.status,
            IssueStatus::Failed,
            "issue1 must be Failed after its own session's Error event"
        );
    }

    /// Stopped event for a different session must NOT affect issue1.
    #[tokio::test]
    async fn test_stopped_event_only_applies_to_own_session() {
        let state = crate::tests::test_state().await;
        crate::spawn_issue_lifecycle_subscriber(&state);

        let session1 = make_session(&state).await;
        let session2 = make_session(&state).await;
        make_linked_issue(&state, "sw-s1", session1.id).await;

        let _tx1 = spawn_harness_sync(&state, session1.clone());
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Stopped for session2 (should be ignored for issue linked to session1)
        state.event_bus.send(SystemEvent::Session {
            session_id: session2.id,
            event: SessionEvent::Stopped {
                status: StopEventStatus::Complete,
                comment: Some("should be ignored".into()),
            },
        });

        // Now emit Done for session1 (without Stopped → Waiting)
        state.event_bus.send(SystemEvent::Session {
            session_id: session1.id,
            event: SessionEvent::Done,
        });

        wait_for_status(&state, "sw-s1", IssueStatus::Waiting).await;

        let issue = state.db.get_issue("sw-s1".into()).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Waiting);
        assert!(
            issue.comments.is_empty(),
            "no comment expected when Stopped was for a different session"
        );
    }
}
