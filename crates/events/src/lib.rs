use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use types::{ContentBlock, ContentBlockDelta, Issue, IssueComment, IssueStatus, Turn};
use uuid::Uuid;

// ── SessionEvent ──────────────────────────────────────────────────────────────

/// Events produced by a single harness session.
///
/// This replaces `types::SessionEvent`.  The `session_id` is carried by the
/// outer `SystemEvent::Session` wrapper rather than by the `Done` variant so
/// that the outer bus can fan out events per-session without duplicating the id
/// in every payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    TurnStarted {
        turn: Turn,
    },
    ContentBlockDelta {
        turn_id: Uuid,
        index: u32,
        delta: ContentBlockDelta,
    },
    ContentBlockDone {
        turn_id: Uuid,
        index: u32,
        block: ContentBlock,
    },
    TurnDone {
        turn_id: Uuid,
    },
    ToolUseStart {
        id: Uuid,
        turn_id: Uuid,
        name: String,
        input: serde_json::Value,
    },
    ToolUseDone {
        id: Uuid,
        turn_id: Uuid,
        name: String,
        output: String,
    },
    /// Emitted just before `Done` when the agent explicitly called the `stop` tool.
    /// Carries the stop status and an optional comment to add to the linked issue.
    Stopped {
        status: StopEventStatus,
        comment: Option<String>,
    },
    /// Emitted when the session finishes successfully.  The `session_id` is on
    /// the outer `SystemEvent::Session { session_id, .. }` wrapper.
    Done,
    Error {
        message: String,
    },
}

/// The status value carried by `SessionEvent::Stopped`.
///
/// Mirrors `tools::StopStatus` but lives in `events` so that crates
/// consuming the event bus do not need to depend on the `tools` crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopEventStatus {
    Complete,
    Waiting,
}

// ── IssueEvent ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IssueEvent {
    Created(Issue),
    StatusChanged {
        issue: Issue,
        from: IssueStatus,
        to: IssueStatus,
    },
    CommentAdded {
        issue: Issue,
        comment: IssueComment,
    },
}

// ── SystemEvent ───────────────────────────────────────────────────────────────

/// Top-level envelope that flows through the global `EventBus`.
///
/// Uses adjacently-tagged serde (`tag = "type", content = "data"`) so that
/// inner variants which are themselves internally-tagged (e.g. `IssueEvent`)
/// do not cause a "duplicate field `type`" serde error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum SystemEvent {
    Session {
        session_id: Uuid,
        event: SessionEvent,
    },
    Issue(IssueEvent),
    External {
        event_id: String,
        event_name: String,
        payload: serde_json::Value,
    },
    TimerFired {
        event_id: String,
        event_name: String,
        fired_at: DateTime<Utc>,
    },
    Custom {
        event_type: String,
        payload: serde_json::Value,
    },
    McpChannelNotification {
        channel_id: String,
        body: String,
        meta: std::collections::HashMap<String, String>,
    },
}

// ── EventBus ──────────────────────────────────────────────────────────────────

/// A cheaply-cloneable global publish/subscribe bus.
///
/// Internally backed by a `tokio::sync::broadcast` channel whose sender is
/// wrapped in an `Arc` so cloning is O(1).
#[derive(Clone, Debug)]
pub struct EventBus {
    tx: broadcast::Sender<SystemEvent>,
}

impl EventBus {
    /// Create a new bus with the given channel capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Subscribe to the bus.  Returns a `Receiver` that will see all events
    /// sent *after* this call.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SystemEvent> {
        self.tx.subscribe()
    }

    /// Send an event to all current subscribers.
    ///
    /// Fire-and-forget: if there are no subscribers or the channel is lagged,
    /// the error is logged at TRACE level and silently discarded.
    pub fn send(&self, event: SystemEvent) {
        if let Err(e) = self.tx.send(event) {
            tracing::trace!("EventBus::send: no active subscribers (lagged/dropped): {e}");
        }
    }
}

// ── Event ID generation ───────────────────────────────────────────────────────

/// Generate a short random event ID (4 lower-alphanumeric characters).
///
/// Uses the same alphabet and UUID-byte-sampling approach as `generate_hook_id`
/// in the `hooks` crate so that the IDs are short, URL-safe, and practically
/// unique for their intended use as ephemeral correlation tokens.
#[must_use]
pub fn generate_event_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let id = Uuid::new_v4();
    let bytes = id.as_bytes();
    (0..4)
        .map(|i| ALPHABET[(bytes[i] as usize) % ALPHABET.len()] as char)
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    // ── Scenario B: McpChannelNotification round-trip ────────────────────────

    #[test]
    fn system_event_mcp_channel_notification_serde_round_trip() {
        let mut meta = std::collections::HashMap::new();
        meta.insert("issue_id".to_string(), "ab12".to_string());
        let ev = SystemEvent::McpChannelNotification {
            channel_id: "alice".into(),
            body: "hi".into(),
            meta,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "mcp_channel_notification");
        assert_eq!(v["data"]["channel_id"], "alice");
        assert_eq!(v["data"]["body"], "hi");
        assert_eq!(v["data"]["meta"]["issue_id"], "ab12");

        let decoded: SystemEvent = serde_json::from_str(&json).unwrap();
        match decoded {
            SystemEvent::McpChannelNotification {
                channel_id,
                body,
                meta,
            } => {
                assert_eq!(channel_id, "alice");
                assert_eq!(body, "hi");
                assert_eq!(meta.get("issue_id").map(String::as_str), Some("ab12"));
            }
            _ => panic!("expected McpChannelNotification variant"),
        }
    }

    // ── EventBus unit tests ───────────────────────────────────────────────────

    /// A single subscriber receives events sent after subscribing.
    #[tokio::test]
    async fn event_bus_single_subscriber_receives_event() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();

        let session_id = Uuid::new_v4();
        bus.send(SystemEvent::Session {
            session_id,
            event: SessionEvent::Done,
        });

        let received = rx.try_recv().expect("should have received an event");
        assert!(
            matches!(received, SystemEvent::Session { session_id: sid, event: SessionEvent::Done } if sid == session_id)
        );
    }

    /// Multiple subscribers each receive the same event.
    #[tokio::test]
    async fn event_bus_multiple_subscribers_each_receive_event() {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        let session_id = Uuid::new_v4();
        bus.send(SystemEvent::Session {
            session_id,
            event: SessionEvent::Done,
        });

        let e1 = rx1.try_recv().expect("rx1 should have received");
        let e2 = rx2.try_recv().expect("rx2 should have received");
        assert!(matches!(e1, SystemEvent::Session { .. }));
        assert!(matches!(e2, SystemEvent::Session { .. }));
    }

    /// `send` is fire-and-forget: no subscribers → no panic.
    #[test]
    fn event_bus_send_with_no_subscribers_does_not_panic() {
        let bus = EventBus::new(8);
        // No subscribers; this must not panic.
        bus.send(SystemEvent::TimerFired {
            event_id: "t1".into(),
            event_name: "heartbeat".into(),
            fired_at: Utc::now(),
        });
    }

    // ── SystemEvent serde round-trips ─────────────────────────────────────────

    #[test]
    fn system_event_session_serde_round_trip() {
        let turn = Turn {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            token_count: None,
            created_at: Utc::now(),
        };
        let ev = SystemEvent::Session {
            session_id: turn.session_id,
            event: SessionEvent::TurnStarted { turn: turn.clone() },
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SystemEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SystemEvent::Session { session_id, event: SessionEvent::TurnStarted { turn: t } } if session_id == turn.session_id && t.id == turn.id)
        );
    }

    #[test]
    fn system_event_issue_serde_round_trip() {
        let issue = types::Issue {
            id: "ab12".into(),
            title: "Test".into(),
            body: "Body".into(),
            status: IssueStatus::Open,
            branch: "main".into(),
            assignee: None,
            session_id: None,
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let ev = SystemEvent::Issue(IssueEvent::Created(issue));
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SystemEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SystemEvent::Issue(IssueEvent::Created(ref i)) if i.id == "ab12")
        );
    }

    #[test]
    fn system_event_external_serde_round_trip() {
        let ev = SystemEvent::External {
            event_id: "evt-42".into(),
            event_name: "ci-complete".into(),
            payload: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SystemEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SystemEvent::External { ref event_id, ref event_name, .. } if event_id == "evt-42" && event_name == "ci-complete")
        );
    }

    #[test]
    fn system_event_timer_fired_serde_round_trip() {
        let now = Utc::now();
        let ev = SystemEvent::TimerFired {
            event_id: "timer-1".into(),
            event_name: "heartbeat".into(),
            fired_at: now,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SystemEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SystemEvent::TimerFired { ref event_id, ref event_name, .. } if event_id == "timer-1" && event_name == "heartbeat")
        );
    }

    #[test]
    fn system_event_custom_serde_round_trip() {
        let ev = SystemEvent::Custom {
            event_type: "custom.test".into(),
            payload: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SystemEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SystemEvent::Custom { ref event_type, .. } if event_type == "custom.test")
        );
    }

    // ── IssueEvent serde round-trips ──────────────────────────────────────────

    fn make_issue() -> types::Issue {
        types::Issue {
            id: "cd34".into(),
            title: "Issue".into(),
            body: "body".into(),
            status: IssueStatus::Open,
            branch: "main".into(),
            assignee: None,
            session_id: None,
            parent_id: None,
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn issue_event_created_serde_round_trip() {
        let ev = IssueEvent::Created(make_issue());
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: IssueEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, IssueEvent::Created(ref i) if i.id == "cd34"));
    }

    #[test]
    fn issue_event_status_changed_serde_round_trip() {
        let ev = IssueEvent::StatusChanged {
            issue: make_issue(),
            from: IssueStatus::Open,
            to: IssueStatus::InProgress,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: IssueEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            IssueEvent::StatusChanged {
                from: IssueStatus::Open,
                to: IssueStatus::InProgress,
                ..
            }
        ));
    }

    #[test]
    fn issue_event_comment_added_serde_round_trip() {
        let comment = IssueComment {
            author: "user".into(),
            created_at: Utc::now(),
            body: "A comment".into(),
        };
        let ev = IssueEvent::CommentAdded {
            issue: make_issue(),
            comment,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: IssueEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, IssueEvent::CommentAdded { ref comment, .. } if comment.author == "user")
        );
    }

    // ── SessionEvent serde round-trips (migrated from types) ─────────────────

    #[test]
    fn session_event_turn_started_serde_round_trip() {
        let turn = Turn {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            token_count: None,
            created_at: Utc::now(),
        };
        let ev = SessionEvent::TurnStarted { turn: turn.clone() };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "turn_started");
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, SessionEvent::TurnStarted { turn: t } if t.id == turn.id));
    }

    #[test]
    fn session_event_content_block_delta_serde_round_trip() {
        let turn_id = Uuid::new_v4();
        let ev = SessionEvent::ContentBlockDelta {
            turn_id,
            index: 0,
            delta: ContentBlockDelta::TextDelta { text: "hi".into() },
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "content_block_delta");
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::ContentBlockDelta { turn_id: tid, .. } if tid == turn_id)
        );
    }

    #[test]
    fn session_event_content_block_done_serde_round_trip() {
        let turn_id = Uuid::new_v4();
        let ev = SessionEvent::ContentBlockDone {
            turn_id,
            index: 0,
            block: ContentBlock::Text {
                text: "world".into(),
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "content_block_done");
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::ContentBlockDone { turn_id: tid, .. } if tid == turn_id)
        );
    }

    #[test]
    fn session_event_turn_done_serde_round_trip() {
        let turn_id = Uuid::new_v4();
        let ev = SessionEvent::TurnDone { turn_id };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "turn_done");
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, SessionEvent::TurnDone { turn_id: tid } if tid == turn_id));
    }

    #[test]
    fn session_event_done_serde_round_trip() {
        let ev = SessionEvent::Done;
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "done");
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, SessionEvent::Done));
    }

    #[test]
    fn session_event_stopped_complete_serde_round_trip() {
        let ev = SessionEvent::Stopped {
            status: StopEventStatus::Complete,
            comment: Some("all done".into()),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "stopped");
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::Stopped { status: StopEventStatus::Complete, comment: Some(ref c) } if c == "all done")
        );
    }

    #[test]
    fn session_event_stopped_waiting_no_comment_serde_round_trip() {
        let ev = SessionEvent::Stopped {
            status: StopEventStatus::Waiting,
            comment: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::Stopped { status: StopEventStatus::Waiting, comment: None })
        );
    }

    #[test]
    fn session_event_error_serde_round_trip() {
        let ev = SessionEvent::Error {
            message: "oops".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "error");
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, SessionEvent::Error { ref message } if message == "oops"));
    }

    // ── generate_event_id tests ───────────────────────────────────────────────

    #[test]
    fn generate_event_id_is_4_chars() {
        let id = generate_event_id();
        assert_eq!(id.len(), 4);
        assert!(
            id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "id must be lower-alphanumeric, got: {id:?}"
        );
    }

    #[test]
    fn generate_event_id_is_unique() {
        let ids: std::collections::HashSet<String> =
            (0..100).map(|_| generate_event_id()).collect();
        assert!(ids.len() > 90, "expected >90 unique IDs, got {}", ids.len());
    }

    /// Verify that `generate_event_id` uses the full lower-alphanumeric alphabet
    /// (guards against mutant `% → /` which would produce garbage characters
    /// and not sample the alphabet at all).
    #[test]
    fn generate_event_id_uses_full_alphabet() {
        let ids: Vec<String> = (0..10_000).map(|_| generate_event_id()).collect();
        let chars: std::collections::HashSet<char> = ids.concat().chars().collect();
        assert!(
            chars.len() >= 30,
            "expected at least 30 distinct chars from the alphabet, got {}",
            chars.len()
        );
        // Every character must be lower-alphanumeric (guards against garbage from
        // the mutant `% → /`).
        for c in &chars {
            assert!(
                c.is_ascii_lowercase() || c.is_ascii_digit(),
                "non-alphanumeric character found: {c:?}"
            );
        }
    }
}
