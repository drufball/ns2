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
                        IssueEvent::StatusChanged { issue, .. } | IssueEvent::CommentAdded { issue, .. } => &issue.id,
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
                types::SessionStatus::Completed
                | types::SessionStatus::Failed
                | types::SessionStatus::Cancelled => {
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
        futures::future::Either::Left(
            BroadcastStream::new(rx).filter_map(move |result| {
                let params = params_for_live.clone();
                async move {
                    match result {
                        Ok(ev) if params.matches(&ev) => Some(Ok::<_, Infallible>(ev_to_sse(&ev))),
                        Ok(_) => None,
                        Err(_lagged) => None,
                    }
                }
            }),
        )
    };

    Sse::new(history_stream.chain(live_stream))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use events::{EventBus, IssueEvent, SystemEvent};
    use types::{Issue, IssueStatus};
    use uuid::Uuid;
    use chrono::Utc;

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
        let q = EventsQuery { session_id: None, issue_id: None, types: None, last_turns: None };
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
        assert!(!q.matches(&issue_ev), "issue event must not pass types=session filter");

        let session_ev = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };
        assert!(q.matches(&session_ev), "session event must pass types=session filter");
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
        assert!(!q.matches(&session_ev), "session event must not pass types=issue filter");

        let issue_ev = SystemEvent::Issue(IssueEvent::Created(make_issue("ab12")));
        assert!(q.matches(&issue_ev), "issue event must pass types=issue filter");
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
        assert!(!q.matches(&not_matching), "event for other session must not pass");
        assert!(!q.matches(&issue_ev), "issue event must not pass session_id filter");
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
        assert!(!q.matches(&not_matching), "event for other issue must not pass");
        assert!(!q.matches(&session_ev), "session event must not pass issue_id filter");
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
        bus.send(SystemEvent::Session { session_id: sid, event: SessionEvent::Done });

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

    // ── issue_id filter: only emits events for the matching issue ─────────────

    #[tokio::test]
    async fn issue_id_filter_only_passes_matching_issue_events() {
        let bus = EventBus::new(32);
        let mut rx = bus.subscribe();

        let target_id = "ab12";
        let other_id = "cd34";

        // Emit events for two different issues
        bus.send(SystemEvent::Issue(IssueEvent::Created(make_issue(target_id))));
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
                        IssueEvent::StatusChanged { issue, .. } | IssueEvent::CommentAdded { issue, .. } => &issue.id,
                    };
                    assert_eq!(issue_id, target_id, "all received events must be for target issue");
                }
                _ => panic!("received non-issue event"),
            }
        }
    }
}
