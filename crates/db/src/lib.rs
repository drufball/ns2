use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use std::str::FromStr;
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

pub struct SqliteDb {
    pool: SqlitePool,
}

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

impl SqliteDb {
    pub fn from_pool(pool: SqlitePool) -> Self {
        SqliteDb { pool }
    }

    pub async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(url).await?;
        MIGRATOR.run(&pool).await?;
        Ok(SqliteDb { pool })
    }
}

impl Db for SqliteDb {}

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
    let session_id =
        Uuid::parse_str(&session_id_str).map_err(|e| Error::Parse(e.to_string()))?;
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

fn role_to_str(role: &Role) -> &'static str {
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

fn timestamp(dt: &DateTime<Utc>) -> i64 {
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
        let row = sqlx::query("SELECT id, name, status, agent, created_at, updated_at FROM sessions WHERE id = ?")
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
        let affected = sqlx::query(
            "UPDATE sessions SET status = ?, updated_at = ? WHERE id = ?",
        )
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
                let block: ContentBlock = serde_json::from_str(&content_str)
                    .map_err(|e| Error::Parse(e.to_string()))?;
                Ok((role, block))
            })
            .collect()
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
            "INSERT INTO issues (id, title, body, status, assignee, session_id, parent_id, blocked_on, comments, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&issue.id)
        .bind(&issue.title)
        .bind(&issue.body)
        .bind(issue.status.to_string())
        .bind(&issue.assignee)
        .bind(issue.session_id.as_ref().map(|id| id.to_string()))
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
            "SELECT id, title, body, status, assignee, session_id, parent_id, blocked_on, comments, created_at, updated_at FROM issues WHERE id = ?",
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
            "SELECT id, title, body, status, assignee, session_id, parent_id, blocked_on, comments, created_at, updated_at FROM issues ORDER BY created_at DESC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        let issues: Result<Vec<Issue>> = rows.iter().map(parse_issue_row).collect();
        let issues = issues?;

        Ok(issues
            .into_iter()
            .filter(|i| status.as_ref().is_none_or(|s| &i.status == s))
            .filter(|i| assignee.as_ref().is_none_or(|a| i.assignee.as_deref() == Some(a.as_str())))
            .filter(|i| parent_id.as_ref().is_none_or(|p| i.parent_id.as_deref() == Some(p.as_str())))
            .collect())
    }

    async fn list_issues_by_session_id(&self, session_id: Uuid) -> Result<Vec<Issue>> {
        let rows = sqlx::query(
            "SELECT id, title, body, status, assignee, session_id, parent_id, blocked_on, comments, created_at, updated_at FROM issues WHERE session_id = ? ORDER BY created_at DESC, id ASC",
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
            "UPDATE issues SET title = ?, body = ?, status = ?, assignee = ?, session_id = ?, parent_id = ?, blocked_on = ?, comments = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&issue.title)
        .bind(&issue.body)
        .bind(issue.status.to_string())
        .bind(&issue.assignee)
        .bind(issue.session_id.as_ref().map(|id| id.to_string()))
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
        assert_eq!(fetched.created_at.timestamp(), session.created_at.timestamp());
        assert_eq!(fetched.updated_at.timestamp(), session.updated_at.timestamp());
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

        let block = types::ContentBlock::Text { text: "hello world".into() };
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
            &types::ContentBlock::Text { text: "third".into() },
        )
        .await
        .unwrap();
        db.create_content_block(
            turn.id,
            0,
            &types::Role::Assistant,
            &types::ContentBlock::Text { text: "first".into() },
        )
        .await
        .unwrap();
        db.create_content_block(
            turn.id,
            1,
            &types::Role::Assistant,
            &types::ContentBlock::Text { text: "second".into() },
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

        let block = types::ContentBlock::Text { text: "user message".into() };
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
        assert_eq!(turns[0].id, id_first,  "first inserted turn must be at index 0");
        assert_eq!(turns[1].id, id_second, "second inserted turn must be at index 1");
        assert_eq!(turns[2].id, id_third,  "third inserted turn must be at index 2");
    }

    // --- IssueDb tests ---

    fn make_issue(id: &str) -> types::Issue {
        types::Issue {
            id: id.into(),
            title: "Test issue".into(),
            body: "Details".into(),
            status: types::IssueStatus::Open,
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

        let open = db.list_issues(Some(types::IssueStatus::Open), None, None).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "aa11");

        let completed = db.list_issues(Some(types::IssueStatus::Completed), None, None).await.unwrap();
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

        let swe_issues = db.list_issues(None, Some("swe".into()), None).await.unwrap();
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

        let children = db.list_issues(None, None, Some("pp00".into())).await.unwrap();
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
        updated.status = types::IssueStatus::Running;
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
        assert_eq!(fetched.status, types::IssueStatus::Running);
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
}
