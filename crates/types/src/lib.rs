use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Open,
    InProgress,
    Completed,
    Failed,
    Cancelled,
    Waiting,
}

impl std::fmt::Display for IssueStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Waiting => write!(f, "waiting"),
        }
    }
}

impl std::str::FromStr for IssueStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "open" => Ok(Self::Open),
            "in_progress" | "running" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "waiting" => Ok(Self::Waiting),
            _ => Err(format!("unknown issue status: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueComment {
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub body: String,
    pub status: IssueStatus,
    pub branch: String,
    pub assignee: Option<String>,
    pub session_id: Option<Uuid>,
    pub parent_id: Option<String>,
    pub blocked_on: Vec<String>,
    pub comments: Vec<IssueComment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Created,
    Running,
    Failed,
    Cancelled,
    Waiting,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Running => write!(f, "running"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Waiting => write!(f, "waiting"),
        }
    }
}

impl std::str::FromStr for SessionStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "created" => Ok(Self::Created),
            "running" => Ok(Self::Running),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "waiting" => Ok(Self::Waiting),
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
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

// ── Event domain types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    Webhook { secret: Option<String> },
    Timer { schedule: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub name: String,
    pub kind: EventKind,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Hook domain types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub id: String,
    pub name: String,
    pub source: HookSource,
    pub filter: Option<HookFilter>,
    pub action: HookAction,
    pub enabled: bool,
    pub created_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookSource {
    Internal { event_types: Vec<String> },
    External { secret: Option<String> },
    Timer { schedule: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookAction {
    SendMessage {
        target: MessageTarget,
        body: String,
    },
    CreateIssue {
        title: String,
        body: String,
        assignee: Option<String>,
        parent: Option<String>,
        start: bool,
    },
    RunShell {
        command: String,
        timeout_secs: u64,
        blocking: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
pub enum MessageTarget {
    Session(String),
    Issue(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookFilter {
    #[serde(default)]
    pub conditions: Vec<FieldCondition>,
    pub expression: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldCondition {
    pub field: String,
    pub op: Op,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    NotEq,
    Contains,
    Matches,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecution {
    pub id: String,
    pub hook_id: String,
    pub triggered_at: chrono::DateTime<chrono::Utc>,
    pub event_payload: serde_json::Value,
    pub status: ExecutionStatus,
    pub result: Option<String>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for ExecutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for ExecutionStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            other => Err(format!("unknown execution status: {other}")),
        }
    }
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
            SessionStatus::Failed,
            SessionStatus::Cancelled,
            SessionStatus::Waiting,
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
        let block = ContentBlock::Text {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&block).expect("serialize");
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

    // --- ContentBlockDelta serde round-trip ---

    #[test]
    fn content_block_delta_text_delta_serde_round_trip() {
        let delta = ContentBlockDelta::TextDelta {
            text: "chunk".into(),
        };
        let json = serde_json::to_string(&delta).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "text_delta");
        assert_eq!(v["text"], "chunk");
        let decoded: ContentBlockDelta = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, ContentBlockDelta::TextDelta { text } if text == "chunk"));
    }

    #[test]
    fn content_block_delta_input_json_delta_serde_round_trip() {
        let delta = ContentBlockDelta::InputJsonDelta {
            partial_json: "{\"k\":".into(),
        };
        let json = serde_json::to_string(&delta).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(v["type"], "input_json_delta");
        assert_eq!(v["partial_json"], "{\"k\":");
        let decoded: ContentBlockDelta = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(decoded, ContentBlockDelta::InputJsonDelta { partial_json } if partial_json == "{\"k\":")
        );
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

    // --- IssueStatus ---

    #[test]
    fn issue_status_display_round_trip() {
        for status in [
            IssueStatus::Open,
            IssueStatus::InProgress,
            IssueStatus::Completed,
            IssueStatus::Failed,
            IssueStatus::Cancelled,
            IssueStatus::Waiting,
        ] {
            let s = status.to_string();
            let parsed: IssueStatus = s.parse().expect("should parse");
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn issue_status_running_parses_to_in_progress() {
        // Legacy "running" string maps to InProgress (backward compat)
        let parsed: IssueStatus = "running".parse().expect("should parse");
        assert_eq!(parsed, IssueStatus::InProgress);
    }

    #[test]
    fn issue_status_in_progress_display() {
        assert_eq!(IssueStatus::InProgress.to_string(), "in_progress");
    }

    #[test]
    fn issue_status_in_progress_from_str() {
        let s: IssueStatus = "in_progress".parse().expect("should parse");
        assert_eq!(s, IssueStatus::InProgress);
    }

    #[test]
    fn issue_status_in_progress_serde_roundtrip() {
        let json = serde_json::to_string(&IssueStatus::InProgress).expect("serialize");
        assert_eq!(json, "\"in_progress\"");
        let decoded: IssueStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, IssueStatus::InProgress);
    }

    #[test]
    fn issue_status_from_str_unknown_returns_err() {
        let result: std::result::Result<IssueStatus, _> = "bogus".parse();
        assert!(result.is_err());
    }

    #[test]
    fn session_status_waiting_round_trip() {
        let s = SessionStatus::Waiting.to_string();
        assert_eq!(s, "waiting");
        let parsed: SessionStatus = s.parse().unwrap();
        assert_eq!(parsed, SessionStatus::Waiting);
    }

    // ── Event / EventKind serde round-trips ───────────────────────────────────

    #[test]
    fn event_kind_webhook_serde_round_trip() {
        let kind = EventKind::Webhook {
            secret: Some("s3cr3t".into()),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: EventKind = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, EventKind::Webhook { secret: Some(ref s) } if s == "s3cr3t"));
    }

    #[test]
    fn event_kind_timer_serde_round_trip() {
        let kind = EventKind::Timer {
            schedule: "0 9 * * 1".into(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: EventKind = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, EventKind::Timer { ref schedule } if schedule == "0 9 * * 1")
        );
    }

    #[test]
    fn event_serde_round_trip() {
        let ev = Event {
            id: "abcd".into(),
            name: "ci-complete".into(),
            kind: EventKind::Webhook {
                secret: Some("s3cr3t".into()),
            },
            description: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "abcd");
        assert_eq!(back.name, "ci-complete");
        assert!(back.description.is_none());
        assert!(
            matches!(back.kind, EventKind::Webhook { secret: Some(ref s) } if s == "s3cr3t")
        );
    }

    #[test]
    fn event_kind_tag_is_snake_case() {
        let webhook_kind = EventKind::Webhook { secret: None };
        let v: serde_json::Value = serde_json::to_value(&webhook_kind).unwrap();
        assert_eq!(v["type"], "webhook");

        let timer_kind = EventKind::Timer {
            schedule: "* * * * *".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&timer_kind).unwrap();
        assert_eq!(v["type"], "timer");
    }

    #[test]
    fn issue_status_waiting_round_trip() {
        let s = IssueStatus::Waiting.to_string();
        assert_eq!(s, "waiting");
        let parsed: IssueStatus = s.parse().unwrap();
        assert_eq!(parsed, IssueStatus::Waiting);
    }

    #[test]
    fn issue_status_display_cancelled() {
        assert_eq!(IssueStatus::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn issue_status_from_str_cancelled() {
        let s: IssueStatus = "cancelled".parse().expect("should parse");
        assert_eq!(s, IssueStatus::Cancelled);
    }

    // --- Issue serde round-trip ---

    #[test]
    fn issue_serde_round_trip() {
        let issue = Issue {
            id: "ab12".into(),
            title: "Fix the bug".into(),
            body: "Details here".into(),
            status: IssueStatus::Open,
            branch: "feat/fix-the-bug".into(),
            assignee: Some("swe".into()),
            session_id: None,
            parent_id: None,
            blocked_on: vec!["xy34".into()],
            comments: vec![IssueComment {
                author: "user".into(),
                created_at: Utc::now(),
                body: "A comment".into(),
            }],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&issue).expect("serialize");
        let decoded: Issue = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.id, "ab12");
        assert_eq!(decoded.title, "Fix the bug");
        assert_eq!(decoded.branch, "feat/fix-the-bug");
        assert_eq!(decoded.blocked_on, vec!["xy34"]);
        assert_eq!(decoded.comments.len(), 1);
        assert_eq!(decoded.comments[0].author, "user");
        assert_eq!(decoded.status, IssueStatus::Open);
    }
}
