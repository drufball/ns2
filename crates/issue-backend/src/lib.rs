use async_trait::async_trait;
use std::sync::Arc;
use types::{Issue, IssueStatus};

/// Filter parameters for listing issues.
pub struct IssueFilter {
    pub status: Option<IssueStatus>,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
}

/// Pluggable storage back-end for issues.
///
/// All issue CRUD operations (`create`, `get`, `list`, `save`) are routed through
/// this trait so that the `issues` service layer is decoupled from any specific
/// database technology.
#[async_trait]
pub trait IssueBackend: Send + Sync {
    /// Persist a new issue.
    async fn create(&self, issue: &Issue) -> Result<()>;
    /// Retrieve an issue by its id.
    async fn get(&self, id: &str) -> Result<Issue>;
    /// List issues, optionally filtered.
    async fn list(&self, filter: IssueFilter) -> Result<Vec<Issue>>;
    /// Persist changes to an existing issue.
    async fn save(&self, issue: &Issue) -> Result<()>;
    /// Delete an issue.  Not supported by all backends.
    async fn delete(&self, id: &str) -> Result<()>;
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Other(String),
}

// ── SqliteIssueBackend ────────────────────────────────────────────────────────

/// An [`IssueBackend`] that delegates to the `SQLite` [`db::Db`] trait object.
pub struct SqliteIssueBackend {
    db: Arc<dyn db::Db>,
}

impl SqliteIssueBackend {
    /// Wrap an existing `Arc<dyn db::Db>`.
    #[must_use]
    pub fn new(db: Arc<dyn db::Db>) -> Self {
        Self { db }
    }
}

/// Map a `db::Error` to an `issue_backend::Error`.
fn map_db_err(e: db::Error) -> Error {
    match e {
        db::Error::NotFound => Error::NotFound,
        other => Error::Other(other.to_string()),
    }
}

#[async_trait]
impl IssueBackend for SqliteIssueBackend {
    async fn create(&self, issue: &Issue) -> Result<()> {
        self.db.create_issue(issue).await.map_err(map_db_err)
    }

    async fn get(&self, id: &str) -> Result<Issue> {
        self.db
            .get_issue(id.to_string())
            .await
            .map_err(map_db_err)
    }

    async fn list(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        self.db
            .list_issues(filter.status, filter.assignee, filter.parent_id)
            .await
            .map_err(map_db_err)
    }

    async fn save(&self, issue: &Issue) -> Result<()> {
        self.db.update_issue(issue).await.map_err(map_db_err)
    }

    async fn delete(&self, _id: &str) -> Result<()> {
        Err(Error::Other(
            "delete not supported by sqlite backend".into(),
        ))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_issue(id: &str) -> Issue {
        Issue {
            id: id.into(),
            title: "Test issue".into(),
            body: "Details".into(),
            status: IssueStatus::Open,
            branch: String::new(),
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

    // ── SqliteIssueBackend integration tests ──────────────────────────────────

    #[tokio::test]
    async fn sqlite_backend_create_and_get() {
        let (db, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();

        let fetched = backend.get("ab12").await.unwrap();
        assert_eq!(fetched.id, "ab12");
        assert_eq!(fetched.title, "Test issue");
    }

    #[tokio::test]
    async fn sqlite_backend_get_not_found_returns_not_found_error() {
        let (db, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let result = backend.get("xxxx").await;
        assert!(
            matches!(result, Err(Error::NotFound)),
            "expected NotFound, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_backend_list_returns_created_issue() {
        let (db, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();

        let issues = backend
            .list(IssueFilter {
                status: None,
                assignee: None,
                parent_id: None,
            })
            .await
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "ab12");
    }

    #[tokio::test]
    async fn sqlite_backend_save_updates_issue() {
        let (db, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();

        let mut updated = backend.get("ab12").await.unwrap();
        updated.title = "Updated title".into();
        updated.status = IssueStatus::InProgress;
        updated.updated_at = Utc::now();
        backend.save(&updated).await.unwrap();

        let fetched = backend.get("ab12").await.unwrap();
        assert_eq!(fetched.title, "Updated title");
        assert_eq!(fetched.status, IssueStatus::InProgress);
    }

    #[tokio::test]
    async fn sqlite_backend_delete_returns_not_supported_error() {
        let (db, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let result = backend.delete("ab12").await;
        assert!(
            matches!(result, Err(Error::Other(_))),
            "expected Other error for delete, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_backend_list_with_status_filter() {
        let (db, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let open_issue = make_issue("aa11");
        backend.create(&open_issue).await.unwrap();

        let mut completed_issue = make_issue("bb22");
        completed_issue.status = IssueStatus::Completed;
        backend.create(&completed_issue).await.unwrap();

        let open_only = backend
            .list(IssueFilter {
                status: Some(IssueStatus::Open),
                assignee: None,
                parent_id: None,
            })
            .await
            .unwrap();

        assert_eq!(open_only.len(), 1);
        assert_eq!(open_only[0].id, "aa11");
    }
}
