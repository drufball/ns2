use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use std::str::FromStr;
use types::{ContentBlock, Role, Session, SessionStatus, Turn};
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

pub trait Db: SessionDb + TurnDb + ContentBlockDb + Send + Sync {}

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

fn block_type_str(block: &ContentBlock) -> &'static str {
    match block {
        ContentBlock::Text { .. } => "text",
        ContentBlock::ToolUse { .. } => "tool_use",
        ContentBlock::ToolResult { .. } => "tool_result",
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
                    "SELECT id, name, status, agent, created_at, updated_at FROM sessions WHERE status = ? ORDER BY created_at DESC",
                )
                .bind(s.to_string())
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, name, status, agent, created_at, updated_at FROM sessions ORDER BY created_at DESC",
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
            "SELECT id, session_id, token_count, created_at FROM turns WHERE session_id = ? ORDER BY created_at ASC",
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
            "INSERT INTO content_blocks (id, turn_id, block_index, role, block_type, content, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(turn_id.to_string())
        .bind(block_index)
        .bind(role_to_str(role))
        .bind(block_type_str(block))
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
        db.update_session_status(session.id, types::SessionStatus::Running)
            .await
            .unwrap();
        let fetched = db.get_session(session.id).await.unwrap();
        assert_eq!(fetched.status, types::SessionStatus::Running);
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
}
