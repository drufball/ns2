use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Hook types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub id: String,
    pub name: String,
    pub source: HookSource,
    pub filter: Option<HookFilter>,
    pub action: HookAction,
    pub enabled: bool,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
        /// minijinja template string
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
    /// All conditions must be true (AND'd).
    #[serde(default)]
    pub conditions: Vec<FieldCondition>,
    /// Optional `JMESPath` expression (reserved for future use).
    pub expression: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldCondition {
    /// Dot-path into the JSON event payload (e.g. `"data.issue.status"`).
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

// ── HookExecution ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecution {
    pub id: String,
    pub hook_id: String,
    pub triggered_at: DateTime<Utc>,
    pub event_payload: serde_json::Value,
    pub status: ExecutionStatus,
    pub result: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
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

// ── Filter evaluation ─────────────────────────────────────────────────────────

pub mod evaluate {
    use events::{IssueEvent, SessionEvent, SystemEvent};
    use super::{FieldCondition, Hook, HookFilter, HookSource, Op};

    /// Map a `SystemEvent` to its canonical event-type string(s).
    #[must_use] 
    pub fn event_type_strings(event: &SystemEvent) -> Vec<&'static str> {
        match event {
            SystemEvent::Issue(IssueEvent::Created(_)) => vec!["issue.created"],
            SystemEvent::Issue(IssueEvent::StatusChanged { .. }) => vec!["issue.status_changed"],
            SystemEvent::Issue(IssueEvent::CommentAdded { .. }) => vec!["issue.comment_added"],
            SystemEvent::Session { event: SessionEvent::Done, .. } => vec!["session.done"],
            SystemEvent::Session { event: SessionEvent::TurnDone { .. }, .. } => vec!["session.turn_done"],
            SystemEvent::Session { event: SessionEvent::Error { .. }, .. } => vec!["session.error"],
            SystemEvent::Session { event: SessionEvent::TurnStarted { .. }, .. } => vec!["session.turn_started"],
            SystemEvent::Session { .. }
            | SystemEvent::External { .. }
            | SystemEvent::TimerFired { .. } => vec![],
        }
    }

    /// Returns `true` when `event` should trigger the given `hook`.
    #[must_use] 
    pub fn matches_event(hook: &Hook, event: &SystemEvent) -> bool {
        // 1. Check source: only Internal hooks are matched against live events.
        let HookSource::Internal { event_types } = &hook.source else {
            return false;
        };

        // Wildcard "*" matches everything; otherwise check the event type strings.
        let type_match = event_types.iter().any(|et| {
            et == "*" || event_type_strings(event).contains(&et.as_str())
        });
        if !type_match {
            return false;
        }

        // 2. Apply optional filter.
        if let Some(filter) = &hook.filter {
            let event_json = serde_json::to_value(event).unwrap_or_default();
            if !evaluate_filter(filter, &event_json) {
                return false;
            }
        }

        true
    }

    fn evaluate_filter(filter: &HookFilter, event_json: &serde_json::Value) -> bool {
        // All conditions must pass (AND).
        for cond in &filter.conditions {
            if !evaluate_condition(cond, event_json) {
                return false;
            }
        }
        true
    }

    pub(crate) fn navigate_path<'a>(
        json: &'a serde_json::Value,
        path: &str,
    ) -> Option<&'a serde_json::Value> {
        let mut current = json;
        for key in path.split('.') {
            current = current.get(key)?;
        }
        Some(current)
    }

    fn evaluate_condition(cond: &FieldCondition, event_json: &serde_json::Value) -> bool {
        let actual = navigate_path(event_json, &cond.field);
        match &cond.op {
            Op::Eq => actual == Some(&cond.value),
            Op::NotEq => actual != Some(&cond.value),
            Op::Contains => {
                // For strings: actual string contains the value string.
                // For arrays: actual array contains the value.
                match (actual, &cond.value) {
                    (Some(serde_json::Value::String(s)), serde_json::Value::String(v)) => {
                        s.contains(v.as_str())
                    }
                    (Some(serde_json::Value::Array(arr)), v) => arr.contains(v),
                    _ => false,
                }
            }
            Op::Matches => {
                // Simple glob-style match using `*` wildcards.
                match (actual, &cond.value) {
                    (Some(serde_json::Value::String(s)), serde_json::Value::String(pattern)) => {
                        glob_match(pattern, s)
                    }
                    _ => false,
                }
            }
        }
    }

    /// Minimal glob matching supporting `*` wildcard anywhere in the pattern.
    fn glob_match(pattern: &str, text: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        let parts: Vec<&str> = pattern.split('*').collect();
        let mut remaining = text;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                if !remaining.starts_with(part) {
                    return false;
                }
                remaining = &remaining[part.len()..];
            } else if i == parts.len() - 1 {
                return remaining.ends_with(part);
            } else if let Some(pos) = remaining.find(part) {
                remaining = &remaining[pos + part.len()..];
            } else {
                return false;
            }
        }
        true
    }
}

// ── Template rendering ────────────────────────────────────────────────────────

pub mod template {
    /// Render a minijinja template with the event JSON as context variable `event`.
    pub fn render_template(template_str: &str, event_json: &serde_json::Value) -> String {
        let mut env = minijinja::Environment::new();
        // Chainable: undefined attribute access returns empty string (no error)
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);
        match env.render_str(template_str, minijinja::context! { event => event_json }) {
            Ok(rendered) => rendered,
            Err(e) => {
                tracing::warn!("Failed to render hook template: {e}");
                template_str.to_string()
            }
        }
    }
}

// ── Action execution ──────────────────────────────────────────────────────────

pub mod execute {
    use chrono::Utc;
    use events::SystemEvent;
    use super::{Hook, HookAction, HookExecution, ExecutionStatus, MessageTarget};
    use super::template::render_template;
    use super::store::HookStore;

    pub async fn run_action(
        hook: &Hook,
        event: &SystemEvent,
        issue_svc: &issues::IssueService,
        hook_store: &dyn HookStore,
    ) {
        let event_json = serde_json::to_value(event).unwrap_or_default();
        let exec_id = uuid::Uuid::new_v4().to_string();
        let mut exec = HookExecution {
            id: exec_id,
            hook_id: hook.id.clone(),
            triggered_at: Utc::now(),
            event_payload: event_json.clone(),
            status: ExecutionStatus::Running,
            result: None,
            completed_at: None,
        };
        let _ = hook_store.create_execution(&exec).await;

        let result = execute_action_inner(hook, &event_json, issue_svc).await;

        exec.completed_at = Some(Utc::now());
        match result {
            Ok(msg) => {
                exec.status = ExecutionStatus::Completed;
                exec.result = Some(msg);
            }
            Err(e) => {
                exec.status = ExecutionStatus::Failed;
                exec.result = Some(e);
            }
        }
        let _ = hook_store.update_execution(&exec).await;
    }

    async fn execute_action_inner(
        hook: &Hook,
        event_json: &serde_json::Value,
        issue_svc: &issues::IssueService,
    ) -> Result<String, String> {
        match &hook.action {
            HookAction::SendMessage { target: MessageTarget::Issue(id), body } => {
                let rendered = render_template(body, event_json);
                issue_svc
                    .add_comment(id.clone(), "ns2-hook".to_string(), rendered)
                    .await
                    .map(|_| "comment added".to_string())
                    .map_err(|e| e.to_string())
            }
            HookAction::SendMessage { target: MessageTarget::Session(_), .. } => {
                Ok("session messaging not yet implemented".to_string())
            }
            HookAction::CreateIssue { .. } => {
                Ok("create_issue action not yet implemented".to_string())
            }
            HookAction::RunShell { .. } => {
                Ok("run_shell action not yet implemented".to_string())
            }
        }
    }
}

// ── DB store ──────────────────────────────────────────────────────────────────

pub mod store {
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use sqlx::Row;
    use sqlx::SqlitePool;
    use super::{Hook, HookAction, HookExecution, HookFilter, HookSource, ExecutionStatus};

    #[derive(Debug, thiserror::Error)]
    pub enum StoreError {
        #[error("not found")]
        NotFound,
        #[error("db error: {0}")]
        Sqlx(#[from] sqlx::Error),
        #[error("parse error: {0}")]
        Parse(String),
    }

    pub type StoreResult<T> = Result<T, StoreError>;

    #[async_trait]
    pub trait HookStore: Send + Sync {
        async fn create_hook(&self, hook: &Hook) -> StoreResult<()>;
        async fn list_hooks(
            &self,
            enabled: Option<bool>,
            source_type: Option<&str>,
        ) -> StoreResult<Vec<Hook>>;
        async fn get_hook(&self, id: &str) -> StoreResult<Hook>;
        async fn update_hook(&self, hook: &Hook) -> StoreResult<()>;
        async fn delete_hook(&self, id: &str) -> StoreResult<()>;
        async fn create_execution(&self, exec: &HookExecution) -> StoreResult<()>;
        async fn update_execution(&self, exec: &HookExecution) -> StoreResult<()>;
        async fn list_executions(
            &self,
            hook_id: &str,
            limit: usize,
        ) -> StoreResult<Vec<HookExecution>>;
    }

    pub struct SqliteHookStore {
        pool: SqlitePool,
    }

    impl SqliteHookStore {
        #[must_use] 
        pub const fn new(pool: SqlitePool) -> Self {
            Self { pool }
        }

        #[must_use] 
        pub const fn pool(&self) -> &SqlitePool {
            &self.pool
        }
    }

    const fn source_type_str(source: &HookSource) -> &'static str {
        match source {
            HookSource::Internal { .. } => "internal",
            HookSource::External { .. } => "external",
            HookSource::Timer { .. } => "timer",
        }
    }

    const fn action_type_str(action: &HookAction) -> &'static str {
        match action {
            HookAction::SendMessage { .. } => "send_message",
            HookAction::CreateIssue { .. } => "create_issue",
            HookAction::RunShell { .. } => "run_shell",
        }
    }

    fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>, StoreError> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| StoreError::Parse(e.to_string()))
    }

    fn parse_hook_row(row: &sqlx::sqlite::SqliteRow) -> StoreResult<Hook> {
        let id: String = row.get("id");
        let name: String = row.get("name");
        let source_json: String = row.get("source");
        let filter_json: Option<String> = row.get("filter");
        let action_json: String = row.get("action");
        let enabled: i64 = row.get("enabled");
        let created_by: Option<String> = row.get("created_by");
        let created_at_str: String = row.get("created_at");
        let updated_at_str: String = row.get("updated_at");

        let source: HookSource = serde_json::from_str(&source_json)
            .map_err(|e| StoreError::Parse(format!("source: {e}")))?;
        let filter: Option<HookFilter> = filter_json
            .as_deref()
            .map(|s| serde_json::from_str(s).map_err(|e| StoreError::Parse(format!("filter: {e}"))))
            .transpose()?;
        let action: HookAction = serde_json::from_str(&action_json)
            .map_err(|e| StoreError::Parse(format!("action: {e}")))?;

        Ok(Hook {
            id,
            name,
            source,
            filter,
            action,
            enabled: enabled != 0,
            created_by,
            created_at: parse_rfc3339(&created_at_str)?,
            updated_at: parse_rfc3339(&updated_at_str)?,
        })
    }

    fn parse_execution_row(row: &sqlx::sqlite::SqliteRow) -> StoreResult<HookExecution> {
        let id: String = row.get("id");
        let hook_id: String = row.get("hook_id");
        let triggered_at_str: String = row.get("triggered_at");
        let event_payload_str: String = row.get("event_payload");
        let status_str: String = row.get("status");
        let result: Option<String> = row.get("result");
        let completed_at_str: Option<String> = row.get("completed_at");

        let event_payload: serde_json::Value = serde_json::from_str(&event_payload_str)
            .map_err(|e| StoreError::Parse(e.to_string()))?;
        let status: ExecutionStatus = status_str.parse()
            .map_err(StoreError::Parse)?;
        let completed_at = completed_at_str
            .as_deref()
            .map(parse_rfc3339)
            .transpose()?;

        Ok(HookExecution {
            id,
            hook_id,
            triggered_at: parse_rfc3339(&triggered_at_str)?,
            event_payload,
            status,
            result,
            completed_at,
        })
    }

    #[async_trait]
    impl HookStore for SqliteHookStore {
        async fn create_hook(&self, hook: &Hook) -> StoreResult<()> {
            let source_json = serde_json::to_string(&hook.source)
                .map_err(|e| StoreError::Parse(e.to_string()))?;
            let filter_json = hook.filter.as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e: serde_json::Error| StoreError::Parse(e.to_string()))?;
            let action_json = serde_json::to_string(&hook.action)
                .map_err(|e| StoreError::Parse(e.to_string()))?;

            sqlx::query(
                "INSERT INTO hooks (id, name, source_type, source, filter, action_type, action, enabled, created_by, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&hook.id)
            .bind(&hook.name)
            .bind(source_type_str(&hook.source))
            .bind(&source_json)
            .bind(&filter_json)
            .bind(action_type_str(&hook.action))
            .bind(&action_json)
            .bind(i64::from(hook.enabled))
            .bind(&hook.created_by)
            .bind(hook.created_at.to_rfc3339())
            .bind(hook.updated_at.to_rfc3339())
            .execute(&self.pool)
            .await?;
            Ok(())
        }

        async fn list_hooks(
            &self,
            enabled: Option<bool>,
            source_type: Option<&str>,
        ) -> StoreResult<Vec<Hook>> {
            let rows = match (enabled, source_type) {
                (None, None) => {
                    sqlx::query(
                        "SELECT id, name, source_type, source, filter, action_type, action, enabled, created_by, created_at, updated_at \
                         FROM hooks ORDER BY created_at ASC"
                    )
                    .fetch_all(&self.pool)
                    .await?
                }
                (Some(en), None) => {
                    sqlx::query(
                        "SELECT id, name, source_type, source, filter, action_type, action, enabled, created_by, created_at, updated_at \
                         FROM hooks WHERE enabled = ? ORDER BY created_at ASC"
                    )
                    .bind(i64::from(en))
                    .fetch_all(&self.pool)
                    .await?
                }
                (None, Some(st)) => {
                    sqlx::query(
                        "SELECT id, name, source_type, source, filter, action_type, action, enabled, created_by, created_at, updated_at \
                         FROM hooks WHERE source_type = ? ORDER BY created_at ASC"
                    )
                    .bind(st)
                    .fetch_all(&self.pool)
                    .await?
                }
                (Some(en), Some(st)) => {
                    sqlx::query(
                        "SELECT id, name, source_type, source, filter, action_type, action, enabled, created_by, created_at, updated_at \
                         FROM hooks WHERE enabled = ? AND source_type = ? ORDER BY created_at ASC"
                    )
                    .bind(i64::from(en))
                    .bind(st)
                    .fetch_all(&self.pool)
                    .await?
                }
            };

            rows.iter().map(parse_hook_row).collect()
        }

        async fn get_hook(&self, id: &str) -> StoreResult<Hook> {
            let row = sqlx::query(
                "SELECT id, name, source_type, source, filter, action_type, action, enabled, created_by, created_at, updated_at \
                 FROM hooks WHERE id = ?"
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
            parse_hook_row(&row)
        }

        async fn update_hook(&self, hook: &Hook) -> StoreResult<()> {
            let source_json = serde_json::to_string(&hook.source)
                .map_err(|e| StoreError::Parse(e.to_string()))?;
            let filter_json = hook.filter.as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e: serde_json::Error| StoreError::Parse(e.to_string()))?;
            let action_json = serde_json::to_string(&hook.action)
                .map_err(|e| StoreError::Parse(e.to_string()))?;

            let affected = sqlx::query(
                "UPDATE hooks SET name = ?, source_type = ?, source = ?, filter = ?, action_type = ?, action = ?, enabled = ?, created_by = ?, updated_at = ? \
                 WHERE id = ?"
            )
            .bind(&hook.name)
            .bind(source_type_str(&hook.source))
            .bind(&source_json)
            .bind(&filter_json)
            .bind(action_type_str(&hook.action))
            .bind(&action_json)
            .bind(i64::from(hook.enabled))
            .bind(&hook.created_by)
            .bind(hook.updated_at.to_rfc3339())
            .bind(&hook.id)
            .execute(&self.pool)
            .await?
            .rows_affected();

            if affected == 0 {
                return Err(StoreError::NotFound);
            }
            Ok(())
        }

        async fn delete_hook(&self, id: &str) -> StoreResult<()> {
            let affected = sqlx::query("DELETE FROM hooks WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected();
            if affected == 0 {
                return Err(StoreError::NotFound);
            }
            Ok(())
        }

        async fn create_execution(&self, exec: &HookExecution) -> StoreResult<()> {
            let event_payload_str = serde_json::to_string(&exec.event_payload)
                .map_err(|e| StoreError::Parse(e.to_string()))?;
            sqlx::query(
                "INSERT INTO hook_executions (id, hook_id, triggered_at, event_payload, status, result, completed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&exec.id)
            .bind(&exec.hook_id)
            .bind(exec.triggered_at.to_rfc3339())
            .bind(&event_payload_str)
            .bind(exec.status.to_string())
            .bind(&exec.result)
            .bind(exec.completed_at.map(|dt| dt.to_rfc3339()))
            .execute(&self.pool)
            .await?;
            Ok(())
        }

        async fn update_execution(&self, exec: &HookExecution) -> StoreResult<()> {
            let event_payload_str = serde_json::to_string(&exec.event_payload)
                .map_err(|e| StoreError::Parse(e.to_string()))?;
            sqlx::query(
                "UPDATE hook_executions SET status = ?, result = ?, completed_at = ?, event_payload = ? WHERE id = ?"
            )
            .bind(exec.status.to_string())
            .bind(&exec.result)
            .bind(exec.completed_at.map(|dt| dt.to_rfc3339()))
            .bind(&event_payload_str)
            .bind(&exec.id)
            .execute(&self.pool)
            .await?;
            Ok(())
        }

        async fn list_executions(
            &self,
            hook_id: &str,
            limit: usize,
        ) -> StoreResult<Vec<HookExecution>> {
            let rows = sqlx::query(
                "SELECT id, hook_id, triggered_at, event_payload, status, result, completed_at \
                 FROM hook_executions WHERE hook_id = ? ORDER BY triggered_at DESC LIMIT ?"
            )
            .bind(hook_id)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(&self.pool)
            .await?;
            rows.iter().map(parse_execution_row).collect()
        }
    }
}

// ── Hook ID generation ────────────────────────────────────────────────────────

#[must_use] 
pub fn generate_hook_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let id = uuid::Uuid::new_v4();
    let bytes = id.as_bytes();
    (0..4)
        .map(|i| ALPHABET[(bytes[i] as usize) % ALPHABET.len()] as char)
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use events::{IssueEvent, SessionEvent, SystemEvent};
    use types::{Issue, IssueStatus, IssueComment};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_issue() -> Issue {
        Issue {
            id: "ab12".into(),
            title: "Test".into(),
            body: "Body".into(),
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

    fn make_internal_hook(event_types: Vec<&str>, filter: Option<HookFilter>) -> Hook {
        Hook {
            id: "h001".into(),
            name: "test hook".into(),
            source: HookSource::Internal {
                event_types: event_types.into_iter().map(String::from).collect(),
            },
            filter,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("watcher".into()),
                body: "Issue {{ event.data.issue.id }} changed".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ── evaluate::matches_event tests ─────────────────────────────────────────

    #[test]
    fn matches_issue_created_event_type() {
        let hook = make_internal_hook(vec!["issue.created"], None);
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn does_not_match_wrong_event_type() {
        let hook = make_internal_hook(vec!["issue.created"], None);
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue: make_issue(),
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        });
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_issue_status_changed() {
        let hook = make_internal_hook(vec!["issue.status_changed"], None);
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue: make_issue(),
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_issue_comment_added() {
        let hook = make_internal_hook(vec!["issue.comment_added"], None);
        let comment = IssueComment { author: "user".into(), created_at: Utc::now(), body: "hi".into() };
        let event = SystemEvent::Issue(IssueEvent::CommentAdded { issue: make_issue(), comment });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_done() {
        let hook = make_internal_hook(vec!["session.done"], None);
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_turn_done() {
        let hook = make_internal_hook(vec!["session.turn_done"], None);
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::TurnDone { turn_id: Uuid::new_v4() },
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_error() {
        let hook = make_internal_hook(vec!["session.error"], None);
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Error { message: "oops".into() },
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_turn_started() {
        use types::Turn;
        let hook = make_internal_hook(vec!["session.turn_started"], None);
        let turn = Turn {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            token_count: None,
            created_at: Utc::now(),
        };
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::TurnStarted { turn },
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn wildcard_matches_any_event() {
        let hook = make_internal_hook(vec!["*"], None);
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(evaluate::matches_event(&hook, &event));

        let event2 = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };
        assert!(evaluate::matches_event(&hook, &event2));
    }

    #[test]
    fn external_hook_does_not_match_live_events() {
        let hook = Hook {
            id: "ext1".into(),
            name: "external".into(),
            source: HookSource::External { secret: None },
            filter: None,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("x".into()),
                body: "hi".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(!evaluate::matches_event(&hook, &event));
    }

    // ── FieldCondition evaluation tests ───────────────────────────────────────
    // Note: SystemEvent JSON structure for Issue(StatusChanged):
    // { "type": "issue", "data": { "type": "status_changed", "issue": {...}, "from": ..., "to": ... } }

    #[test]
    fn field_condition_eq_matches() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                // StatusChanged event: data.issue.status reflects current status
                field: "data.issue.status".into(),
                op: Op::Eq,
                value: serde_json::json!("running"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.status_changed"], Some(filter));

        let mut issue = make_issue();
        issue.status = IssueStatus::Running;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_eq_no_match() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.issue.status".into(),
                op: Op::Eq,
                value: serde_json::json!("completed"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.status_changed"], Some(filter));

        let mut issue = make_issue();
        issue.status = IssueStatus::Running;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        });
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_not_eq_matches() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.issue.status".into(),
                op: Op::NotEq,
                value: serde_json::json!("open"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.status_changed"], Some(filter));

        let mut issue = make_issue();
        issue.status = IssueStatus::Running;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_contains_string() {
        // For IssueEvent::Created, JSON is: { "type": "issue", "data": { "type": "created", "id": "ab12", "title": "Test", ... } }
        // The issue fields are directly under "data" (not "data.issue").
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Contains,
                value: serde_json::json!("Test"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue())); // title = "Test"
        assert!(evaluate::matches_event(&hook, &event));
    }

    // ── minijinja template rendering tests ────────────────────────────────────

    #[test]
    fn template_renders_event_issue_id() {
        // Simulate what the event JSON looks like for status_changed
        let event_json = serde_json::json!({
            "type": "issue",
            "data": { "type": "status_changed", "issue": { "id": "ab12", "status": "running" } }
        });
        let rendered = template::render_template(
            "Issue {{ event.data.issue.id }} is now {{ event.data.issue.status }}",
            &event_json,
        );
        assert_eq!(rendered, "Issue ab12 is now running");
    }

    #[test]
    fn template_with_no_vars_returns_literal() {
        let event_json = serde_json::json!({});
        let rendered = template::render_template("Hello, world!", &event_json);
        assert_eq!(rendered, "Hello, world!");
    }

    #[test]
    fn template_missing_var_renders_empty() {
        let event_json = serde_json::json!({});
        // In Chainable mode, undefined nested attribute access returns empty string
        let rendered = template::render_template("{{ event.missing.field }}", &event_json);
        assert!(rendered.is_empty(), "expected empty, got: {rendered:?}");
    }

    // ── Hook ID generation ────────────────────────────────────────────────────

    #[test]
    fn generate_hook_id_is_4_chars() {
        let id = generate_hook_id();
        assert_eq!(id.len(), 4);
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn generate_hook_id_is_unique() {
        let ids: std::collections::HashSet<String> =
            (0..100).map(|_| generate_hook_id()).collect();
        assert!(ids.len() > 90);
    }
}
