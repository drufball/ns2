use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use std::str::FromStr;
use std::sync::Arc;
use types::{ContentBlock, Issue, IssueComment, IssueStatus, Role, Session, SessionStatus, Turn};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migrate error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("parse error: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait]
pub trait SessionDb {
    async fn create_session(&self, session: &Session) -> Result<()>;
    async fn get_session(&self, id: Uuid) -> Result<Session>;
    async fn list_sessions(&self, status: Option<SessionStatus>) -> Result<Vec<Session>>;
    async fn update_session_status(&self, id: Uuid, status: SessionStatus) -> Result<()>;
}

#[async_trait]
pub trait TurnDb {
    async fn create_turn(&self, turn: &Turn) -> Result<()>;
    async fn list_turns(&self, session_id: Uuid) -> Result<Vec<Turn>>;
}

#[async_trait]
pub trait ContentBlockDb {
    async fn create_content_block(
        &self,
        turn_id: Uuid,
        block_index: i64,
        role: &Role,
        block: &ContentBlock,
    ) -> Result<()>;
    async fn list_content_blocks(&self, turn_id: Uuid) -> Result<Vec<(Role, ContentBlock)>>;
    /// Return the text of the most recently written assistant text block for `session_id`,
    /// or `None` if no text blocks exist yet.
    async fn get_last_text_for_session(&self, session_id: Uuid) -> Result<Option<String>>;
}

#[async_trait]
pub trait IssueDb {
    async fn create_issue(&self, issue: &Issue) -> Result<()>;
    async fn get_issue(&self, id: String) -> Result<Issue>;
    async fn list_issues(
        &self,
        status: Option<IssueStatus>,
        assignee: Option<String>,
        parent_id: Option<String>,
    ) -> Result<Vec<Issue>>;
    async fn list_issues_by_session_id(&self, session_id: Uuid) -> Result<Vec<Issue>>;
    async fn update_issue(&self, issue: &Issue) -> Result<()>;
}

pub trait Db: SessionDb + TurnDb + ContentBlockDb + IssueDb + Send + Sync {}

pub(crate) struct SqliteDb {
    pool: SqlitePool,
}

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

impl SqliteDb {
    /// # Errors
    ///
    /// Returns an error if the database connection or migration fails.
    pub(crate) async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(url).await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }
}

#[cfg(test)]
impl SqliteDb {
    pub(crate) const fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Db for SqliteDb {}

impl SqliteDb {
    #[must_use]
    pub(crate) const fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Connect to the `SQLite` database at `url`, run migrations, and return
/// trait-object handles for [`Db`] and [`HookStore`].
///
/// # Errors
///
/// Returns an error if the database connection or migration fails.
pub async fn connect(url: &str) -> Result<(Arc<dyn Db>, Arc<dyn HookStore>)> {
    let sqlite_db = SqliteDb::connect(url).await?;
    let hook_store: Arc<dyn HookStore> = Arc::new(SqliteHookStore::new(sqlite_db.pool().clone()));
    let db: Arc<dyn Db> = Arc::new(sqlite_db);
    Ok((db, hook_store))
}

fn parse_session_row(row: &sqlx::sqlite::SqliteRow) -> Result<Session> {
    use sqlx::Row;
    let id_str: String = row.get("id");
    let status_str: String = row.get("status");
    let created_at_ts: i64 = row.get("created_at");
    let updated_at_ts: i64 = row.get("updated_at");

    let id = Uuid::parse_str(&id_str).map_err(|e| Error::Parse(e.to_string()))?;
    let status = SessionStatus::from_str(&status_str).map_err(Error::Parse)?;
    let created_at = Utc
        .timestamp_opt(created_at_ts, 0)
        .single()
        .ok_or_else(|| Error::Parse("invalid created_at timestamp".into()))?;
    let updated_at = Utc
        .timestamp_opt(updated_at_ts, 0)
        .single()
        .ok_or_else(|| Error::Parse("invalid updated_at timestamp".into()))?;

    Ok(Session {
        id,
        name: row.get("name"),
        status,
        agent: row.get("agent"),
        created_at,
        updated_at,
    })
}

fn parse_turn_row(row: &sqlx::sqlite::SqliteRow) -> Result<Turn> {
    use sqlx::Row;
    let id_str: String = row.get("id");
    let session_id_str: String = row.get("session_id");
    let created_at_ts: i64 = row.get("created_at");

    let id = Uuid::parse_str(&id_str).map_err(|e| Error::Parse(e.to_string()))?;
    let session_id = Uuid::parse_str(&session_id_str).map_err(|e| Error::Parse(e.to_string()))?;
    let created_at = Utc
        .timestamp_opt(created_at_ts, 0)
        .single()
        .ok_or_else(|| Error::Parse("invalid created_at timestamp".into()))?;

    Ok(Turn {
        id,
        session_id,
        token_count: row.get("token_count"),
        created_at,
    })
}

const fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn role_from_str(s: &str) -> Result<Role> {
    match s {
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        _ => Err(Error::Parse(format!("unknown role: {s}"))),
    }
}

const fn timestamp(dt: &DateTime<Utc>) -> i64 {
    dt.timestamp()
}

#[async_trait]
impl SessionDb for SqliteDb {
    async fn create_session(&self, session: &Session) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (id, name, status, agent, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(&session.name)
        .bind(session.status.to_string())
        .bind(&session.agent)
        .bind(timestamp(&session.created_at))
        .bind(timestamp(&session.updated_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_session(&self, id: Uuid) -> Result<Session> {
        let row = sqlx::query(
            "SELECT id, name, status, agent, created_at, updated_at FROM sessions WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(Error::NotFound)?;
        parse_session_row(&row)
    }

    async fn list_sessions(&self, status: Option<SessionStatus>) -> Result<Vec<Session>> {
        let rows = match status {
            Some(s) => {
                sqlx::query(
                    "SELECT id, name, status, agent, created_at, updated_at FROM sessions WHERE status = ? ORDER BY created_at DESC, id ASC",
                )
                .bind(s.to_string())
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, name, status, agent, created_at, updated_at FROM sessions ORDER BY created_at DESC, id ASC",
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(parse_session_row).collect()
    }

    async fn update_session_status(&self, id: Uuid, status: SessionStatus) -> Result<()> {
        let now = Utc::now().timestamp();
        let affected = sqlx::query("UPDATE sessions SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status.to_string())
            .bind(now)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected();
        if affected == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }
}

#[async_trait]
impl TurnDb for SqliteDb {
    async fn create_turn(&self, turn: &Turn) -> Result<()> {
        sqlx::query(
            "INSERT INTO turns (id, session_id, token_count, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(turn.id.to_string())
        .bind(turn.session_id.to_string())
        .bind(turn.token_count)
        .bind(timestamp(&turn.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_turns(&self, session_id: Uuid) -> Result<Vec<Turn>> {
        let rows = sqlx::query(
            "SELECT id, session_id, token_count, created_at FROM turns WHERE session_id = ? ORDER BY rowid ASC",
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(parse_turn_row).collect()
    }
}

#[async_trait]
impl ContentBlockDb for SqliteDb {
    async fn create_content_block(
        &self,
        turn_id: Uuid,
        block_index: i64,
        role: &Role,
        block: &ContentBlock,
    ) -> Result<()> {
        let content = serde_json::to_string(block).map_err(|e| Error::Parse(e.to_string()))?;
        let id = Uuid::new_v4();
        let now = Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO content_blocks (id, turn_id, block_index, role, content, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(turn_id.to_string())
        .bind(block_index)
        .bind(role_to_str(role))
        .bind(&content)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_content_blocks(&self, turn_id: Uuid) -> Result<Vec<(Role, ContentBlock)>> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT role, content FROM content_blocks WHERE turn_id = ? ORDER BY block_index ASC",
        )
        .bind(turn_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let role_str: String = row.get("role");
                let content_str: String = row.get("content");
                let role = role_from_str(&role_str)?;
                let block: ContentBlock =
                    serde_json::from_str(&content_str).map_err(|e| Error::Parse(e.to_string()))?;
                Ok((role, block))
            })
            .collect()
    }

    async fn get_last_text_for_session(&self, session_id: Uuid) -> Result<Option<String>> {
        use sqlx::Row;
        // Find the most recently inserted assistant TEXT block across all turns for this session.
        // Filter by json_extract to only consider text-type content blocks.
        let row = sqlx::query(
            "SELECT cb.content FROM content_blocks cb \
             JOIN turns t ON cb.turn_id = t.id \
             WHERE t.session_id = ? AND cb.role = 'assistant' \
               AND json_extract(cb.content, '$.type') = 'text' \
             ORDER BY cb.rowid DESC \
             LIMIT 1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some(r) => {
                let content_str: String = r.get("content");
                let block: ContentBlock =
                    serde_json::from_str(&content_str).map_err(|e| Error::Parse(e.to_string()))?;
                match block {
                    ContentBlock::Text { text } => Ok(Some(text)),
                    _ => Ok(None),
                }
            }
        }
    }
}

fn parse_issue_row(row: &sqlx::sqlite::SqliteRow) -> Result<Issue> {
    use sqlx::Row;
    let created_at_ts: i64 = row.get("created_at");
    let updated_at_ts: i64 = row.get("updated_at");
    let status_str: String = row.get("status");
    let blocked_on_str: String = row.get("blocked_on");
    let comments_str: String = row.get("comments");
    let session_id_str: Option<String> = row.get("session_id");

    let status = IssueStatus::from_str(&status_str).map_err(Error::Parse)?;
    let created_at = Utc
        .timestamp_opt(created_at_ts, 0)
        .single()
        .ok_or_else(|| Error::Parse("invalid created_at".into()))?;
    let updated_at = Utc
        .timestamp_opt(updated_at_ts, 0)
        .single()
        .ok_or_else(|| Error::Parse("invalid updated_at".into()))?;
    let blocked_on: Vec<String> =
        serde_json::from_str(&blocked_on_str).map_err(|e| Error::Parse(e.to_string()))?;
    let comments: Vec<IssueComment> =
        serde_json::from_str(&comments_str).map_err(|e| Error::Parse(e.to_string()))?;
    let session_id = session_id_str
        .as_deref()
        .map(|s| Uuid::parse_str(s).map_err(|e| Error::Parse(e.to_string())))
        .transpose()?;

    Ok(Issue {
        id: row.get("id"),
        title: row.get("title"),
        body: row.get("body"),
        status,
        branch: row.get("branch"),
        assignee: row.get("assignee"),
        session_id,
        parent_id: row.get("parent_id"),
        blocked_on,
        comments,
        created_at,
        updated_at,
    })
}

#[async_trait]
impl IssueDb for SqliteDb {
    async fn create_issue(&self, issue: &Issue) -> Result<()> {
        let blocked_on =
            serde_json::to_string(&issue.blocked_on).map_err(|e| Error::Parse(e.to_string()))?;
        let comments =
            serde_json::to_string(&issue.comments).map_err(|e| Error::Parse(e.to_string()))?;
        sqlx::query(
            "INSERT INTO issues (id, title, body, status, branch, assignee, session_id, parent_id, blocked_on, comments, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&issue.id)
        .bind(&issue.title)
        .bind(&issue.body)
        .bind(issue.status.to_string())
        .bind(&issue.branch)
        .bind(&issue.assignee)
        .bind(issue.session_id.as_ref().map(std::string::ToString::to_string))
        .bind(&issue.parent_id)
        .bind(&blocked_on)
        .bind(&comments)
        .bind(timestamp(&issue.created_at))
        .bind(timestamp(&issue.updated_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_issue(&self, id: String) -> Result<Issue> {
        let row = sqlx::query(
            "SELECT id, title, body, status, branch, assignee, session_id, parent_id, blocked_on, comments, created_at, updated_at FROM issues WHERE id = ?",
        )
        .bind(&id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(Error::NotFound)?;
        parse_issue_row(&row)
    }

    async fn list_issues(
        &self,
        status: Option<IssueStatus>,
        assignee: Option<String>,
        parent_id: Option<String>,
    ) -> Result<Vec<Issue>> {
        let rows = sqlx::query(
            "SELECT id, title, body, status, branch, assignee, session_id, parent_id, blocked_on, comments, created_at, updated_at FROM issues ORDER BY created_at DESC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        let issues: Result<Vec<Issue>> = rows.iter().map(parse_issue_row).collect();
        let issues = issues?;

        Ok(issues
            .into_iter()
            .filter(|i| status.as_ref().is_none_or(|s| &i.status == s))
            .filter(|i| {
                assignee
                    .as_ref()
                    .is_none_or(|a| i.assignee.as_deref() == Some(a.as_str()))
            })
            .filter(|i| {
                parent_id
                    .as_ref()
                    .is_none_or(|p| i.parent_id.as_deref() == Some(p.as_str()))
            })
            .collect())
    }

    async fn list_issues_by_session_id(&self, session_id: Uuid) -> Result<Vec<Issue>> {
        let rows = sqlx::query(
            "SELECT id, title, body, status, branch, assignee, session_id, parent_id, blocked_on, comments, created_at, updated_at FROM issues WHERE session_id = ? ORDER BY created_at DESC, id ASC",
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(parse_issue_row).collect()
    }

    async fn update_issue(&self, issue: &Issue) -> Result<()> {
        let blocked_on =
            serde_json::to_string(&issue.blocked_on).map_err(|e| Error::Parse(e.to_string()))?;
        let comments =
            serde_json::to_string(&issue.comments).map_err(|e| Error::Parse(e.to_string()))?;
        let affected = sqlx::query(
            "UPDATE issues SET title = ?, body = ?, status = ?, branch = ?, assignee = ?, session_id = ?, parent_id = ?, blocked_on = ?, comments = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&issue.title)
        .bind(&issue.body)
        .bind(issue.status.to_string())
        .bind(&issue.branch)
        .bind(&issue.assignee)
        .bind(issue.session_id.as_ref().map(std::string::ToString::to_string))
        .bind(&issue.parent_id)
        .bind(&blocked_on)
        .bind(&comments)
        .bind(timestamp(&issue.updated_at))
        .bind(&issue.id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }
}

// ── HookStore ─────────────────────────────────────────────────────────────────

use types::{ExecutionStatus, Hook, HookAction, HookExecution, HookFilter, HookSource};

#[async_trait]
pub trait HookStore: Send + Sync {
    async fn create_hook(&self, hook: &Hook) -> Result<()>;
    async fn list_hooks(
        &self,
        enabled: Option<bool>,
        source_type: Option<&str>,
    ) -> Result<Vec<Hook>>;
    async fn get_hook(&self, id: &str) -> Result<Hook>;
    async fn update_hook(&self, hook: &Hook) -> Result<()>;
    async fn delete_hook(&self, id: &str) -> Result<()>;
    async fn create_execution(&self, exec: &HookExecution) -> Result<()>;
    async fn update_execution(&self, exec: &HookExecution) -> Result<()>;
    async fn list_executions(&self, hook_id: &str, limit: usize) -> Result<Vec<HookExecution>>;
}

pub(crate) struct SqliteHookStore {
    pool: SqlitePool,
}

impl SqliteHookStore {
    #[must_use]
    pub(crate) const fn new(pool: SqlitePool) -> Self {
        Self { pool }
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

fn parse_rfc3339_hook(s: &str) -> Result<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Error::Parse(e.to_string()))
}

fn parse_hook_row(row: &sqlx::sqlite::SqliteRow) -> Result<Hook> {
    use sqlx::Row;
    let id: String = row.get("id");
    let name: String = row.get("name");
    let source_json: String = row.get("source");
    let filter_json: Option<String> = row.get("filter");
    let action_json: String = row.get("action");
    let enabled: i64 = row.get("enabled");
    let created_by: Option<String> = row.get("created_by");
    let created_at_str: String = row.get("created_at");
    let updated_at_str: String = row.get("updated_at");

    let source: HookSource =
        serde_json::from_str(&source_json).map_err(|e| Error::Parse(format!("source: {e}")))?;
    let filter: Option<HookFilter> = filter_json
        .as_deref()
        .map(|s| serde_json::from_str(s).map_err(|e| Error::Parse(format!("filter: {e}"))))
        .transpose()?;
    let action: HookAction =
        serde_json::from_str(&action_json).map_err(|e| Error::Parse(format!("action: {e}")))?;

    Ok(Hook {
        id,
        name,
        source,
        filter,
        action,
        enabled: enabled != 0,
        created_by,
        created_at: parse_rfc3339_hook(&created_at_str)?,
        updated_at: parse_rfc3339_hook(&updated_at_str)?,
    })
}

fn parse_execution_row(row: &sqlx::sqlite::SqliteRow) -> Result<HookExecution> {
    use sqlx::Row;
    let id: String = row.get("id");
    let hook_id: String = row.get("hook_id");
    let triggered_at_str: String = row.get("triggered_at");
    let event_payload_str: String = row.get("event_payload");
    let status_str: String = row.get("status");
    let result: Option<String> = row.get("result");
    let completed_at_str: Option<String> = row.get("completed_at");

    let event_payload: serde_json::Value =
        serde_json::from_str(&event_payload_str).map_err(|e| Error::Parse(e.to_string()))?;
    let status: ExecutionStatus = status_str.parse().map_err(Error::Parse)?;
    let completed_at = completed_at_str
        .as_deref()
        .map(parse_rfc3339_hook)
        .transpose()?;

    Ok(HookExecution {
        id,
        hook_id,
        triggered_at: parse_rfc3339_hook(&triggered_at_str)?,
        event_payload,
        status,
        result,
        completed_at,
    })
}

#[async_trait]
impl HookStore for SqliteHookStore {
    async fn create_hook(&self, hook: &Hook) -> Result<()> {
        let source_json =
            serde_json::to_string(&hook.source).map_err(|e| Error::Parse(e.to_string()))?;
        let filter_json = hook
            .filter
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e: serde_json::Error| Error::Parse(e.to_string()))?;
        let action_json =
            serde_json::to_string(&hook.action).map_err(|e| Error::Parse(e.to_string()))?;

        sqlx::query(
            "INSERT INTO hooks (id, name, source_type, source, filter, action_type, action, \
             enabled, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    ) -> Result<Vec<Hook>> {
        let rows =
            match (enabled, source_type) {
                (None, None) => sqlx::query(
                    "SELECT id, name, source_type, source, filter, action_type, action, enabled, \
                     created_by, created_at, updated_at FROM hooks ORDER BY created_at ASC",
                )
                .fetch_all(&self.pool)
                .await?,
                (Some(en), None) => sqlx::query(
                    "SELECT id, name, source_type, source, filter, action_type, action, enabled, \
                     created_by, created_at, updated_at FROM hooks WHERE enabled = ? \
                     ORDER BY created_at ASC",
                )
                .bind(i64::from(en))
                .fetch_all(&self.pool)
                .await?,
                (None, Some(st)) => sqlx::query(
                    "SELECT id, name, source_type, source, filter, action_type, action, enabled, \
                     created_by, created_at, updated_at FROM hooks WHERE source_type = ? \
                     ORDER BY created_at ASC",
                )
                .bind(st)
                .fetch_all(&self.pool)
                .await?,
                (Some(en), Some(st)) => sqlx::query(
                    "SELECT id, name, source_type, source, filter, action_type, action, enabled, \
                     created_by, created_at, updated_at FROM hooks \
                     WHERE enabled = ? AND source_type = ? ORDER BY created_at ASC",
                )
                .bind(i64::from(en))
                .bind(st)
                .fetch_all(&self.pool)
                .await?,
            };
        rows.iter().map(parse_hook_row).collect()
    }

    async fn get_hook(&self, id: &str) -> Result<Hook> {
        let row = sqlx::query(
            "SELECT id, name, source_type, source, filter, action_type, action, enabled, \
             created_by, created_at, updated_at FROM hooks WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(Error::NotFound)?;
        parse_hook_row(&row)
    }

    async fn update_hook(&self, hook: &Hook) -> Result<()> {
        let source_json =
            serde_json::to_string(&hook.source).map_err(|e| Error::Parse(e.to_string()))?;
        let filter_json = hook
            .filter
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e: serde_json::Error| Error::Parse(e.to_string()))?;
        let action_json =
            serde_json::to_string(&hook.action).map_err(|e| Error::Parse(e.to_string()))?;

        let affected = sqlx::query(
            "UPDATE hooks SET name = ?, source_type = ?, source = ?, filter = ?, \
             action_type = ?, action = ?, enabled = ?, created_by = ?, updated_at = ? \
             WHERE id = ?",
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
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn delete_hook(&self, id: &str) -> Result<()> {
        let affected = sqlx::query("DELETE FROM hooks WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if affected == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn create_execution(&self, exec: &HookExecution) -> Result<()> {
        let event_payload_str =
            serde_json::to_string(&exec.event_payload).map_err(|e| Error::Parse(e.to_string()))?;
        sqlx::query(
            "INSERT INTO hook_executions (id, hook_id, triggered_at, event_payload, \
             status, result, completed_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
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

    async fn update_execution(&self, exec: &HookExecution) -> Result<()> {
        let event_payload_str =
            serde_json::to_string(&exec.event_payload).map_err(|e| Error::Parse(e.to_string()))?;
        sqlx::query(
            "UPDATE hook_executions SET status = ?, result = ?, completed_at = ?, \
             event_payload = ? WHERE id = ?",
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

    async fn list_executions(&self, hook_id: &str, limit: usize) -> Result<Vec<HookExecution>> {
        let rows = sqlx::query(
            "SELECT id, hook_id, triggered_at, event_payload, status, result, completed_at \
             FROM hook_executions WHERE hook_id = ? ORDER BY triggered_at DESC LIMIT ?",
        )
        .bind(hook_id)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(parse_execution_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_create_and_get_session(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Created,
            agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        db.create_session(&session).await.unwrap();
        let fetched = db.get_session(session.id).await.unwrap();
        assert_eq!(fetched.id, session.id);
        assert_eq!(fetched.name, "test");
        assert_eq!(fetched.status, types::SessionStatus::Created);
        assert!(fetched.agent.is_none());
        assert_eq!(
            fetched.created_at.timestamp(),
            session.created_at.timestamp()
        );
        assert_eq!(
            fetched.updated_at.timestamp(),
            session.updated_at.timestamp()
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_sessions(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);

        let s1 = types::Session {
            id: Uuid::new_v4(),
            name: "session-one".into(),
            status: types::SessionStatus::Created,
            agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let s2 = types::Session {
            id: Uuid::new_v4(),
            name: "session-two".into(),
            status: types::SessionStatus::Running,
            agent: Some("gpt".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        db.create_session(&s1).await.unwrap();
        db.create_session(&s2).await.unwrap();

        let all = db.list_sessions(None).await.unwrap();
        assert_eq!(all.len(), 2);

        let created_only = db
            .list_sessions(Some(types::SessionStatus::Created))
            .await
            .unwrap();
        assert_eq!(created_only.len(), 1);
        assert_eq!(created_only[0].name, "session-one");

        let running_only = db
            .list_sessions(Some(types::SessionStatus::Running))
            .await
            .unwrap();
        assert_eq!(running_only.len(), 1);
        assert_eq!(running_only[0].name, "session-two");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_update_session_status(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = types::Session {
            id: Uuid::new_v4(),
            name: "updatable".into(),
            status: types::SessionStatus::Created,
            agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        db.create_session(&session).await.unwrap();
        let before_update = Utc::now();
        db.update_session_status(session.id, types::SessionStatus::Running)
            .await
            .unwrap();
        let fetched = db.get_session(session.id).await.unwrap();
        let after_update = Utc::now();
        assert_eq!(fetched.status, types::SessionStatus::Running);
        assert!(fetched.updated_at.timestamp() >= before_update.timestamp());
        assert!(fetched.updated_at.timestamp() <= after_update.timestamp());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_session_not_found(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let result = db.get_session(Uuid::new_v4()).await;
        assert!(matches!(result, Err(Error::NotFound)));
    }

    // Helper to create a minimal session in the db
    async fn insert_session(db: &SqliteDb) -> types::Session {
        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test-session".into(),
            status: types::SessionStatus::Created,
            agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        db.create_session(&session).await.unwrap();
        session
    }

    // Helper to create a turn under a given session
    async fn insert_turn(db: &SqliteDb, session_id: Uuid) -> types::Turn {
        let turn = types::Turn {
            id: Uuid::new_v4(),
            session_id,
            token_count: Some(5),
            created_at: Utc::now(),
        };
        db.create_turn(&turn).await.unwrap();
        turn
    }

    // --- TurnDb tests ---

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_create_and_list_turns(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn = insert_turn(&db, session.id).await;

        let turns = db.list_turns(session.id).await.unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].id, turn.id);
        assert_eq!(turns[0].session_id, session.id);
        assert_eq!(turns[0].token_count, Some(5));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_turns_empty(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turns = db.list_turns(session.id).await.unwrap();
        assert!(turns.is_empty());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_turns_multiple(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;

        // Insert two turns
        insert_turn(&db, session.id).await;
        insert_turn(&db, session.id).await;

        let turns = db.list_turns(session.id).await.unwrap();
        assert_eq!(turns.len(), 2);
        // All should belong to the same session
        for t in &turns {
            assert_eq!(t.session_id, session.id);
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_turns_only_for_session(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session_a = insert_session(&db).await;
        let session_b = insert_session(&db).await;

        insert_turn(&db, session_a.id).await;
        insert_turn(&db, session_b.id).await;

        let turns_a = db.list_turns(session_a.id).await.unwrap();
        assert_eq!(turns_a.len(), 1);
        assert_eq!(turns_a[0].session_id, session_a.id);
    }

    // --- ContentBlockDb tests ---

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_create_and_list_content_blocks(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn = insert_turn(&db, session.id).await;

        let block = types::ContentBlock::Text {
            text: "hello world".into(),
        };
        db.create_content_block(turn.id, 0, &types::Role::Assistant, &block)
            .await
            .unwrap();

        let blocks = db.list_content_blocks(turn.id).await.unwrap();
        assert_eq!(blocks.len(), 1);
        let (role, retrieved) = &blocks[0];
        assert_eq!(*role, types::Role::Assistant);
        assert!(matches!(retrieved, types::ContentBlock::Text { text } if text == "hello world"));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_content_blocks_order_by_index(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn = insert_turn(&db, session.id).await;

        // Insert in reverse order to verify ordering
        db.create_content_block(
            turn.id,
            2,
            &types::Role::Assistant,
            &types::ContentBlock::Text {
                text: "third".into(),
            },
        )
        .await
        .unwrap();
        db.create_content_block(
            turn.id,
            0,
            &types::Role::Assistant,
            &types::ContentBlock::Text {
                text: "first".into(),
            },
        )
        .await
        .unwrap();
        db.create_content_block(
            turn.id,
            1,
            &types::Role::Assistant,
            &types::ContentBlock::Text {
                text: "second".into(),
            },
        )
        .await
        .unwrap();

        let blocks = db.list_content_blocks(turn.id).await.unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0].1, types::ContentBlock::Text { text } if text == "first"));
        assert!(matches!(&blocks[1].1, types::ContentBlock::Text { text } if text == "second"));
        assert!(matches!(&blocks[2].1, types::ContentBlock::Text { text } if text == "third"));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_content_blocks_user_role(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn = insert_turn(&db, session.id).await;

        let block = types::ContentBlock::Text {
            text: "user message".into(),
        };
        db.create_content_block(turn.id, 0, &types::Role::User, &block)
            .await
            .unwrap();

        let blocks = db.list_content_blocks(turn.id).await.unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, types::Role::User);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_content_blocks_tool_use(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn = insert_turn(&db, session.id).await;

        let block = types::ContentBlock::ToolUse {
            id: "tool-abc".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        };
        db.create_content_block(turn.id, 0, &types::Role::Assistant, &block)
            .await
            .unwrap();

        let blocks = db.list_content_blocks(turn.id).await.unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(&blocks[0].1, types::ContentBlock::ToolUse { id, .. } if id == "tool-abc")
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_content_blocks_empty(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn = insert_turn(&db, session.id).await;

        let blocks = db.list_content_blocks(turn.id).await.unwrap();
        assert!(blocks.is_empty());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_update_session_status_not_found(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let result = db
            .update_session_status(Uuid::new_v4(), types::SessionStatus::Completed)
            .await;
        assert!(matches!(result, Err(Error::NotFound)));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_turns_insertion_order_within_same_second(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;

        // Fixed timestamp so all three turns share the exact same created_at second.
        let same_ts = Utc.timestamp_opt(1_700_000_000, 0).unwrap();

        // UUIDs chosen so that lexicographic (id ASC) order is the REVERSE of insertion order.
        // Insertion order: ff... → 88... → 00...
        // Old ORDER BY id ASC would return: 00... → 88... → ff... (wrong)
        // Correct ORDER BY rowid ASC returns: ff... → 88... → 00... (insertion order)
        let id_first = Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();
        let id_second = Uuid::parse_str("88888888-8888-8888-8888-888888888888").unwrap();
        let id_third = Uuid::parse_str("00000000-0000-0000-0000-000000000000").unwrap();

        for id in [id_first, id_second, id_third] {
            let turn = types::Turn {
                id,
                session_id: session.id,
                token_count: None,
                created_at: same_ts,
            };
            db.create_turn(&turn).await.unwrap();
        }

        let turns = db.list_turns(session.id).await.unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(
            turns[0].id, id_first,
            "first inserted turn must be at index 0"
        );
        assert_eq!(
            turns[1].id, id_second,
            "second inserted turn must be at index 1"
        );
        assert_eq!(
            turns[2].id, id_third,
            "third inserted turn must be at index 2"
        );
    }

    // --- IssueDb tests ---

    fn make_issue(id: &str) -> types::Issue {
        types::Issue {
            id: id.into(),
            title: "Test issue".into(),
            body: "Details".into(),
            status: types::IssueStatus::Open,
            branch: String::new(),
            assignee: None,
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_create_and_get_issue(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let issue = make_issue("ab12");
        db.create_issue(&issue).await.unwrap();
        let fetched = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(fetched.id, "ab12");
        assert_eq!(fetched.title, "Test issue");
        assert_eq!(fetched.body, "Details");
        assert_eq!(fetched.status, types::IssueStatus::Open);
        assert!(fetched.assignee.is_none());
        assert!(fetched.session_id.is_none());
        assert!(fetched.parent_id.is_none());
        assert!(fetched.blocked_on.is_empty());
        assert!(fetched.comments.is_empty());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_issue_not_found(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let result = db.get_issue("xxxx".into()).await;
        assert!(matches!(result, Err(Error::NotFound)));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_issues_empty(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let issues = db.list_issues(None, None, None).await.unwrap();
        assert!(issues.is_empty());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_issues_filter_by_status(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let mut i1 = make_issue("aa11");
        i1.status = types::IssueStatus::Open;
        let mut i2 = make_issue("bb22");
        i2.status = types::IssueStatus::Completed;
        db.create_issue(&i1).await.unwrap();
        db.create_issue(&i2).await.unwrap();

        let open = db
            .list_issues(Some(types::IssueStatus::Open), None, None)
            .await
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "aa11");

        let completed = db
            .list_issues(Some(types::IssueStatus::Completed), None, None)
            .await
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, "bb22");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_issues_filter_by_assignee(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let mut i1 = make_issue("aa11");
        i1.assignee = Some("swe".into());
        let mut i2 = make_issue("bb22");
        i2.assignee = Some("qa".into());
        db.create_issue(&i1).await.unwrap();
        db.create_issue(&i2).await.unwrap();

        let swe_issues = db
            .list_issues(None, Some("swe".into()), None)
            .await
            .unwrap();
        assert_eq!(swe_issues.len(), 1);
        assert_eq!(swe_issues[0].id, "aa11");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_issues_filter_by_parent(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let parent = make_issue("pp00");
        db.create_issue(&parent).await.unwrap();

        let mut child = make_issue("cc11");
        child.parent_id = Some("pp00".into());
        db.create_issue(&child).await.unwrap();

        let mut other = make_issue("oo22");
        other.parent_id = Some("other".into());
        db.create_issue(&other).await.unwrap();

        let children = db
            .list_issues(None, None, Some("pp00".into()))
            .await
            .unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, "cc11");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_update_issue(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let issue = make_issue("ab12");
        db.create_issue(&issue).await.unwrap();

        let mut updated = db.get_issue("ab12".into()).await.unwrap();
        updated.title = "Updated title".into();
        updated.status = types::IssueStatus::InProgress;
        updated.assignee = Some("swe".into());
        updated.blocked_on = vec!["xy34".into()];
        updated.comments = vec![types::IssueComment {
            author: "user".into(),
            created_at: Utc::now(),
            body: "A comment".into(),
        }];
        updated.updated_at = Utc::now();
        db.update_issue(&updated).await.unwrap();

        let fetched = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(fetched.title, "Updated title");
        assert_eq!(fetched.status, types::IssueStatus::InProgress);
        assert_eq!(fetched.assignee.as_deref(), Some("swe"));
        assert_eq!(fetched.blocked_on, vec!["xy34"]);
        assert_eq!(fetched.comments.len(), 1);
        assert_eq!(fetched.comments[0].author, "user");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_update_issue_not_found(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let issue = make_issue("xxxx");
        let result = db.update_issue(&issue).await;
        assert!(matches!(result, Err(Error::NotFound)));
    }

    // --- get_last_text_for_session tests ---

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_last_text_for_session_empty(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let result = db.get_last_text_for_session(session.id).await.unwrap();
        assert!(result.is_none(), "no text blocks yet, should return None");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_last_text_for_session_returns_text(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn = insert_turn(&db, session.id).await;

        db.create_content_block(
            turn.id,
            0,
            &types::Role::Assistant,
            &types::ContentBlock::Text {
                text: "hello from the agent".into(),
            },
        )
        .await
        .unwrap();

        let result = db.get_last_text_for_session(session.id).await.unwrap();
        assert_eq!(result.as_deref(), Some("hello from the agent"));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_last_text_for_session_returns_latest_text(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn1 = insert_turn(&db, session.id).await;
        let turn2 = insert_turn(&db, session.id).await;

        db.create_content_block(
            turn1.id,
            0,
            &types::Role::Assistant,
            &types::ContentBlock::Text {
                text: "first text".into(),
            },
        )
        .await
        .unwrap();

        db.create_content_block(
            turn2.id,
            0,
            &types::Role::Assistant,
            &types::ContentBlock::Text {
                text: "second text".into(),
            },
        )
        .await
        .unwrap();

        let result = db.get_last_text_for_session(session.id).await.unwrap();
        assert_eq!(
            result.as_deref(),
            Some("second text"),
            "should return the most recent text"
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_last_text_for_session_ignores_other_sessions(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session_a = insert_session(&db).await;
        let session_b = insert_session(&db).await;

        let turn_b = insert_turn(&db, session_b.id).await;
        db.create_content_block(
            turn_b.id,
            0,
            &types::Role::Assistant,
            &types::ContentBlock::Text {
                text: "session b text".into(),
            },
        )
        .await
        .unwrap();

        let result = db.get_last_text_for_session(session_a.id).await.unwrap();
        assert!(
            result.is_none(),
            "should not return text from other sessions"
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_last_text_for_session_ignores_user_messages(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session = insert_session(&db).await;
        let turn = insert_turn(&db, session.id).await;

        db.create_content_block(
            turn.id,
            0,
            &types::Role::User,
            &types::ContentBlock::Text {
                text: "user question".into(),
            },
        )
        .await
        .unwrap();

        let result = db.get_last_text_for_session(session.id).await.unwrap();
        assert!(
            result.is_none(),
            "user messages should not count as last text"
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_issue_with_session_id(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session_id = Uuid::new_v4();
        let mut issue = make_issue("ab12");
        issue.session_id = Some(session_id);
        db.create_issue(&issue).await.unwrap();
        let fetched = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(fetched.session_id, Some(session_id));
    }

    // --- list_issues_by_session_id tests ---

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_issues_by_session_id_finds_linked_issue(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session_id = Uuid::new_v4();

        let mut issue = make_issue("ab12");
        issue.session_id = Some(session_id);
        db.create_issue(&issue).await.unwrap();

        // Another issue with a different session_id — should NOT be returned
        let other_session_id = Uuid::new_v4();
        let mut other = make_issue("cd34");
        other.session_id = Some(other_session_id);
        db.create_issue(&other).await.unwrap();

        let found = db.list_issues_by_session_id(session_id).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ab12");
        assert_eq!(found[0].session_id, Some(session_id));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_issues_by_session_id_returns_empty_when_none(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session_id = Uuid::new_v4();

        // Issue with no session_id
        let issue = make_issue("ab12");
        db.create_issue(&issue).await.unwrap();

        let found = db.list_issues_by_session_id(session_id).await.unwrap();
        assert!(found.is_empty());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_issues_by_session_id_returns_multiple(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let session_id = Uuid::new_v4();

        let mut i1 = make_issue("ab12");
        i1.session_id = Some(session_id);
        let mut i2 = make_issue("cd34");
        i2.session_id = Some(session_id);
        db.create_issue(&i1).await.unwrap();
        db.create_issue(&i2).await.unwrap();

        let found = db.list_issues_by_session_id(session_id).await.unwrap();
        assert_eq!(found.len(), 2);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_issue_branch_round_trip(pool: SqlitePool) {
        let db = SqliteDb::from_pool(pool);
        let mut issue = make_issue("br01");
        issue.branch = "feat/worktree-support".into();
        db.create_issue(&issue).await.unwrap();

        // get_issue preserves branch
        let fetched = db.get_issue("br01".into()).await.unwrap();
        assert_eq!(fetched.branch, "feat/worktree-support");

        // list_issues preserves branch
        let listed = db.list_issues(None, None, None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].branch, "feat/worktree-support");

        // update_issue persists a changed branch
        let mut updated = fetched;
        updated.branch = "fix/updated-branch".into();
        updated.updated_at = Utc::now();
        db.update_issue(&updated).await.unwrap();

        let after_update = db.get_issue("br01".into()).await.unwrap();
        assert_eq!(after_update.branch, "fix/updated-branch");
    }

    // ── SqliteHookStore tests ─────────────────────────────────────────────────

    fn make_hook(id: &str) -> types::Hook {
        types::Hook {
            id: id.into(),
            name: "test-hook".into(),
            source: types::HookSource::Internal {
                event_types: vec!["issue.created".into()],
            },
            filter: None,
            action: types::HookAction::SendMessage {
                target: types::MessageTarget::Issue("x".into()),
                body: "hi".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // test: create_then_list_hooks_returns_correct_source_type
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_create_then_list_hooks_returns_correct_source_type(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);
        let hook = make_hook("hook-001");
        store.create_hook(&hook).await.unwrap();

        let hooks = store.list_hooks(None, None).await.unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].id, "hook-001");
        assert!(
            matches!(
                &hooks[0].source,
                types::HookSource::Internal { event_types }
                    if event_types == &["issue.created"]
            ),
            "source should be Internal with correct event_types"
        );
    }

    // test: source_type_str filters correctly
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_hooks_filter_by_source_type(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        let internal_hook = make_hook("h-internal");
        store.create_hook(&internal_hook).await.unwrap();

        let mut external_hook = make_hook("h-external");
        external_hook.source = types::HookSource::External { secret: None };
        store.create_hook(&external_hook).await.unwrap();

        let mut timer_hook = make_hook("h-timer");
        timer_hook.source = types::HookSource::Timer {
            schedule: "*/5 * * * *".into(),
        };
        store.create_hook(&timer_hook).await.unwrap();

        // Filter by "internal" — should return only the internal hook
        let internal_list = store.list_hooks(None, Some("internal")).await.unwrap();
        assert_eq!(internal_list.len(), 1);
        assert_eq!(internal_list[0].id, "h-internal");

        // Filter by "external"
        let external_list = store.list_hooks(None, Some("external")).await.unwrap();
        assert_eq!(external_list.len(), 1);
        assert_eq!(external_list[0].id, "h-external");

        // Filter by "timer"
        let timer_list = store.list_hooks(None, Some("timer")).await.unwrap();
        assert_eq!(timer_list.len(), 1);
        assert_eq!(timer_list[0].id, "h-timer");
    }

    // test: action_type_str_round_trips_correctly
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_action_type_str_round_trips_correctly(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);
        let hook = make_hook("hook-action");
        store.create_hook(&hook).await.unwrap();

        let fetched = store.get_hook("hook-action").await.unwrap();
        assert!(
            matches!(
                &fetched.action,
                types::HookAction::SendMessage { target, body }
                    if matches!(target, types::MessageTarget::Issue(s) if s == "x")
                    && body == "hi"
            ),
            "action should be SendMessage with correct target and body"
        );
    }

    // test: action_type CreateIssue round-trips
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_action_create_issue_round_trips(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);
        let mut hook = make_hook("hook-ci");
        hook.action = types::HookAction::CreateIssue {
            title: "Auto issue".into(),
            body: "Created by hook".into(),
            assignee: Some("dev".into()),
            parent: None,
            start: false,
        };
        store.create_hook(&hook).await.unwrap();

        let fetched = store.get_hook("hook-ci").await.unwrap();
        assert!(
            matches!(
                &fetched.action,
                types::HookAction::CreateIssue { title, assignee, .. }
                    if title == "Auto issue" && assignee.as_deref() == Some("dev")
            ),
            "action should be CreateIssue with correct fields"
        );
    }

    // test: enabled_flag_round_trips_correctly
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_enabled_flag_round_trips_correctly(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        // Create a hook with enabled: false
        let mut hook = make_hook("hook-disabled");
        hook.enabled = false;
        store.create_hook(&hook).await.unwrap();

        // List hooks and assert enabled == false
        let hooks = store.list_hooks(None, None).await.unwrap();
        assert_eq!(hooks.len(), 1);
        assert!(!hooks[0].enabled, "hook should be disabled after creation");

        // Update the hook to enabled: true
        let mut updated = hooks[0].clone();
        updated.enabled = true;
        updated.updated_at = Utc::now();
        store.update_hook(&updated).await.unwrap();

        // Get the hook and assert enabled == true
        let fetched = store.get_hook("hook-disabled").await.unwrap();
        assert!(fetched.enabled, "hook should be enabled after update");
    }

    // test: list_hooks filter by enabled flag
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_list_hooks_filter_by_enabled(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        let enabled_hook = make_hook("h-enabled");
        store.create_hook(&enabled_hook).await.unwrap();

        let mut disabled_hook = make_hook("h-disabled");
        disabled_hook.enabled = false;
        store.create_hook(&disabled_hook).await.unwrap();

        let enabled_only = store.list_hooks(Some(true), None).await.unwrap();
        assert_eq!(enabled_only.len(), 1);
        assert_eq!(enabled_only[0].id, "h-enabled");
        assert!(enabled_only[0].enabled);

        let disabled_only = store.list_hooks(Some(false), None).await.unwrap();
        assert_eq!(disabled_only.len(), 1);
        assert_eq!(disabled_only[0].id, "h-disabled");
        assert!(!disabled_only[0].enabled);
    }

    // test: update_nonexistent_hook_returns_not_found
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_update_nonexistent_hook_returns_not_found(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);
        let hook = make_hook("no-such-id");
        let result = store.update_hook(&hook).await;
        assert!(
            matches!(result, Err(Error::NotFound)),
            "updating a non-existent hook should return NotFound, got: {result:?}"
        );
    }

    // test: delete_nonexistent_hook_returns_not_found
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_delete_nonexistent_hook_returns_not_found(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);
        let result = store.delete_hook("no-such-id").await;
        assert!(
            matches!(result, Err(Error::NotFound)),
            "deleting a non-existent hook should return NotFound, got: {result:?}"
        );
    }

    // test: delete an existing hook succeeds and it's no longer listable
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_delete_hook_removes_it(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);
        let hook = make_hook("hook-to-delete");
        store.create_hook(&hook).await.unwrap();

        // Confirm it exists
        let before = store.list_hooks(None, None).await.unwrap();
        assert_eq!(before.len(), 1);

        // Delete it
        store.delete_hook("hook-to-delete").await.unwrap();

        // Confirm it's gone
        let after = store.list_hooks(None, None).await.unwrap();
        assert!(after.is_empty());

        // get_hook should return NotFound
        let get_result = store.get_hook("hook-to-delete").await;
        assert!(matches!(get_result, Err(Error::NotFound)));
    }

    // test: action_type_str is stored correctly in the DB for all action types
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_action_type_str_stored_correctly_send_message(pool: SqlitePool) {
        use sqlx::Row;
        let store = SqliteHookStore::new(pool.clone());
        let hook = make_hook("hook-sm");
        // hook already has SendMessage action
        store.create_hook(&hook).await.unwrap();

        // Query raw action_type column to verify the stored string
        let row = sqlx::query("SELECT action_type FROM hooks WHERE id = ?")
            .bind("hook-sm")
            .fetch_one(&pool)
            .await
            .unwrap();
        let action_type: String = row.get("action_type");
        assert_eq!(action_type, "send_message");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_action_type_str_stored_correctly_create_issue(pool: SqlitePool) {
        use sqlx::Row;
        let store = SqliteHookStore::new(pool.clone());
        let mut hook = make_hook("hook-ci2");
        hook.action = types::HookAction::CreateIssue {
            title: "title".into(),
            body: "body".into(),
            assignee: None,
            parent: None,
            start: false,
        };
        store.create_hook(&hook).await.unwrap();

        let row = sqlx::query("SELECT action_type FROM hooks WHERE id = ?")
            .bind("hook-ci2")
            .fetch_one(&pool)
            .await
            .unwrap();
        let action_type: String = row.get("action_type");
        assert_eq!(action_type, "create_issue");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_action_type_str_stored_correctly_run_shell(pool: SqlitePool) {
        use sqlx::Row;
        let store = SqliteHookStore::new(pool.clone());
        let mut hook = make_hook("hook-rs");
        hook.action = types::HookAction::RunShell {
            command: "echo hi".into(),
            timeout_secs: 10,
            blocking: false,
        };
        store.create_hook(&hook).await.unwrap();

        let row = sqlx::query("SELECT action_type FROM hooks WHERE id = ?")
            .bind("hook-rs")
            .fetch_one(&pool)
            .await
            .unwrap();
        let action_type: String = row.get("action_type");
        assert_eq!(action_type, "run_shell");
    }

    // ── HookExecution CRUD SQLite tests ──────────────────────────────────────

    fn make_execution(hook_id: &str) -> types::HookExecution {
        types::HookExecution {
            id: uuid::Uuid::new_v4().to_string(),
            hook_id: hook_id.into(),
            triggered_at: Utc::now(),
            event_payload: serde_json::json!({"type": "test"}),
            status: types::ExecutionStatus::Running,
            result: None,
            completed_at: None,
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_hook_execution_create_and_list(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        // Create a hook first (no FK constraint in schema, but good practice)
        let hook = make_hook("exec-hook-1");
        store.create_hook(&hook).await.unwrap();

        // Create an execution
        let exec = make_execution("exec-hook-1");
        let exec_id = exec.id.clone();
        store.create_execution(&exec).await.unwrap();

        // List executions and assert it's there
        let executions = store.list_executions("exec-hook-1", 10).await.unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].id, exec_id);
        assert_eq!(executions[0].hook_id, "exec-hook-1");
        assert_eq!(executions[0].status, types::ExecutionStatus::Running);
        assert!(executions[0].result.is_none());
        assert!(executions[0].completed_at.is_none());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_hook_execution_update_status(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        let hook = make_hook("exec-hook-2");
        store.create_hook(&hook).await.unwrap();

        // Create execution with status Running
        let mut exec = make_execution("exec-hook-2");
        store.create_execution(&exec).await.unwrap();

        // Update execution with status Completed
        exec.status = types::ExecutionStatus::Completed;
        exec.result = Some("done".into());
        exec.completed_at = Some(Utc::now());
        store.update_execution(&exec).await.unwrap();

        // List executions and assert status is now Completed
        let executions = store.list_executions("exec-hook-2", 10).await.unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].status, types::ExecutionStatus::Completed);
        assert_eq!(executions[0].result.as_deref(), Some("done"));
        assert!(executions[0].completed_at.is_some());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_hook_execution_update_to_failed(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        let hook = make_hook("exec-hook-3");
        store.create_hook(&hook).await.unwrap();

        let mut exec = make_execution("exec-hook-3");
        store.create_execution(&exec).await.unwrap();

        exec.status = types::ExecutionStatus::Failed;
        exec.result = Some("error: something went wrong".into());
        exec.completed_at = Some(Utc::now());
        store.update_execution(&exec).await.unwrap();

        let executions = store.list_executions("exec-hook-3", 10).await.unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].status, types::ExecutionStatus::Failed);
        assert!(executions[0].result.as_deref().unwrap().contains("error"));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_hook_execution_list_default_limit(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        let hook = make_hook("exec-hook-limit");
        store.create_hook(&hook).await.unwrap();

        // Create 25 executions
        for _ in 0..25 {
            let exec = make_execution("exec-hook-limit");
            store.create_execution(&exec).await.unwrap();
        }

        // List with limit=20 — assert 20 returned
        let executions = store.list_executions("exec-hook-limit", 20).await.unwrap();
        assert_eq!(
            executions.len(),
            20,
            "expected 20 executions with limit=20, got {}",
            executions.len()
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_hook_execution_list_empty_when_no_executions(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        let hook = make_hook("exec-hook-empty");
        store.create_hook(&hook).await.unwrap();

        let executions = store.list_executions("exec-hook-empty", 10).await.unwrap();
        assert!(executions.is_empty());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_hook_execution_list_returns_descending_order(pool: SqlitePool) {
        let store = SqliteHookStore::new(pool);

        let hook = make_hook("exec-hook-order");
        store.create_hook(&hook).await.unwrap();

        // Create 3 executions with different triggered_at times
        let base_time = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for i in 0..3_i64 {
            let exec = types::HookExecution {
                id: format!("exec-order-{i}"),
                hook_id: "exec-hook-order".into(),
                triggered_at: base_time + chrono::Duration::seconds(i),
                event_payload: serde_json::json!({}),
                status: types::ExecutionStatus::Running,
                result: None,
                completed_at: None,
            };
            store.create_execution(&exec).await.unwrap();
        }

        let executions = store.list_executions("exec-hook-order", 10).await.unwrap();
        assert_eq!(executions.len(), 3);
        // Should be in descending order (most recent first)
        assert_eq!(executions[0].id, "exec-order-2");
        assert_eq!(executions[1].id, "exec-order-1");
        assert_eq!(executions[2].id, "exec-order-0");
    }
}
