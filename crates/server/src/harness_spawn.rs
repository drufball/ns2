use events::{IssueEvent, SessionEvent, StopEventStatus, SystemEvent};
use std::sync::Arc;
use types::SessionStatus;

use crate::state::AppState;

/// Spawn the global issue-lifecycle subscriber.
///
/// This task watches for `IssueEvent::StatusChanged { to: Cancelled }` on the
/// event bus.  When such an event is received for an issue that has a linked
/// session, the subscriber:
///
/// 1. Removes the session's `msg_sender` from the senders map (terminating
///    any live harness).
/// 2. Updates the session's status to `Cancelled` in the DB.
///
/// This is called once at server startup, before any connections are accepted.
pub fn spawn_issue_lifecycle_subscriber(state: &AppState) {
    let mut rx = state.event_bus.subscribe();
    let msg_senders = Arc::clone(&state.msg_senders);
    let db = Arc::clone(&state.db);

    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match rx.recv().await {
                Ok(SystemEvent::Issue(IssueEvent::StatusChanged {
                    issue,
                    to: types::IssueStatus::Cancelled,
                    ..
                })) => {
                    if let Some(session_id) = issue.session_id {
                        // Remove the sender so the harness exits cleanly.
                        {
                            let mut senders = msg_senders.lock().await;
                            senders.remove(&session_id);
                        }
                        // Mark the session cancelled in the DB.
                        let _ = db
                            .update_session_status(session_id, SessionStatus::Cancelled)
                            .await;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("Issue lifecycle subscriber lagged {n} messages");
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

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
            // Track the most recent Stopped event so we can use it on Done.
            let mut stopped_status: Option<StopEventStatus> = None;
            let mut stopped_comment: Option<String> = None;

            while let Ok(event) = rx.recv().await {
                match event {
                    SystemEvent::Session {
                        session_id: sid,
                        event: SessionEvent::Stopped { status, comment },
                    } if sid == session_id => {
                        stopped_status = Some(status);
                        stopped_comment = comment;
                    }
                    SystemEvent::Session {
                        session_id: sid,
                        event: SessionEvent::Done,
                    } if sid == session_id => {
                        // Use the Stopped signal if present; otherwise default to Waiting.
                        let park_status = if matches!(stopped_status, Some(StopEventStatus::Complete)) {
                            types::IssueStatus::Completed
                        } else {
                            types::IssueStatus::Waiting
                        };
                        let _ = svc
                            .park_issue(&id, park_status, stopped_comment.take(), None)
                            .await;
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
// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use events::StopEventStatus;
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

    /// Scenario for Cancelled arm: emitting `IssueEvent::StatusChanged { to: Cancelled }`
    /// must remove the session's `msg_sender` and mark the session Cancelled in DB.
    #[tokio::test]
    async fn test_lifecycle_subscriber_cancel_kills_harness() {
        let state = crate::tests::test_state().await;

        // Create a session and an issue linked to it.
        let session = make_session(&state).await;
        make_running_issue(&state, "lc-c1", "Cancel test").await;

        // Spawn the lifecycle subscriber.
        spawn_issue_lifecycle_subscriber(&state);

        // Spawn a harness so there's a msg_sender in the map.
        let _tx = spawn_harness_sync(&state, session.clone(), Some("lc-c1".into()));

        // Give the harness time to register its sender.
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // Confirm the sender is present before cancellation.
        {
            let senders = state.msg_senders.lock().await;
            let has_key = senders.contains_key(&session.id);
            drop(senders);
            assert!(
                has_key,
                "msg_sender must be present before cancel"
            );
        }

        // Build an issue with the session_id so the event carries it.
        let issue = {
            let mut i = state.db.get_issue("lc-c1".to_string()).await.unwrap();
            i.session_id = Some(session.id);
            i.status = types::IssueStatus::Cancelled;
            i
        };

        // Emit StatusChanged { to: Cancelled }.
        state.event_bus.send(SystemEvent::Issue(
            events::IssueEvent::StatusChanged {
                from: types::IssueStatus::Running,
                to: types::IssueStatus::Cancelled,
                issue,
            },
        ));

        // Wait for the lifecycle subscriber to process the event.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            let senders = state.msg_senders.lock().await;
            if !senders.contains_key(&session.id) {
                break;
            }
            drop(senders);
            assert!(
                tokio::time::Instant::now() <= deadline,
                "msg_sender was not removed within 3s after Cancelled event"
            );
        }

        // Verify the session is marked Cancelled in the DB.
        let db_session = state.db.get_session(session.id).await.unwrap();
        assert_eq!(
            db_session.status,
            SessionStatus::Cancelled,
            "session must be marked Cancelled in DB after IssueEvent::StatusChanged{{Cancelled}}"
        );
    }

    /// Scenario 7a: Stopped{Complete, comment} then Done → issue Completed with comment.
    #[tokio::test]
    async fn test_stopped_complete_with_comment_marks_issue_completed() {
        let state = crate::tests::test_state().await;
        let session1 = make_session(&state).await;
        make_running_issue(&state, "sw-c1", "Issue Complete").await;

        let _tx = spawn_harness_sync(&state, session1.clone(), Some("sw-c1".into()));
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
        let session1 = make_session(&state).await;
        make_running_issue(&state, "sw-w1", "Issue Waiting").await;

        let _tx = spawn_harness_sync(&state, session1.clone(), Some("sw-w1".into()));
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
        let session1 = make_session(&state).await;
        make_running_issue(&state, "sw-d1", "Issue No Stop").await;

        let _tx = spawn_harness_sync(&state, session1.clone(), Some("sw-d1".into()));
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

    /// A Done event for session2 must NOT finish issue1 (whose watcher is bound
    /// to session1).  Only a Done event for session1 should transition issue1.
    #[tokio::test]
    async fn test_harness_spawn_done_only_finishes_own_issue() {
        let state = crate::tests::test_state().await;

        // Create two sessions and two running issues.
        let session1 = make_session(&state).await;
        let session2 = make_session(&state).await;
        make_running_issue(&state, "hs-d-i1", "Issue 1").await;
        make_running_issue(&state, "hs-d-i2", "Issue 2").await;

        let _tx1 = spawn_harness_sync(&state, session1.clone(), Some("hs-d-i1".into()));
        let _tx2 = spawn_harness_sync(&state, session2.clone(), Some("hs-d-i2".into()));

        // Give the watcher tasks a moment to subscribe to the event bus.
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Done for session2 — issue1's watcher must ignore it.
        state.event_bus.send(SystemEvent::Session {
            session_id: session2.id,
            event: SessionEvent::Done,
        });

        // Brief pause to let the event propagate.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // issue1 must still be Running.
        let issue1 = state.db.get_issue("hs-d-i1".into()).await.unwrap();
        assert_eq!(
            issue1.status,
            IssueStatus::Running,
            "issue1 must remain Running after a Done event for a different session"
        );

        // Emit Done for session1 — now issue1's watcher should respond (→ Waiting).
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

        // issue2 should also be Waiting (its watcher received session2's Done above).
        let issue2_final = state.db.get_issue("hs-d-i2".into()).await.unwrap();
        assert_eq!(
            issue2_final.status,
            IssueStatus::Waiting,
            "issue2 must be Waiting after its own session's Done event"
        );
    }

    /// An Error event for a different session must NOT fail issue1.  Only an
    /// Error event for issue1's own session should mark it Failed.
    #[tokio::test]
    async fn test_harness_spawn_error_only_fails_own_issue() {
        let state = crate::tests::test_state().await;

        let session1 = make_session(&state).await;
        let session2 = make_session(&state).await;
        make_running_issue(&state, "hs-e-i1", "Error Filter Issue").await;

        let _tx1 = spawn_harness_sync(&state, session1.clone(), Some("hs-e-i1".into()));

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Error for the unrelated session2 — watcher must ignore it.
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

        let session1 = make_session(&state).await;
        let session2 = make_session(&state).await;
        make_running_issue(&state, "sw-s1", "Session filter test").await;

        let _tx1 = spawn_harness_sync(&state, session1.clone(), Some("sw-s1".into()));
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Emit Stopped for session2 (should be ignored by issue1's watcher)
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
