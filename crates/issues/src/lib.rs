use chrono::Utc;
use events::{EventBus, IssueEvent, SystemEvent};
use std::fmt::Write as _;
use std::sync::Arc;
use types::{Issue, IssueComment, IssueStatus, Session, SessionStatus};
use uuid::Uuid;

#[must_use]
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

#[must_use]
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
    event_bus: EventBus,
}

impl IssueService {
    pub fn new(db: Arc<dyn db::Db>) -> Self {
        Self {
            db,
            event_bus: EventBus::new(1024),
        }
    }

    /// Create an `IssueService` with an externally-provided `EventBus`.
    /// Use this when you want issue events to flow into the same bus as
    /// session events (i.e. the server's global bus).
    pub fn with_event_bus(db: Arc<dyn db::Db>, event_bus: EventBus) -> Self {
        Self { db, event_bus }
    }

    #[must_use]
    pub fn db(&self) -> &Arc<dyn db::Db> {
        &self.db
    }

    #[must_use]
    pub const fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }

    /// # Errors
    ///
    /// Returns an error if the database write fails.
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
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::Created(issue.clone())));
        Ok(issue)
    }

    /// # Errors
    ///
    /// Returns an error if the issue is not found or the database write fails.
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

    /// # Errors
    ///
    /// Returns an error if the issue is not found or the database write fails.
    pub async fn add_comment(&self, id: String, author: String, body: String) -> Result<Issue> {
        let mut issue = self.db.get_issue(id).await?;
        let comment = IssueComment {
            author,
            created_at: Utc::now(),
            body,
        };
        issue.comments.push(comment.clone());
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::CommentAdded {
                issue: issue.clone(),
                comment,
            }));
        Ok(issue)
    }

    /// # Errors
    ///
    /// Returns an error if the issue is not found or the database write fails.
    pub async fn finish_issue(&self, id: &str, summary: Option<String>) -> Result<()> {
        let mut issue = self.db.get_issue(id.to_string()).await?;
        let from = issue.status.clone();
        if let Some(text) = summary {
            if !text.is_empty() {
                let author = issue
                    .assignee
                    .clone()
                    .unwrap_or_else(|| "agent".to_string());
                let comment = IssueComment {
                    author,
                    created_at: Utc::now(),
                    body: text,
                };
                issue.comments.push(comment.clone());
                self.event_bus
                    .send(SystemEvent::Issue(IssueEvent::CommentAdded {
                        issue: issue.clone(),
                        comment,
                    }));
            }
        }
        issue.status = IssueStatus::Completed;
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::StatusChanged {
                issue: issue.clone(),
                from,
                to: IssueStatus::Completed,
            }));
        Ok(())
    }

    /// Park an issue after a session ends: optionally add a comment, then set
    /// status to the provided `status` (must be `Waiting` or `Completed`).
    ///
    /// This is called by the issue watcher when it receives a `Stopped` event
    /// from the harness, enabling the comment and status to be set atomically.
    ///
    /// # Errors
    ///
    /// Returns an error if the issue is not found, the database write fails, or
    /// `status` is not `Waiting` or `Completed`.
    pub async fn park_issue(
        &self,
        id: &str,
        status: IssueStatus,
        comment: Option<String>,
        author: Option<String>,
    ) -> Result<()> {
        match status {
            IssueStatus::Waiting | IssueStatus::Completed => {}
            other => {
                return Err(Error::BadRequest(format!(
                    "park_issue: status must be Waiting or Completed, got {other}"
                )));
            }
        }

        let mut issue = self.db.get_issue(id.to_string()).await?;
        let from = issue.status.clone();

        if let Some(text) = comment {
            if !text.is_empty() {
                let comment_author = author
                    .or_else(|| issue.assignee.clone())
                    .unwrap_or_else(|| "agent".to_string());
                let comment_obj = IssueComment {
                    author: comment_author,
                    created_at: Utc::now(),
                    body: text,
                };
                issue.comments.push(comment_obj.clone());
                self.event_bus
                    .send(SystemEvent::Issue(IssueEvent::CommentAdded {
                        issue: issue.clone(),
                        comment: comment_obj,
                    }));
            }
        }

        issue.status = status.clone();
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::StatusChanged {
                issue: issue.clone(),
                from,
                to: status,
            }));
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the issue is not found or the database write fails.
    pub async fn fail_issue(&self, id: &str, error_message: String) -> Result<()> {
        let mut issue = self.db.get_issue(id.to_string()).await?;
        let from = issue.status.clone();
        let comment = IssueComment {
            author: "system".to_string(),
            created_at: Utc::now(),
            body: error_message,
        };
        issue.comments.push(comment.clone());
        issue.status = IssueStatus::Failed;
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::CommentAdded {
                issue: issue.clone(),
                comment,
            }));
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::StatusChanged {
                issue: issue.clone(),
                from,
                to: IssueStatus::Failed,
            }));
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the issue is not found, has no assignee, is not open,
    /// or a database write fails.
    pub async fn start_issue(&self, id: String) -> Result<StartIssueOutcome> {
        let mut issue = self.db.get_issue(id.clone()).await?;

        if issue.assignee.is_none() {
            return Err(Error::BadRequest(
                "issue has no assignee; set one with `issue edit --assignee <agent>`".into(),
            ));
        }
        if issue.status != IssueStatus::Open {
            return Err(Error::BadRequest(format!(
                "issue is already {}",
                issue.status
            )));
        }

        let from = issue.status.clone();
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

        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::StatusChanged {
                issue: issue.clone(),
                from,
                to: IssueStatus::Running,
            }));

        let mut initial_message = format!("{}\n\n{}", issue.title, issue.body);
        if !issue.comments.is_empty() {
            initial_message.push_str("\n\n---\n# Issue History\n");
            for comment in &issue.comments {
                let _ = writeln!(
                    initial_message,
                    "\n**{}** ({}): {}",
                    comment.author,
                    comment.created_at.format("%Y-%m-%d %H:%M UTC"),
                    comment.body
                );
            }
        }

        Ok(StartIssueOutcome {
            issue,
            session,
            initial_message,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the issue is not found, is already completed or failed,
    /// or a database write fails.
    pub async fn complete_issue(&self, id: String, comment: String) -> Result<Issue> {
        let mut issue = self.db.get_issue(id.clone()).await?;
        if matches!(issue.status, IssueStatus::Completed | IssueStatus::Failed) {
            return Err(Error::BadRequest(format!(
                "issue is already {}",
                issue.status
            )));
        }
        let from = issue.status.clone();
        let new_comment = IssueComment {
            author: "user".into(),
            created_at: Utc::now(),
            body: comment,
        };
        issue.comments.push(new_comment.clone());
        issue.status = IssueStatus::Completed;
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::CommentAdded {
                issue: issue.clone(),
                comment: new_comment,
            }));
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::StatusChanged {
                issue: issue.clone(),
                from,
                to: IssueStatus::Completed,
            }));
        Ok(issue)
    }

    /// # Errors
    ///
    /// Returns an error if the issue is not found, is not open or running,
    /// or a database write fails.
    pub async fn cancel_issue(&self, id: String) -> Result<Issue> {
        let mut issue = self.db.get_issue(id.clone()).await?;

        match issue.status {
            IssueStatus::Open | IssueStatus::Running | IssueStatus::Waiting => {}
            _ => {
                return Err(Error::BadRequest(format!(
                    "cannot cancel issue {id}: only open, running, or waiting issues can be cancelled (current status: {})",
                    issue.status
                )));
            }
        }

        let from = issue.status.clone();
        let comment = IssueComment {
            author: "system".to_string(),
            created_at: Utc::now(),
            body: "issue cancelled by user".to_string(),
        };
        issue.comments.push(comment.clone());
        issue.status = IssueStatus::Cancelled;
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::CommentAdded {
                issue: issue.clone(),
                comment,
            }));
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::StatusChanged {
                issue: issue.clone(),
                from,
                to: IssueStatus::Cancelled,
            }));
        Ok(issue)
    }

    /// # Errors
    ///
    /// Returns an error if the issue is not found, is not failed/completed/cancelled,
    /// or a database write fails.
    pub async fn reopen_issue(&self, id: String, comment: Option<String>) -> Result<Issue> {
        let mut issue = self.db.get_issue(id.clone()).await?;

        // Only `failed`, `completed`, and `cancelled` can be reopened
        let keep_session_id = match issue.status {
            IssueStatus::Failed | IssueStatus::Cancelled => false, // clear session_id → fresh session on next start
            IssueStatus::Completed | IssueStatus::Waiting => true, // keep session_id → resume history on next start
            _ => {
                return Err(Error::BadRequest(format!(
                    "cannot reopen issue {id}: only failed, completed, cancelled, or waiting issues can be reopened (current status: {})",
                    issue.status
                )));
            }
        };

        let from = issue.status.clone();

        // Optionally append a user comment before the status transition
        if let Some(comment_text) = comment {
            if !comment_text.is_empty() {
                let new_comment = IssueComment {
                    author: "user".into(),
                    created_at: Utc::now(),
                    body: comment_text,
                };
                issue.comments.push(new_comment.clone());
                self.event_bus
                    .send(SystemEvent::Issue(IssueEvent::CommentAdded {
                        issue: issue.clone(),
                        comment: new_comment,
                    }));
            }
        }

        issue.status = IssueStatus::Open;
        if !keep_session_id {
            issue.session_id = None;
        }
        issue.updated_at = Utc::now();
        self.db.update_issue(&issue).await?;
        self.event_bus
            .send(SystemEvent::Issue(IssueEvent::StatusChanged {
                issue: issue.clone(),
                from,
                to: IssueStatus::Open,
            }));
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
            if let Err(e) = self
                .db
                .update_session_status(session.id, SessionStatus::Failed)
                .await
            {
                eprintln!(
                    "[orphan_sweep] failed to update session {} to failed: {e}",
                    session.id
                );
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
            self.sessions
                .lock()
                .unwrap()
                .insert(session.id, session.clone());
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
            let result: Vec<Session> = sessions
                .values()
                .filter(|s| status.as_ref().is_none_or(|st| &s.status == st))
                .cloned()
                .collect();
            drop(sessions);
            Ok(result)
        }

        async fn update_session_status(&self, id: Uuid, status: SessionStatus) -> db::Result<()> {
            let mut sessions = self.sessions.lock().unwrap();
            let session = sessions.get_mut(&id).ok_or(db::Error::NotFound)?;
            session.status = status;
            drop(sessions);
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

        async fn list_content_blocks(
            &self,
            _turn_id: Uuid,
        ) -> db::Result<Vec<(Role, ContentBlock)>> {
            Ok(vec![])
        }

        async fn get_last_text_for_session(&self, _session_id: Uuid) -> db::Result<Option<String>> {
            Ok(None)
        }
    }

    #[async_trait]
    impl db::IssueDb for MemoryDb {
        async fn create_issue(&self, issue: &Issue) -> db::Result<()> {
            self.issues
                .lock()
                .unwrap()
                .insert(issue.id.clone(), issue.clone());
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
            let result: Vec<Issue> = issues
                .values()
                .filter(|i| i.session_id == Some(session_id))
                .cloned()
                .collect();
            drop(issues);
            Ok(result)
        }

        async fn update_issue(&self, issue: &Issue) -> db::Result<()> {
            let mut issues = self.issues.lock().unwrap();
            if !issues.contains_key(&issue.id) {
                return Err(db::Error::NotFound);
            }
            issues.insert(issue.id.clone(), issue.clone());
            drop(issues);
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
        let result = svc
            .complete_issue("ab12".into(), "Looks good".into())
            .await
            .unwrap();

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
        assert_eq!(
            fetched_issue.comments[0].body,
            "session lost on server restart"
        );
    }

    // --- create_issue ---

    #[tokio::test]
    async fn create_issue_returns_open_issue_with_generated_id() {
        let db = Arc::new(MemoryDb::new());
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let issue = svc
            .create_issue(CreateIssueInput {
                title: "Fix the bug".into(),
                body: "Details".into(),
                assignee: None,
                parent_id: None,
                blocked_on: vec![],
                branch: None,
            })
            .await
            .unwrap();

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

        let issue = svc
            .create_issue(CreateIssueInput {
                title: "My task".into(),
                body: "B".into(),
                assignee: None,
                parent_id: None,
                blocked_on: vec![],
                branch: Some("my-explicit-branch".into()),
            })
            .await
            .unwrap();

        assert_eq!(issue.branch, "my-explicit-branch");
    }

    #[tokio::test]
    async fn create_issue_inherits_parent_branch() {
        let db = Arc::new(MemoryDb::new());
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let parent = svc
            .create_issue(CreateIssueInput {
                title: "Parent".into(),
                body: "B".into(),
                assignee: None,
                parent_id: None,
                blocked_on: vec![],
                branch: Some("parent-branch".into()),
            })
            .await
            .unwrap();

        let child = svc
            .create_issue(CreateIssueInput {
                title: "Child".into(),
                body: "B".into(),
                assignee: None,
                parent_id: Some(parent.id.clone()),
                blocked_on: vec![],
                branch: None,
            })
            .await
            .unwrap();

        assert_eq!(child.branch, "parent-branch");
    }

    // --- edit_issue ---

    #[tokio::test]
    async fn edit_issue_updates_fields() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let updated = svc
            .edit_issue(
                "ab12".into(),
                EditIssueInput {
                    title: Some("New title".into()),
                    body: None,
                    assignee: None,
                    parent_id: None,
                    blocked_on: None,
                    branch: None,
                },
            )
            .await
            .unwrap();

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

        let updated = svc
            .edit_issue(
                "ab12".into(),
                EditIssueInput {
                    title: None,
                    body: None,
                    assignee: None,
                    parent_id: Some(None),
                    blocked_on: None,
                    branch: None,
                },
            )
            .await
            .unwrap();

        assert!(updated.parent_id.is_none());
    }

    #[tokio::test]
    async fn edit_issue_absent_field_leaves_unchanged() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.parent_id = Some("parent1".into());
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let updated = svc
            .edit_issue(
                "ab12".into(),
                EditIssueInput {
                    title: Some("Renamed".into()),
                    body: None,
                    assignee: None,
                    parent_id: None,
                    blocked_on: None,
                    branch: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.parent_id, Some("parent1".into()));
    }

    // --- add_comment ---

    #[tokio::test]
    async fn add_comment_appends_to_issue() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let updated = svc
            .add_comment("ab12".into(), "user".into(), "Great issue".into())
            .await
            .unwrap();

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

        svc.finish_issue("ab12", Some("Done! All tests pass.".into()))
            .await
            .unwrap();

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

        svc.finish_issue("ab12", Some("summary".into()))
            .await
            .unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.comments[0].author, "agent");
    }

    // --- park_issue ---

    #[tokio::test]
    async fn park_issue_complete_with_comment_marks_completed_and_adds_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        issue.assignee = Some("swe".into());
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.park_issue("ab12", IssueStatus::Completed, Some("Task done.".into()), None)
            .await
            .unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Completed);
        assert_eq!(persisted.comments.len(), 1);
        assert_eq!(persisted.comments[0].body, "Task done.");
        assert_eq!(persisted.comments[0].author, "swe");
    }

    #[tokio::test]
    async fn park_issue_waiting_with_no_comment_marks_waiting_no_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.park_issue("ab12", IssueStatus::Waiting, None, None).await.unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Waiting);
        assert_eq!(persisted.comments.len(), 0);
    }

    #[tokio::test]
    async fn park_issue_complete_no_comment_marks_completed_no_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.park_issue("ab12", IssueStatus::Completed, None, None).await.unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Completed);
        assert_eq!(persisted.comments.len(), 0);
    }

    #[tokio::test]
    async fn park_issue_uses_explicit_author_when_provided() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        issue.assignee = Some("bot".into());
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.park_issue(
            "ab12",
            IssueStatus::Completed,
            Some("Done".into()),
            Some("custom-author".into()),
        )
        .await
        .unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.comments[0].author, "custom-author");
    }

    #[tokio::test]
    async fn park_issue_falls_back_to_agent_when_no_assignee_and_no_author() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        issue.assignee = None;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.park_issue("ab12", IssueStatus::Completed, Some("summary".into()), None)
            .await
            .unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.comments[0].author, "agent");
    }

    #[tokio::test]
    async fn park_issue_rejects_invalid_status() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let result = svc.park_issue("ab12", IssueStatus::Running, None, None).await;
        assert!(
            matches!(result, Err(Error::BadRequest(_))),
            "park_issue with Running status should return BadRequest"
        );

        let result = svc.park_issue("ab12", IssueStatus::Failed, None, None).await;
        assert!(
            matches!(result, Err(Error::BadRequest(_))),
            "park_issue with Failed status should return BadRequest"
        );

        let result = svc.park_issue("ab12", IssueStatus::Open, None, None).await;
        assert!(
            matches!(result, Err(Error::BadRequest(_))),
            "park_issue with Open status should return BadRequest"
        );

        let result = svc.park_issue("ab12", IssueStatus::Cancelled, None, None).await;
        assert!(
            matches!(result, Err(Error::BadRequest(_))),
            "park_issue with Cancelled status should return BadRequest"
        );
    }

    #[tokio::test]
    async fn park_issue_empty_string_comment_not_added() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.park_issue("ab12", IssueStatus::Waiting, Some(String::new()), None)
            .await
            .unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(
            persisted.status,
            IssueStatus::Waiting,
            "status should change to Waiting"
        );
        assert_eq!(
            persisted.comments.len(),
            0,
            "empty-string comment must not be added"
        );
    }

    // --- fail_issue ---

    #[tokio::test]
    async fn fail_issue_marks_failed_with_system_comment() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        svc.fail_issue("ab12", "timeout exceeded".into())
            .await
            .unwrap();

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Failed);
        assert_eq!(persisted.comments.len(), 1);
        assert_eq!(persisted.comments[0].author, "system");
        assert_eq!(persisted.comments[0].body, "timeout exceeded");
    }

    // --- cancel_issue ---

    #[tokio::test]
    async fn cancel_open_issue_transitions_to_cancelled() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let result = svc.cancel_issue("ab12".into()).await.unwrap();

        assert_eq!(result.status, IssueStatus::Cancelled);
        assert_eq!(result.comments.len(), 1);
        assert_eq!(result.comments[0].author, "system");
        assert_eq!(result.comments[0].body, "issue cancelled by user");

        let persisted = db.get_issue("ab12".into()).await.unwrap();
        assert_eq!(persisted.status, IssueStatus::Cancelled);
    }

    #[tokio::test]
    async fn cancel_running_issue_transitions_to_cancelled() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        issue.session_id = Some(Uuid::new_v4());
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let result = svc.cancel_issue("ab12".into()).await.unwrap();

        assert_eq!(result.status, IssueStatus::Cancelled);
        assert!(
            result.session_id.is_some(),
            "session_id should be preserved"
        );
    }

    #[tokio::test]
    async fn cancel_completed_issue_fails() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Completed;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let result = svc.cancel_issue("ab12".into()).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn cancel_failed_issue_fails() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Failed;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let result = svc.cancel_issue("ab12".into()).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn cancel_already_cancelled_issue_fails() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Cancelled;
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let result = svc.cancel_issue("ab12".into()).await;

        assert!(matches!(result, Err(Error::BadRequest(_))));
    }

    #[tokio::test]
    async fn reopen_cancelled_issue_clears_session_id() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Cancelled;
        issue.session_id = Some(Uuid::new_v4());
        db.create_issue(&issue).await.unwrap();
        let svc = make_service(Arc::clone(&db) as Arc<dyn db::Db>);

        let result = svc.reopen_issue("ab12".into(), None).await.unwrap();

        assert_eq!(result.status, IssueStatus::Open);
        assert!(result.session_id.is_none());
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

    // --- event emission ---

    fn make_service_with_bus(db: &Arc<dyn db::Db>) -> (IssueService, events::EventBus) {
        let bus = events::EventBus::new(32);
        let svc = IssueService::with_event_bus(Arc::clone(db), bus.clone());
        (svc, bus)
    }

    #[tokio::test]
    async fn create_issue_emits_created_event() {
        let db = Arc::new(MemoryDb::new());
        let (svc, bus) = make_service_with_bus(&(Arc::clone(&db) as Arc<dyn db::Db>));
        let mut rx = bus.subscribe();

        svc.create_issue(CreateIssueInput {
            title: "Test event".into(),
            body: "body".into(),
            assignee: None,
            parent_id: None,
            blocked_on: vec![],
            branch: None,
        })
        .await
        .unwrap();

        let ev = rx.try_recv().expect("should have received an event");
        assert!(
            matches!(ev, events::SystemEvent::Issue(events::IssueEvent::Created(ref i)) if i.title == "Test event"),
            "expected IssueEvent::Created, got: {ev:?}"
        );
    }

    #[tokio::test]
    async fn start_issue_emits_status_changed_event() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();

        let (svc, bus) = make_service_with_bus(&(Arc::clone(&db) as Arc<dyn db::Db>));
        let mut rx = bus.subscribe();

        svc.start_issue("ab12".into()).await.unwrap();

        let ev = rx.try_recv().expect("should have received an event");
        assert!(
            matches!(
                ev,
                events::SystemEvent::Issue(events::IssueEvent::StatusChanged {
                    from: IssueStatus::Open,
                    to: IssueStatus::Running,
                    ..
                })
            ),
            "expected StatusChanged Open->Running, got: {ev:?}"
        );
    }

    #[tokio::test]
    async fn complete_issue_emits_status_changed_and_comment_added() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();

        let (svc, bus) = make_service_with_bus(&(Arc::clone(&db) as Arc<dyn db::Db>));
        let mut rx = bus.subscribe();

        svc.complete_issue("ab12".into(), "All done".into())
            .await
            .unwrap();

        let ev1 = rx.try_recv().expect("should have received first event");
        assert!(
            matches!(
                ev1,
                events::SystemEvent::Issue(events::IssueEvent::CommentAdded { .. })
            ),
            "first event should be CommentAdded, got: {ev1:?}"
        );

        let ev2 = rx.try_recv().expect("should have received second event");
        assert!(
            matches!(
                ev2,
                events::SystemEvent::Issue(events::IssueEvent::StatusChanged {
                    to: IssueStatus::Completed,
                    ..
                })
            ),
            "second event should be StatusChanged->Completed, got: {ev2:?}"
        );
    }

    #[tokio::test]
    async fn add_comment_emits_comment_added_event() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();

        let (svc, bus) = make_service_with_bus(&(Arc::clone(&db) as Arc<dyn db::Db>));
        let mut rx = bus.subscribe();

        svc.add_comment("ab12".into(), "tester".into(), "Great issue".into())
            .await
            .unwrap();

        let ev = rx.try_recv().expect("should have received an event");
        assert!(
            matches!(
                ev,
                events::SystemEvent::Issue(events::IssueEvent::CommentAdded {
                    ref comment, ..
                }) if comment.author == "tester" && comment.body == "Great issue"
            ),
            "expected CommentAdded with author 'tester', got: {ev:?}"
        );
    }

    #[tokio::test]
    async fn cancel_issue_emits_status_changed_event() {
        let db = Arc::new(MemoryDb::new());
        let issue = open_issue("ab12");
        db.create_issue(&issue).await.unwrap();

        let (svc, bus) = make_service_with_bus(&(Arc::clone(&db) as Arc<dyn db::Db>));
        let mut rx = bus.subscribe();

        svc.cancel_issue("ab12".into()).await.unwrap();

        // Skip CommentAdded
        let _comment_ev = rx.try_recv().expect("should have received CommentAdded");

        let ev = rx.try_recv().expect("should have received StatusChanged");
        assert!(
            matches!(
                ev,
                events::SystemEvent::Issue(events::IssueEvent::StatusChanged {
                    to: IssueStatus::Cancelled,
                    ..
                })
            ),
            "expected StatusChanged->Cancelled, got: {ev:?}"
        );
    }

    #[tokio::test]
    async fn reopen_issue_emits_status_changed_event() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Failed;
        db.create_issue(&issue).await.unwrap();

        let (svc, bus) = make_service_with_bus(&(Arc::clone(&db) as Arc<dyn db::Db>));
        let mut rx = bus.subscribe();

        svc.reopen_issue("ab12".into(), None).await.unwrap();

        let ev = rx.try_recv().expect("should have received an event");
        assert!(
            matches!(
                ev,
                events::SystemEvent::Issue(events::IssueEvent::StatusChanged {
                    from: IssueStatus::Failed,
                    to: IssueStatus::Open,
                    ..
                })
            ),
            "expected StatusChanged Failed->Open, got: {ev:?}"
        );
    }

    #[tokio::test]
    async fn fail_issue_emits_comment_added_and_status_changed() {
        let db = Arc::new(MemoryDb::new());
        let mut issue = open_issue("ab12");
        issue.status = IssueStatus::Running;
        db.create_issue(&issue).await.unwrap();

        let (svc, bus) = make_service_with_bus(&(Arc::clone(&db) as Arc<dyn db::Db>));
        let mut rx = bus.subscribe();

        svc.fail_issue("ab12", "timeout".into()).await.unwrap();

        let ev1 = rx.try_recv().expect("should have received first event");
        assert!(
            matches!(
                ev1,
                events::SystemEvent::Issue(events::IssueEvent::CommentAdded { .. })
            ),
            "first event should be CommentAdded, got: {ev1:?}"
        );

        let ev2 = rx.try_recv().expect("should have received second event");
        assert!(
            matches!(
                ev2,
                events::SystemEvent::Issue(events::IssueEvent::StatusChanged {
                    to: IssueStatus::Failed,
                    ..
                })
            ),
            "second event should be StatusChanged->Failed, got: {ev2:?}"
        );
    }
}
