use std::sync::Arc;
use chrono::Utc;
use types::{Issue, IssueComment, IssueStatus, Session, SessionStatus};
use uuid::Uuid;

pub fn slugify(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut result = String::new();
    let mut in_sep = false;
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            result.push(ch);
            in_sep = false;
        } else if !in_sep {
            result.push('-');
            in_sep = true;
        }
    }
    result.trim_matches('-').to_string()
}

pub fn generate_issue_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let id = Uuid::new_v4();
    let bytes = id.as_bytes();
    (0..4)
        .map(|i| ALPHABET[(bytes[i] as usize) % ALPHABET.len()] as char)
        .collect()
}

pub struct CreateIssueInput {
    pub title: String,
    pub body: String,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
    pub blocked_on: Vec<String>,
    pub branch: Option<String>,
}

pub struct EditIssueInput {
    pub title: Option<String>,
    pub body: Option<String>,
    pub assignee: Option<Option<String>>,
    pub parent_id: Option<Option<String>>,
    pub blocked_on: Option<Vec<String>>,
    pub branch: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db error: {0}")]
    Db(#[from] db::Error),
    #[error("bad request: {0}")]
    BadRequest(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct StartIssueOutcome {
    pub issue: Issue,
    pub session: Session,
    pub initial_message: String,
}

#[derive(Clone)]
pub struct IssueService {
    db: Arc<dyn db::Db>,
}

impl IssueService {
    pub fn new(db: Arc<dyn db::Db>) -> Self {
        Self { db }
    }

    pub fn db(&self) -> &Arc<dyn db::Db> {
        &self.db
    }

    pub async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue> {
        let now = Utc::now();
        let id = generate_issue_id();
        let branch = if let Some(b) = input.branch {
            b
        } else if let Some(ref parent_id) = input.parent_id {
            match self.db.get_issue(parent_id.clone()).await {
                Ok(parent) => parent.branch,
                Err(_) => format!("{}-{}", id, slugify(&input.title)),
            }
        } else {
            format!("{}-{}", id, slugify(&input.title))
        };
        let issue = Issue {
            id,
            title: input.title,
            body: input.body,
            status: IssueStatus::Open,
            branch,
            assignee: input.assignee,
            session_id: None,
            parent_id: input.parent_id,
            blocked_on: input.blocked_on,
            comments: vec![],
            created_at: now,
            updated_at: now,
        };
        self.db.create_issue(&issue).await?;
        Ok(issue)
    }

    pub async fn edit_issue(&self, id: String, input: EditIssueInput) -> Result<Issue> {
        let mut issue = self.db.get_issue(id).await?;
        if let Some(title) = input.title {
            issue.title = title;
        }
        if let Some(body) = input.body {
            issue.body = body;
        }
        if let Some(assignee_opt) = input.assignee {
            issue.assignee = assignee_opt;
        }
        if let Some(parent_opt) = input.parent_id {
            issue.parent_id = parent_opt;
        }
        if let Some(blocked_on) = input.blocked_on {
            issue.blocked_on = blocked_on;
        }
        if let Some(branch) = input.branch {
            issue.branch = branch;
        }
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        Ok(issue)
    }

    pub async fn add_comment(&self, id: String, author: String, body: String) -> Result<Issue> {
        let mut issue = self.db.get_issue(id).await?;
        issue.comments.push(IssueComment {
            author,
            created_at: Utc::now(),
            body,
        });
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        Ok(issue)
    }

    pub async fn finish_issue(&self, id: &str, summary: Option<String>) -> Result<()> {
        let mut issue = self.db.get_issue(id.to_string()).await?;
        if let Some(text) = summary {
            if !text.is_empty() {
                let author = issue.assignee.clone().unwrap_or_else(|| "agent".to_string());
                issue.comments.push(IssueComment {
                    author,
                    created_at: Utc::now(),
                    body: text,
                });
            }
        }
        issue.status = IssueStatus::Completed;
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        Ok(())
    }

    pub async fn fail_issue(&self, id: &str, error_message: String) -> Result<()> {
        let mut issue = self.db.get_issue(id.to_string()).await?;
        issue.comments.push(IssueComment {
            author: "system".to_string(),
            created_at: Utc::now(),
            body: error_message,
        });
        issue.status = IssueStatus::Failed;
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        Ok(())
    }

    pub async fn start_issue(&self, id: String) -> Result<StartIssueOutcome> {
        let mut issue = self.db.get_issue(id.clone()).await?;

        if issue.assignee.is_none() {
            return Err(Error::BadRequest("issue has no assignee; set one with `issue edit --assignee <agent>`".into()));
        }
        if issue.status != IssueStatus::Open {
            return Err(Error::BadRequest(format!("issue is already {}", issue.status)));
        }

        let now = Utc::now();
        let session = Session {
            id: Uuid::new_v4(),
            name: format!("issue-{}", issue.id),
            status: SessionStatus::Created,
            agent: issue.assignee.clone(),
            created_at: now,
            updated_at: now,
        };
        self.db.create_session(&session).await?;

        issue.session_id = Some(session.id);
        issue.status = IssueStatus::Running;
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;

        let mut initial_message = format!("{}\n\n{}", issue.title, issue.body);
        if !issue.comments.is_empty() {
            initial_message.push_str("\n\n---\n# Issue History\n");
            for comment in &issue.comments {
                initial_message.push_str(&format!(
                    "\n**{}** ({}): {}\n",
                    comment.author,
                    comment.created_at.format("%Y-%m-%d %H:%M UTC"),
                    comment.body
                ));
            }
        }

        Ok(StartIssueOutcome { issue, session, initial_message })
    }

    pub async fn complete_issue(&self, id: String, comment: String) -> Result<Issue> {
        let mut issue = self.db.get_issue(id.clone()).await?;
        if matches!(issue.status, IssueStatus::Completed | IssueStatus::Failed) {
            return Err(Error::BadRequest(format!("issue is already {}", issue.status)));
        }
        issue.comments.push(IssueComment {
            author: "user".into(),
            created_at: Utc::now(),
            body: comment,
        });
        issue.status = IssueStatus::Completed;
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        Ok(issue)
    }

    pub async fn reopen_issue(&self, id: String, comment: Option<String>) -> Result<Issue> {
        let mut issue = self.db.get_issue(id.clone()).await?;

        // Only `failed` and `completed` can be reopened
        let keep_session_id = match issue.status {
            IssueStatus::Failed => false,   // clear session_id → fresh session on next start
            IssueStatus::Completed => true, // keep session_id → resume history on next start
            _ => {
                return Err(Error::BadRequest(format!(
                    "cannot reopen issue {id}: only failed or completed issues can be reopened (current status: {})",
                    issue.status
                )));
            }
        };

        // Optionally append a user comment before the status transition
        if let Some(comment_text) = comment {
            if !comment_text.is_empty() {
                issue.comments.push(IssueComment {
                    author: "user".into(),
                    created_at: Utc::now(),
                    body: comment_text,
                });
            }
        }

        issue.status = IssueStatus::Open;
        if !keep_session_id {
            issue.session_id = None;
        }
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        Ok(issue)
    }

    /// Orphan sweep: run at startup before accepting any connections.
    ///
    /// Finds all sessions stuck in `running` state (no live harness after restart),
    /// marks them `failed`, and for any linked issue does the same while appending a
    /// system comment so the issue history is self-explanatory.
    ///
    /// Errors are logged and swallowed — a sweep failure must not crash the server.
    pub async fn orphan_sweep(&self) {
        let orphans = match self.db.list_sessions(Some(SessionStatus::Running)).await {
            Ok(sessions) => sessions,
            Err(e) => {
                eprintln!("[orphan_sweep] failed to list running sessions: {e}");
                return;
            }
        };

        for session in orphans {
            // 1. Mark the session failed.
            if let Err(e) = self.db.update_session_status(session.id, SessionStatus::Failed).await {
                eprintln!("[orphan_sweep] failed to update session {} to failed: {e}", session.id);
                // Continue — try the rest.
            }

            // 2. Find any issue linked to this session and recover it too.
            let issues = match self.db.list_issues_by_session_id(session.id).await {
                Ok(issues) => issues,
                Err(e) => {
                    eprintln!(
                        "[orphan_sweep] failed to list issues for session {}: {e}",
                        session.id
                    );
                    continue;
                }
            };

            for mut issue in issues {
                issue.comments.push(IssueComment {
                    author: "system".into(),
                    body: "session lost on server restart".into(),
                    created_at: Utc::now(),
                });
                issue.status = types::IssueStatus::Failed;
                issue.updated_at = Utc::now();

                if let Err(e) = self.db.update_issue(&issue).await {
                    eprintln!(
                        "[orphan_sweep] failed to update issue {} to failed: {e}",
                        issue.id
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use db::{IssueDb, SessionDb};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use types::{ContentBlock, Role, Turn};

    // --- MemoryDb ---

    struct MemoryDb {
        sessions: Mutex<HashMap<Uuid, Session>>,
        issues: Mutex<HashMap<String, Issue>>,
    }

    impl MemoryDb {
        fn new() -> Self {
            Self {
                sessions: Mutex::new(HashMap::new()),
                issues: Mutex::new(HashMap::new()),
            }
        }
    }

    impl db::Db for MemoryDb {}

    #[async_trait]
    impl db::SessionDb for MemoryDb {
        async fn create_session(&self, session: &Session) -> db::Result<()> {
            self.sessions.lock().unwrap().insert(session.id, session.clone());
            Ok(())
        }

        async fn get_session(&self, id: Uuid) -> db::Result<Session> {
            self.sessions
                .lock()
                .unwrap()
                .get(&id)
                .cloned()
                .ok_or(db::Error::NotFound)
        }

        async fn list_sessions(&self, status: Option<SessionStatus>) -> db::Result<Vec<Session>> {
            let sessions = self.sessions.lock().unwrap();
            let result = sessions
                .values()
                .filter(|s| status.as_ref().is_none_or(|st| &s.status == st))
                .cloned()
                .collect();
            Ok(result)
        }

        async fn update_session_status(&self, id: Uuid, status: SessionStatus) -> db::Result<()> {
            let mut sessions = self.sessions.lock().unwrap();
            let session = sessions.get_mut(&id).ok_or(db::Error::NotFound)?;
            session.status = status;
            Ok(())
        }
    }

    #[async_trait]
    impl db::TurnDb for MemoryDb {
        async fn create_turn(&self, _turn: &Turn) -> db::Result<()> {
            Ok(())
        }

        async fn list_turns(&self, _session_id: Uuid) -> db::Result<Vec<Turn>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl db::ContentBlockDb for MemoryDb {
        async fn create_content_block(
            &self,
            _turn_id: Uuid,
            _block_index: i64,
            _role: &Role,
            _block: &ContentBlock,
        ) -> db::Result<()> {
            Ok(())
        }

        async fn list_content_blocks(&self, _turn_id: Uuid) -> db::Result<Vec<(Role, ContentBlock)>> {
            Ok(vec![])
        }

        async fn get_last_text_for_session(&self, _session_id: Uuid) -> db::Result<Option<String>> {
            Ok(None)
        }
    }

    #[async_trait]
    impl db::IssueDb for MemoryDb {
        async fn create_issue(&self, issue: &Issue) -> db::Result<()> {
            self.issues.lock().unwrap().insert(issue.id.clone(), issue.clone());
            Ok(())
        }

        async fn get_issue(&self, id: String) -> db::Result<Issue> {
            self.issues
                .lock()
                .unwrap()
                .get(&id)
                .cloned()
                .ok_or(db::Error::NotFound)
        }

        async fn list_issues(
            &self,
            _status: Option<IssueStatus>,
            _assignee: Option<String>,
            _parent_id: Option<String>,
        ) -> db::Result<Vec<Issue>> {
            Ok(self.issues.lock().unwrap().values().cloned().collect())
        }

        async fn list_issues_by_session_id(&self, session_id: Uuid) -> db::Result<Vec<Issue>> {
            let issues = self.issues.lock().unwrap();
            let result = issues
                .values()
                .filter(|i| i.session_id == Some(session_id))
                .cloned()
                .collect();
            Ok(result)
        }

        async fn update_issue(&self, issue: &Issue) -> db::Result<()> {
            let mut issues = self.issues.lock().unwrap();
            if !issues.contains_key(&issue.id) {
                return Err(db::Error::NotFound);
            }
            issues.insert(issue.id.clone(), issue.clone());
            Ok(())
        }
    }

    // --- Helpers ---

    fn make_service(db: Arc<dyn db::Db>) -> IssueService {
        IssueService::new(db)
    }

    fn open_issue(id: &str) -> Issue {
        let now = Utc::now();
        Issue {
            id: id.into(),
            title: "Test issue".into(),
            body: "Test body".into(),
            status: IssueStatus::Open,
            branch: String::new(),
            assignee: Some("swe".into()),
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    // --- Tests ---

    #[tokio::test]
    async fn start_issue_transitions_open_to_running() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let outcome = svc.start_issue("ab12".into()).await.unwrap();

        assert_eq!(outcome.issue.status, IssueStatus::Running);
        assert!(outcome.issue.session_id.is_some());

        // Verify persisted in db
        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Running);
        assert!(persisted.session_id.is_some());
    }

    #[tokio::test]
    async fn start_issue_requires_assignee() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.assignee = None;
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.start_issue("ab12".into()).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn start_issue_requires_open_status() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.start_issue("ab12".into()).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn complete_issue_adds_comment_and_marks_completed() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.complete_issue("ab12".into(), "Looks good".into()).await.unwrap();

        assert_eq!(result.status, IssueStatus::Completed);
        assert_eq!(result.comments.len(), 1);
        assert_eq!(result.comments[0].author, "user");
        assert_eq!(result.comments[0].body, "Looks good");

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Completed);
    }

    #[tokio::test]
    async fn complete_issue_fails_if_already_completed() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Completed;
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.complete_issue("ab12".into(), "again".into()).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn complete_issue_fails_if_already_failed() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Failed;
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.complete_issue("ab12".into(), "again".into()).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn reopen_failed_issue_clears_session_id() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Failed;
        issue.session_id = Some(Uuid::new_v4());
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.reopen_issue("ab12".into(), None).await.unwrap();

        assert_eq!(result.status, IssueStatus::Open);
        assert!(result.session_id.is_none());

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert!(persisted.session_id.is_none());
    }

    #[tokio::test]
    async fn reopen_completed_issue_keeps_session_id() {
        let db = Arc::new(MemoryDb::new());
        let session_id = Uuid::new_v4();
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Completed;
        issue.session_id = Some(session_id);
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.reopen_issue("ab12".into(), None).await.unwrap();

        assert_eq!(result.status, IssueStatus::Open);
        assert_eq!(result.session_id, Some(session_id));

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.session_id, Some(session_id));
    }

    #[tokio::test]
    async fn reopen_open_issue_fails() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.reopen_issue("ab12".into(), None).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn reopen_running_issue_fails() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc.reopen_issue("ab12".into(), None).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn reopen_with_comment_appends_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Failed;
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        let result = svc
            .reopen_issue("ab12".into(), Some("please try again".into()))
            .await
            .unwrap();

        assert_eq!(result.status, IssueStatus::Open);
        assert_eq!(result.comments.len(), 1);
        assert_eq!(result.comments[0].author, "user");
        assert_eq!(result.comments[0].body, "please try again");
    }

    #[tokio::test]
    async fn orphan_sweep_marks_running_session_failed() {
        let db = Arc::new(MemoryDb::new());
        let now = Utc::now();
        let session = Session {
            id: Uuid::new_v4(),
            name: "orphan-session".into(),
            status: SessionStatus::Running,
            agent: None,
            created_at: now,
            updated_at: now,
        };
        db.create_session(&session).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        svc.orphan_sweep().await;

        let fetched = db.get_session(session.id).await.unwrap();
        assert_eq!(fetched.status, SessionStatus::Failed);
    }

    #[tokio::test]
    async fn orphan_sweep_marks_linked_issue_failed_with_comment() {
        let db = Arc::new(MemoryDb::new());
        let now = Utc::now();
        let session = Session {
            id: Uuid::new_v4(),
            name: "orphan-session".into(),
            status: SessionStatus::Running,
            agent: None,
            created_at: now,
            updated_at: now,
        };
        db.create_session(&session).await.unwrap();

        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        issue.session_id = Some(session.id);
        db.create_issue(&issue).await.unwrap();

        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);
        svc.orphan_sweep().await;

        let fetched_issue = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(fetched_issue.status, IssueStatus::Failed);
        assert_eq!(fetched_issue.comments.len(), 1);
        assert_eq!(fetched_issue.comments[0].author, "system");
        assert_eq!(fetched_issue.comments[0].body, "session lost on server restart");
    }

    // --- create_issue ---

    #[tokio::test]
    async fn create_issue_returns_open_issue_with_generated_id() {
        let db = Arc::new(MemoryDb::new());
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let issue = svc.create_issue(CreateIssueInput {
            title: "Fix the bug".into(),
            body: "Details".into(),
            assignee: None,
            parent_id: None,
            blocked_on: vec![],
            branch: None,
        }).await.unwrap();

        assert_eq!(issue.status, IssueStatus::Open);
        assert_eq!(issue.id.len(), 4);
        assert!(issue.branch.contains("fix-the-bug"));

        let persisted = db.get_issue(issue.id.clone()).await.unwrap();
        assert_eq!(persisted.title, "Fix the bug");
    }

    #[tokio::test]
    async fn create_issue_uses_explicit_branch() {
        let db = Arc::new(MemoryDb::new());
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let issue = svc.create_issue(CreateIssueInput {
            title: "My task".into(),
            body: "B".into(),
            assignee: None,
            parent_id: None,
            blocked_on: vec![],
            branch: Some("my-explicit-branch".into()),
        }).await.unwrap();

        assert_eq!(issue.branch, "my-explicit-branch");
    }

    #[tokio::test]
    async fn create_issue_inherits_parent_branch() {
        let db = Arc::new(MemoryDb::new());
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let parent = svc.create_issue(CreateIssueInput {
            title: "Parent".into(),
            body: "B".into(),
            assignee: None,
            parent_id: None,
            blocked_on: vec![],
            branch: Some("parent-branch".into()),
        }).await.unwrap();

        let child = svc.create_issue(CreateIssueInput {
            title: "Child".into(),
            body: "B".into(),
            assignee: None,
            parent_id: Some(parent.id.clone()),
            blocked_on: vec![],
            branch: None,
        }).await.unwrap();

        assert_eq!(child.branch, "parent-branch");
    }

    // --- edit_issue ---

    #[tokio::test]
    async fn edit_issue_updates_fields() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let updated = svc.edit_issue("ab12".into(), EditIssueInput {
            title: Some("New title".into()),
            body: None,
            assignee: None,
            parent_id: None,
            blocked_on: None,
            branch: None,
        }).await.unwrap();

        assert_eq!(updated.title, "New title");
        assert_eq!(updated.body, "Test body");
    }

    #[tokio::test]
    async fn edit_issue_clears_parent_with_explicit_none() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.parent_id = Some("parent1".into());
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let updated = svc.edit_issue("ab12".into(), EditIssueInput {
            title: None,
            body: None,
            assignee: None,
            parent_id: Some(None),
            blocked_on: None,
            branch: None,
        }).await.unwrap();

        assert!(updated.parent_id.is_none());
    }

    #[tokio::test]
    async fn edit_issue_absent_field_leaves_unchanged() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.parent_id = Some("parent1".into());
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let updated = svc.edit_issue("ab12".into(), EditIssueInput {
            title: Some("Renamed".into()),
            body: None,
            assignee: None,
            parent_id: None,
            blocked_on: None,
            branch: None,
        }).await.unwrap();

        assert_eq!(updated.parent_id, Some("parent1".into()));
    }

    // --- add_comment ---

    #[tokio::test]
    async fn add_comment_appends_to_issue() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let updated = svc.add_comment("ab12".into(), "user".into(), "Great issue".into()).await.unwrap();

        assert_eq!(updated.comments.len(), 1);
        assert_eq!(updated.comments[0].author, "user");
        assert_eq!(updated.comments[0].body, "Great issue");

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.comments.len(), 1);
    }

    // --- finish_issue ---

    #[tokio::test]
    async fn finish_issue_marks_completed_with_summary_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        issue.assignee = Some("swe".into());
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.finish_issue("ab12", Some("Done! All tests pass.".into())).await.unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Completed);
        assert_eq!(persisted.comments.len(), 1);
        assert_eq!(persisted.comments[0].author, "swe");
        assert_eq!(persisted.comments[0].body, "Done! All tests pass.");
    }

    #[tokio::test]
    async fn finish_issue_no_summary_adds_no_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.finish_issue("ab12", None).await.unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Completed);
        assert_eq!(persisted.comments.len(), 0);
    }

    #[tokio::test]
    async fn finish_issue_empty_summary_adds_no_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.finish_issue("ab12", Some(String::new())).await.unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Completed);
        assert_eq!(persisted.comments.len(), 0);
    }

    #[tokio::test]
    async fn finish_issue_uses_agent_author_when_no_assignee() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        issue.assignee = None;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.finish_issue("ab12", Some("summary".into())).await.unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.comments[0].author, "agent");
    }

    // --- fail_issue ---

    #[tokio::test]
    async fn fail_issue_marks_failed_with_system_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.fail_issue("ab12", "timeout exceeded".into()).await.unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Failed);
        assert_eq!(persisted.comments.len(), 1);
        assert_eq!(persisted.comments[0].author, "system");
        assert_eq!(persisted.comments[0].body, "timeout exceeded");
    }

    // --- slugify ---

    #[test]
    fn slugify_simple_title() {
        assert_eq!(slugify("Fix the bug"), "fix-the-bug");
    }

    #[test]
    fn slugify_consecutive_specials_collapsed() {
        assert_eq!(slugify("foo--bar"), "foo-bar");
        assert_eq!(slugify("foo  bar"), "foo-bar");
    }

    #[test]
    fn slugify_trims_leading_trailing_dashes() {
        assert_eq!(slugify("  leading"), "leading");
        assert_eq!(slugify("trailing  "), "trailing");
    }
}
