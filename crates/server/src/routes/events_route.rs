use axum::{
    extract::{Query, State},
    response::sse::{Event, Sse},
};
use events::{IssueEvent, SessionEvent, SystemEvent};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

use crate::state::AppState;

// ─── Query params ──────────────────────────────────────────────────────────────

/// Query parameters for `GET /events`.
///
/// - `session_id` — filter to `SystemEvent::Session` events for that session only.
///   When set, historical turns are replayed from the DB before streaming live events.
/// - `issue_id` — filter to `SystemEvent::Issue` events where the issue id matches.
/// - `types` — comma-separated list of event type names (`session`, `issue`,
///   `external`, `timer`).  If absent, all event types are emitted.
/// - `last_turns` — when `session_id` is set, limit the historical replay to the
///   last N turns.  `0` skips all history.  Absent → replay all.
#[derive(Debug, Deserialize, Clone)]
pub struct EventsQuery {
    pub(crate) session_id: Option<Uuid>,
    pub(crate) issue_id: Option<String>,
    pub(crate) types: Option<String>,
    pub(crate) last_turns: Option<usize>,
}

impl EventsQuery {
    /// Returns `true` when the event passes all active filters.
    pub(crate) fn matches(&self, ev: &SystemEvent) -> bool {
        // type filter
        if let Some(ref types_str) = self.types {
            let type_name = match ev {
                SystemEvent::Session { .. } => "session",
                SystemEvent::Issue(_) => "issue",
                SystemEvent::External { .. } => "external",
                SystemEvent::TimerFired { .. } => "timer",
            };
            if !types_str.split(',').map(str::trim).any(|x| x == type_name) {
                return false;
            }
        }

        // session_id filter
        if let Some(sid) = self.session_id {
            match ev {
                SystemEvent::Session { session_id, .. } => {
                    if *session_id != sid {
                        return false;
                    }
                }
                _ => return false,
            }
        }

        // issue_id filter
        if let Some(ref iid) = self.issue_id {
            match ev {
                SystemEvent::Issue(ie) => {
                    let issue_id = match ie {
                        IssueEvent::Created(i) => &i.id,
                        IssueEvent::StatusChanged { issue, .. }
                        | IssueEvent::CommentAdded { issue, .. } => &issue.id,
                    };
                    if issue_id != iid {
                        return false;
                    }
                }
                _ => return false,
            }
        }

        true
    }
}

fn ev_to_sse(ev: &SystemEvent) -> Event {
    Event::default().data(serde_json::to_string(ev).unwrap_or_default())
}

// ─── Handler ──────────────────────────────────────────────────────────────────

pub async fn events(
    State(state): State<AppState>,
    Query(params): Query<EventsQuery>,
) -> Sse<impl futures::Stream<Item = std::result::Result<Event, Infallible>>> {
    // Subscribe BEFORE reading history to avoid race.
    let rx = state.event_bus.subscribe();

    // Build historical events for session_id replay.
    // Also track whether the session is already in a terminal state so we know
    // whether to attach a live stream at all.
    let mut history: Vec<SystemEvent> = Vec::new();
    let mut session_already_done = false;

    if let Some(session_id) = params.session_id {
        if let Ok(sess) = state.db.get_session(session_id).await {
            if let Ok(turns) = state.db.list_turns(session_id).await {
                let turns_to_replay: Vec<_> = match params.last_turns {
                    Some(0) => vec![],
                    Some(n) => turns.iter().rev().take(n).rev().cloned().collect(),
                    None => turns.clone(),
                };

                for turn in &turns_to_replay {
                    history.push(SystemEvent::Session {
                        session_id,
                        event: SessionEvent::TurnStarted { turn: turn.clone() },
                    });
                    if let Ok(blocks) = state.db.list_content_blocks(turn.id).await {
                        for (i, (_role, block)) in blocks.into_iter().enumerate() {
                            let index = u32::try_from(i).unwrap_or(u32::MAX);
                            if let types::ContentBlock::Text { ref text } = block {
                                history.push(SystemEvent::Session {
                                    session_id,
                                    event: SessionEvent::ContentBlockDelta {
                                        turn_id: turn.id,
                                        index,
                                        delta: types::ContentBlockDelta::TextDelta {
                                            text: text.clone(),
                                        },
                                    },
                                });
                            }
                            history.push(SystemEvent::Session {
                                session_id,
                                event: SessionEvent::ContentBlockDone {
                                    turn_id: turn.id,
                                    index,
                                    block,
                                },
                            });
                        }
                    }
                    history.push(SystemEvent::Session {
                        session_id,
                        event: SessionEvent::TurnDone { turn_id: turn.id },
                    });
                }
            }

            match sess.status {
                types::SessionStatus::Failed
                | types::SessionStatus::Cancelled
                | types::SessionStatus::Waiting => {
                    history.push(SystemEvent::Session {
                        session_id,
                        event: SessionEvent::Done,
                    });
                    session_already_done = true;
                }
                _ => {}
            }
        }
    }

    let params_for_live = params.clone();
    let history_stream = stream::iter(history).map(|ev| Ok::<_, Infallible>(ev_to_sse(&ev)));

    // For terminal sessions with session_id filter: no live stream needed.
    // For all other cases: attach a live stream filtered by query params.
    let live_stream: futures::future::Either<_, _> = if session_already_done {
        futures::future::Either::Right(stream::empty())
    } else {
        futures::future::Either::Left(BroadcastStream::new(rx).filter_map(move |result| {
            let params = params_for_live.clone();
            async move {
                match result {
                    Ok(ev) if params.matches(&ev) => Some(Ok::<_, Infallible>(ev_to_sse(&ev))),
                    Ok(_) => None,
                    Err(_lagged) => None,
                }
            }
        }))
    };

    Sse::new(history_stream.chain(live_stream))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use events::{EventBus, IssueEvent, SystemEvent};
    use types::{Issue, IssueStatus};
    use uuid::Uuid;

    fn make_issue(id: &str) -> Issue {
        Issue {
            id: id.into(),
            title: "test".into(),
            body: "body".into(),
            status: IssueStatus::Open,
            branch: "main".into(),
            assignee: None,
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ── EventsQuery::matches ──────────────────────────────────────────────────

    #[test]
    fn matches_no_filter_accepts_all() {
        let q = EventsQuery {
            session_id: None,
            issue_id: None,
            types: None,
            last_turns: None,
        };
        let ev = SystemEvent::Issue(IssueEvent::Created(make_issue("ab12")));
        assert!(q.matches(&ev));
    }

    #[test]
    fn matches_types_session_filters_out_issue_events() {
        let q = EventsQuery {
            session_id: None,
            issue_id: None,
            types: Some("session".into()),
            last_turns: None,
        };
        let issue_ev = SystemEvent::Issue(IssueEvent::Created(make_issue("ab12")));
        assert!(
            !q.matches(&issue_ev),
            "issue event must not pass types=session filter"
        );

        let session_ev = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };
        assert!(
            q.matches(&session_ev),
            "session event must pass types=session filter"
        );
    }

    #[test]
    fn matches_types_issue_filters_out_session_events() {
        let q = EventsQuery {
            session_id: None,
            issue_id: None,
            types: Some("issue".into()),
            last_turns: None,
        };
        let session_ev = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };
        assert!(
            !q.matches(&session_ev),
            "session event must not pass types=issue filter"
        );

        let issue_ev = SystemEvent::Issue(IssueEvent::Created(make_issue("ab12")));
        assert!(
            q.matches(&issue_ev),
            "issue event must pass types=issue filter"
        );
    }

    #[test]
    fn matches_types_comma_separated() {
        let q = EventsQuery {
            session_id: None,
            issue_id: None,
            types: Some("session,issue".into()),
            last_turns: None,
        };
        let session_ev = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };
        let issue_ev = SystemEvent::Issue(IssueEvent::Created(make_issue("ab12")));
        assert!(q.matches(&session_ev));
        assert!(q.matches(&issue_ev));
    }

    #[test]
    fn matches_session_id_filters_wrong_session() {
        let target_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let q = EventsQuery {
            session_id: Some(target_id),
            issue_id: None,
            types: None,
            last_turns: None,
        };

        let matching = SystemEvent::Session {
            session_id: target_id,
            event: SessionEvent::Done,
        };
        let not_matching = SystemEvent::Session {
            session_id: other_id,
            event: SessionEvent::Done,
        };
        let issue_ev = SystemEvent::Issue(IssueEvent::Created(make_issue("ab12")));

        assert!(q.matches(&matching), "event for target session must pass");
        assert!(
            !q.matches(&not_matching),
            "event for other session must not pass"
        );
        assert!(
            !q.matches(&issue_ev),
            "issue event must not pass session_id filter"
        );
    }

    #[test]
    fn matches_issue_id_filters_correctly() {
        let q = EventsQuery {
            session_id: None,
            issue_id: Some("ab12".into()),
            types: None,
            last_turns: None,
        };

        let matching = SystemEvent::Issue(IssueEvent::Created(make_issue("ab12")));
        let not_matching = SystemEvent::Issue(IssueEvent::Created(make_issue("cd34")));
        let session_ev = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };

        assert!(q.matches(&matching), "event for target issue must pass");
        assert!(
            !q.matches(&not_matching),
            "event for other issue must not pass"
        );
        assert!(
            !q.matches(&session_ev),
            "session event must not pass issue_id filter"
        );
    }

    // ── Live stream passes filtered events ────────────────────────────────────

    #[tokio::test]
    async fn types_session_filter_blocks_issue_events_in_live_stream() {
        let bus = EventBus::new(32);
        let mut rx = bus.subscribe();

        // Emit an issue event
        bus.send(SystemEvent::Issue(IssueEvent::Created(make_issue("ab12"))));
        // Emit a session event
        let sid = Uuid::new_v4();
        bus.send(SystemEvent::Session {
            session_id: sid,
            event: SessionEvent::Done,
        });

        let q = EventsQuery {
            session_id: None,
            issue_id: None,
            types: Some("session".into()),
            last_turns: None,
        };

        // Drain the channel and filter
        let mut received = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if q.matches(&ev) {
                received.push(ev);
            }
        }

        assert_eq!(received.len(), 1, "only 1 session event should pass");
        assert!(matches!(received[0], SystemEvent::Session { .. }));
    }

    // ── Route-level integration tests ─────────────────────────────────────────

    /// Builds an in-memory `AppState` for route-level tests.
    /// Delegates to the shared `test_state()` helper in `crate::tests`.
    async fn make_route_state() -> crate::state::AppState {
        crate::tests::test_state().await
    }

    /// Helper: collect SSE body chunks with a deadline, returning all raw bytes received.
    ///
    /// Stops collecting when `deadline` elapses, returning whatever bytes arrived.
    async fn collect_sse_body_with_timeout(
        body: axum::body::Body,
        timeout: std::time::Duration,
    ) -> String {
        use http_body_util::BodyExt;
        use std::pin::pin;

        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut body = pin!(body);
        let mut collected = String::new();

        loop {
            tokio::select! {
                biased;
                () = &mut deadline => break,
                frame = body.frame() => {
                    match frame {
                        Some(Ok(f)) => {
                            if let Ok(data) = f.into_data() {
                                collected.push_str(&String::from_utf8_lossy(&data));
                            }
                        }
                        Some(Err(_)) | None => break,
                    }
                }
            }
        }

        collected
    }

    /// Route-level test: `GET /events?issue_id=ab12` must block events for cd34.
    ///
    /// This verifies that `params.matches(&ev)` is actually wired into the
    /// live-stream branch of the `events` handler (not just unit-tested in
    /// isolation).  A subscription to `issue_id=ab12` must only see events whose
    /// `issue.id` matches; an event for `cd34` must be silently dropped.
    #[tokio::test]
    async fn route_live_stream_filter_blocks_events_for_different_issue_id() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = make_route_state().await;
        let app = crate::build_router(state.clone());

        // Send the SSE request; the handler subscribes to the bus during this call.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/events?issue_id=ab12")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // Now pump the bus: one matching event and one non-matching event.
        // These go into the broadcast channel, ready for the stream to consume.
        state
            .event_bus
            .send(SystemEvent::Issue(IssueEvent::Created(make_issue("ab12"))));
        state
            .event_bus
            .send(SystemEvent::Issue(IssueEvent::Created(make_issue("cd34"))));

        // Collect whatever arrives within a short window (the two events should be
        // immediately available since they're already in the broadcast queue).
        let raw =
            collect_sse_body_with_timeout(resp.into_body(), std::time::Duration::from_millis(200))
                .await;

        // Parse the data: lines from the SSE output
        let data_lines: Vec<&str> = raw.lines().filter(|l| l.starts_with("data: ")).collect();

        // Exactly one event should have arrived: the ab12 one.
        assert_eq!(
            data_lines.len(),
            1,
            "only 1 data line expected (issue_id=ab12 filter); got: {data_lines:?}\nraw={raw:?}"
        );

        // Verify it is the ab12 event, not cd34.
        let json = &data_lines[0]["data: ".len()..];
        let ev: SystemEvent =
            serde_json::from_str(json).expect("SSE data must be valid SystemEvent JSON");
        match ev {
            SystemEvent::Issue(IssueEvent::Created(ref issue)) => {
                assert_eq!(
                    issue.id, "ab12",
                    "received event must be for issue ab12, not cd34"
                );
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    /// Route-level test: a terminal session returns historical events + Done and
    /// then the stream closes (does NOT hang open).
    ///
    /// This verifies that the `session_already_done` path in the `events` handler
    /// actually sets `session_already_done = true`, emits `SessionEvent::Done`,
    /// and then attaches an empty stream so the response body finishes.
    #[tokio::test]
    async fn route_terminal_session_returns_done_event_and_stream_closes() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = make_route_state().await;
        let app = crate::build_router(state.clone());

        // Pre-populate a completed session in the DB.
        let session = types::Session {
            id: Uuid::new_v4(),
            name: "done-route-test".into(),
            status: types::SessionStatus::Waiting,
            agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, types::SessionStatus::Waiting)
            .await
            .unwrap();

        // Open the SSE stream for the completed session.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // For a terminal session the stream must close on its own (history +
        // Done then empty live-stream).  We use to_bytes with a generous timeout
        // to confirm it terminates rather than hanging.
        let body_bytes = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            axum::body::to_bytes(resp.into_body(), usize::MAX),
        )
        .await
        .expect("SSE stream for completed session must close within 2 s")
        .expect("body read error");

        let raw = std::str::from_utf8(&body_bytes).unwrap();

        // Assert: a Done event is present in the SSE output.
        let has_done = raw.lines().filter(|l| l.starts_with("data: ")).any(|l| {
            let json = &l["data: ".len()..];
            serde_json::from_str::<SystemEvent>(json).is_ok_and(|ev| {
                matches!(
                    ev,
                    SystemEvent::Session {
                        event: SessionEvent::Done,
                        ..
                    }
                )
            })
        });
        assert!(
            has_done,
            "completed session SSE stream must contain a Done event; raw={raw:?}"
        );
    }

    /// Route-level test: a Waiting session SSE stream replays history, emits Done,
    /// and closes — does NOT hang open waiting for new events.
    ///
    /// This verifies that `SessionStatus::Waiting` is treated as terminal in the
    /// `events` handler (`session_already_done` = true path), fixing the bug where
    /// clients subscribed to a Waiting session would hang indefinitely.
    #[tokio::test]
    async fn route_waiting_session_emits_done_and_closes() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = make_route_state().await;
        let app = crate::build_router(state.clone());

        // Pre-populate a Waiting session in the DB.
        let session = types::Session {
            id: Uuid::new_v4(),
            name: "waiting-route-test".into(),
            status: types::SessionStatus::Waiting,
            agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.db.create_session(&session).await.unwrap();
        state
            .db
            .update_session_status(session.id, types::SessionStatus::Waiting)
            .await
            .unwrap();

        // Open the SSE stream for the Waiting session.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/events?session_id={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // For a terminal (Waiting) session the stream must close on its own.
        // We use to_bytes with a timeout: if it doesn't finish, the session is
        // keeping the stream open (the bug).
        let body_bytes = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            axum::body::to_bytes(resp.into_body(), usize::MAX),
        )
        .await
        .expect("SSE stream for Waiting session must close within 2 s — did not close (bug: Waiting not treated as terminal)")
        .expect("body read error");

        let raw = std::str::from_utf8(&body_bytes).unwrap();

        // Assert: a Done event is present in the SSE output.
        let has_done = raw.lines().filter(|l| l.starts_with("data: ")).any(|l| {
            let json = &l["data: ".len()..];
            serde_json::from_str::<SystemEvent>(json).is_ok_and(|ev| {
                matches!(
                    ev,
                    SystemEvent::Session {
                        event: SessionEvent::Done,
                        ..
                    }
                )
            })
        });
        assert!(
            has_done,
            "Waiting session SSE stream must contain a Done event; raw={raw:?}"
        );
    }

    // ── issue_id filter: only emits events for the matching issue ─────────────

    #[tokio::test]
    async fn issue_id_filter_only_passes_matching_issue_events() {
        let bus = EventBus::new(32);
        let mut rx = bus.subscribe();

        let target_id = "ab12";
        let other_id = "cd34";

        // Emit events for two different issues
        bus.send(SystemEvent::Issue(IssueEvent::Created(make_issue(
            target_id,
        ))));
        bus.send(SystemEvent::Issue(IssueEvent::StatusChanged {
            issue: make_issue(other_id),
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        }));
        bus.send(SystemEvent::Issue(IssueEvent::StatusChanged {
            issue: make_issue(target_id),
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        }));
        // Also emit a session event (should be filtered out)
        bus.send(SystemEvent::Session {
            session_id: uuid::Uuid::new_v4(),
            event: events::SessionEvent::Done,
        });

        let q = EventsQuery {
            session_id: None,
            issue_id: Some(target_id.to_string()),
            types: None,
            last_turns: None,
        };

        let mut received = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if q.matches(&ev) {
                received.push(ev);
            }
        }

        assert_eq!(
            received.len(),
            2,
            "only events for issue {target_id} should pass, got {received:?}"
        );
        for ev in &received {
            match ev {
                SystemEvent::Issue(ie) => {
                    let issue_id = match ie {
                        IssueEvent::Created(i) => &i.id,
                        IssueEvent::StatusChanged { issue, .. }
                        | IssueEvent::CommentAdded { issue, .. } => &issue.id,
                    };
                    assert_eq!(
                        issue_id, target_id,
                        "all received events must be for target issue"
                    );
                }
                _ => panic!("received non-issue event"),
            }
        }
    }
}
