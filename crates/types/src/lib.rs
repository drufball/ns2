use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Created,
    Running,
    Completed,
    Failed,
    Cancelled,
}


impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionStatus::Created => write!(f, "created"),
            SessionStatus::Running => write!(f, "running"),
            SessionStatus::Completed => write!(f, "completed"),
            SessionStatus::Failed => write!(f, "failed"),
            SessionStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::str::FromStr for SessionStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "created" => Ok(SessionStatus::Created),
            "running" => Ok(SessionStatus::Running),
            "completed" => Ok(SessionStatus::Completed),
            "failed" => Ok(SessionStatus::Failed),
            "cancelled" => Ok(SessionStatus::Cancelled),
            _ => Err(format!("unknown status: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub name: String,
    pub status: SessionStatus,
    pub agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: Uuid,
    pub session_id: Uuid,
    pub token_count: Option<i64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    TurnStarted { turn: Turn },
    ContentBlockDelta { turn_id: Uuid, index: u32, delta: ContentBlockDelta },
    ContentBlockDone { turn_id: Uuid, index: u32, block: ContentBlock },
    TurnDone { turn_id: Uuid },
    SessionDone { session_id: Uuid },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    // --- SessionStatus::Display ---

    #[test]
    fn session_status_display_created() {
        assert_eq!(SessionStatus::Created.to_string(), "created");
    }

    #[test]
    fn session_status_display_running() {
        assert_eq!(SessionStatus::Running.to_string(), "running");
    }

    #[test]
    fn session_status_display_completed() {
        assert_eq!(SessionStatus::Completed.to_string(), "completed");
    }

    #[test]
    fn session_status_display_failed() {
        assert_eq!(SessionStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn session_status_display_cancelled() {
        assert_eq!(SessionStatus::Cancelled.to_string(), "cancelled");
    }

    // --- SessionStatus::from_str ---

    #[test]
    fn session_status_from_str_round_trip() {
        for status in [
            SessionStatus::Created,
            SessionStatus::Running,
            SessionStatus::Completed,
            SessionStatus::Failed,
            SessionStatus::Cancelled,
        ] {
            let s = status.to_string();
            let parsed = SessionStatus::from_str(&s).expect("should parse");
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn session_status_from_str_unknown_returns_err() {
        let result = SessionStatus::from_str("bogus");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("unknown status"));
    }

    // --- Session serde round-trip ---

    #[test]
    fn session_serde_round_trip() {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let session = Session {
            id,
            name: "test-session".into(),
            status: SessionStatus::Running,
            agent: Some("claude".into()),
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string(&session).expect("serialize");
        let decoded: Session = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.name, "test-session");
        assert_eq!(decoded.status, SessionStatus::Running);
        assert_eq!(decoded.agent.as_deref(), Some("claude"));
    }

    // --- Turn serde round-trip ---

    #[test]
    fn turn_serde_round_trip() {
        let id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let turn = Turn {
            id,
            session_id,
            token_count: Some(42),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&turn).expect("serialize");
        let decoded: Turn = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.session_id, session_id);
        assert_eq!(decoded.token_count, Some(42));
    }

    // --- ContentBlock serde round-trip and tagged shape ---

    #[test]
    fn content_block_text_serde_round_trip() {
        let block = ContentBlock::Text { text: "hello".into() };
        let json = serde_json::to_string(&block).expect("serialize");
        // The serde tag should produce {"type":"text","text":"..."}
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "text");
        assert_eq!(v["text"], "hello");

        let decoded: ContentBlock = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, ContentBlock::Text { text } if text == "hello"));
    }

    #[test]
    fn content_block_tool_use_serde_round_trip() {
        let block = ContentBlock::ToolUse {
            id: "tool-1".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        };
        let json = serde_json::to_string(&block).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "tool_use");
        assert_eq!(v["id"], "tool-1");
        assert_eq!(v["name"], "bash");

        let decoded: ContentBlock = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, ContentBlock::ToolUse { id, .. } if id == "tool-1"));
    }

    #[test]
    fn content_block_tool_result_serde_round_trip() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "tool-1".into(),
            content: "output".into(),
        };
        let json = serde_json::to_string(&block).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "tool_result");
        assert_eq!(v["tool_use_id"], "tool-1");

        let decoded: ContentBlock = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(decoded, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "tool-1")
        );
    }

    // --- SessionEvent serde round-trip and tagged shape ---

    #[test]
    fn session_event_turn_started_serde_round_trip() {
        let turn = Turn {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            token_count: None,
            created_at: Utc::now(),
        };
        let event = SessionEvent::TurnStarted { turn: turn.clone() };
        let json = serde_json::to_string(&event).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "turn_started");

        let decoded: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, SessionEvent::TurnStarted { turn: t } if t.id == turn.id));
    }

    #[test]
    fn session_event_content_block_delta_serde_round_trip() {
        let turn_id = Uuid::new_v4();
        let event = SessionEvent::ContentBlockDelta {
            turn_id,
            index: 0,
            delta: ContentBlockDelta::TextDelta { text: "hi".into() },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "content_block_delta");

        let decoded: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, SessionEvent::ContentBlockDelta { turn_id: tid, .. } if tid == turn_id));
    }

    #[test]
    fn session_event_content_block_done_serde_round_trip() {
        let turn_id = Uuid::new_v4();
        let event = SessionEvent::ContentBlockDone {
            turn_id,
            index: 0,
            block: ContentBlock::Text { text: "world".into() },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "content_block_done");
        let decoded: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, SessionEvent::ContentBlockDone { turn_id: tid, .. } if tid == turn_id));
    }

    #[test]
    fn session_event_turn_done_serde_round_trip() {
        let turn_id = Uuid::new_v4();
        let event = SessionEvent::TurnDone { turn_id };
        let json = serde_json::to_string(&event).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "turn_done");
        let decoded: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, SessionEvent::TurnDone { turn_id: tid } if tid == turn_id));
    }

    #[test]
    fn session_event_session_done_serde_round_trip() {
        let session_id = Uuid::new_v4();
        let event = SessionEvent::SessionDone { session_id };
        let json = serde_json::to_string(&event).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "session_done");
        let decoded: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(decoded, SessionEvent::SessionDone { session_id: sid } if sid == session_id)
        );
    }

    #[test]
    fn session_event_error_serde_round_trip() {
        let event = SessionEvent::Error { message: "oops".into() };
        let json = serde_json::to_string(&event).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "error");
        let decoded: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, SessionEvent::Error { message } if message == "oops"));
    }

    // --- ContentBlockDelta serde round-trip ---

    #[test]
    fn content_block_delta_text_delta_serde_round_trip() {
        let delta = ContentBlockDelta::TextDelta { text: "chunk".into() };
        let json = serde_json::to_string(&delta).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "text_delta");
        assert_eq!(v["text"], "chunk");
        let decoded: ContentBlockDelta = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, ContentBlockDelta::TextDelta { text } if text == "chunk"));
    }

    #[test]
    fn content_block_delta_input_json_delta_serde_round_trip() {
        let delta = ContentBlockDelta::InputJsonDelta { partial_json: "{\"k\":".into() };
        let json = serde_json::to_string(&delta).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "input_json_delta");
        assert_eq!(v["partial_json"], "{\"k\":");
        let decoded: ContentBlockDelta = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, ContentBlockDelta::InputJsonDelta { partial_json } if partial_json == "{\"k\":"));
    }

    #[test]
    fn tool_definition_serde_round_trip() {
        let def = ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        };
        let json = serde_json::to_string(&def).expect("serialize");
        let decoded: ToolDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.name, "read");
        assert_eq!(decoded.description, "Read a file");
    }

    // --- Role serde ---

    #[test]
    fn role_serde_user() {
        let json = serde_json::to_string(&Role::User).expect("serialize");
        assert_eq!(json, "\"user\"");
        let decoded: Role = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, Role::User);
    }

    #[test]
    fn role_serde_assistant() {
        let json = serde_json::to_string(&Role::Assistant).expect("serialize");
        assert_eq!(json, "\"assistant\"");
        let decoded: Role = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, Role::Assistant);
    }
}
