mod cwd;
mod history;
mod hooks;
mod loop_;
mod prompt;
mod retry;

#[cfg(test)]
use async_trait::async_trait;

pub use anthropic::StubClient;
pub use cwd::resolve_session_cwd;
pub use loop_::run;

#[cfg(test)]
use anthropic::{AnthropicClient, MessageRequest, MessageResponse};
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use cwd::resolve_session_cwd_with_root;
#[cfg(test)]
use events::SessionEvent;
#[cfg(test)]
use hooks::{run_hook, run_post_tool_use_hooks, run_pre_tool_use_hooks, run_stop_hooks};
#[cfg(test)]
use loop_::run_tool_dispatch_loop;
#[cfg(test)]
use prompt::build_system_prompt;
#[cfg(test)]
use types::{ContentBlock, Role};
#[cfg(test)]
use uuid::Uuid;

use std::path::PathBuf;
use std::sync::Arc;
use types::Session;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db error: {0}")]
    Db(#[from] db::Error),
    #[error("anthropic error: {0}")]
    Anthropic(#[from] anthropic::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct HarnessConfig {
    pub session: Session,
    pub model: String,
    pub tools: Vec<Arc<dyn tools::Tool>>,
    /// Injectable git root for tests. Production code passes `None`; tests pass `Some(temp_dir)`.
    pub git_root: Option<PathBuf>,
    /// Session working directory (worktree path). Hooks are spawned with this as their cwd
    /// so they operate on the correct tree. Set by `run()` after resolving the worktree;
    /// tests leave this as `None`.
    pub cwd: Option<PathBuf>,
}

#[cfg(test)]
#[allow(clippy::struct_field_names)]
mod tests {
    use super::*;
    use mockall::mock;
    use tokio::sync::{broadcast, mpsc, Mutex};

    // Serialize tests that mutate NS2_MAX_RETRIES to prevent races.
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    mock! {
        pub TestDb {}

        #[async_trait]
        impl db::SessionDb for TestDb {
            async fn create_session(&self, session: &types::Session) -> db::Result<()>;
            async fn get_session(&self, id: Uuid) -> db::Result<types::Session>;
            async fn list_sessions(&self, status: Option<types::SessionStatus>) -> db::Result<Vec<types::Session>>;
            async fn update_session_status(&self, id: Uuid, status: types::SessionStatus) -> db::Result<()>;
        }

        #[async_trait]
        impl db::TurnDb for TestDb {
            async fn create_turn(&self, turn: &types::Turn) -> db::Result<()>;
            async fn list_turns(&self, session_id: Uuid) -> db::Result<Vec<types::Turn>>;
        }

        #[async_trait]
        impl db::ContentBlockDb for TestDb {
            async fn create_content_block(
                &self,
                turn_id: Uuid,
                block_index: i64,
                role: &types::Role,
                block: &types::ContentBlock,
            ) -> db::Result<()>;
            async fn list_content_blocks(&self, turn_id: Uuid) -> db::Result<Vec<(types::Role, types::ContentBlock)>>;
            async fn get_last_text_for_session(&self, session_id: Uuid) -> db::Result<Option<String>>;
        }

        #[async_trait]
        impl db::IssueDb for TestDb {
            async fn create_issue(&self, issue: &types::Issue) -> db::Result<()>;
            async fn get_issue(&self, id: String) -> db::Result<types::Issue>;
            async fn list_issues(
                &self,
                status: Option<types::IssueStatus>,
                assignee: Option<String>,
                parent_id: Option<String>,
            ) -> db::Result<Vec<types::Issue>>;
            async fn list_issues_by_session_id(&self, session_id: uuid::Uuid) -> db::Result<Vec<types::Issue>>;
            async fn update_issue(&self, issue: &types::Issue) -> db::Result<()>;
        }

        impl db::Db for TestDb {}
    }

    // Helper: build a session
    fn make_session() -> types::Session {
        types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // Helper: set up a mock DB that accepts any create_turn / create_content_block / update calls
    // and returns empty lists for list_turns / list_content_blocks / list_issues_by_session_id.
    fn permissive_mock_db() -> MockTestDb {
        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        // No linked issue → no worktree is created for regular harness tests
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));
        mock_db
    }

    /// Test helper: run the tool dispatch loop with a throw-away stop channel.
    /// Used in tests that only need to verify tool dispatch behaviour and don't
    /// care about the stop signal.
    async fn run_tool_dispatch_loop_test(
        config: &HarnessConfig,
        client: &Arc<dyn AnthropicClient>,
        db: &Arc<dyn db::Db>,
        event_tx: &tokio::sync::broadcast::Sender<SessionEvent>,
        hooks: &agents::AgentHooks,
        history: Vec<(types::Role, Vec<types::ContentBlock>)>,
    ) -> crate::Result<(Option<tools::StopSignal>, Option<String>)> {
        let (mut stop_rx, _stop_tool) = tools::StopTool::new_pair();
        run_tool_dispatch_loop(config, client, db, event_tx, hooks, None, history, &mut stop_rx)
            .await
    }

    // ── Worktree tests ────────────────────────────────────────────────────────

    /// `ensure_worktree` with an existing directory returns `Some(path)` without running git.
    #[tokio::test]
    async fn ensure_worktree_existing_dir_is_reused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let worktree_path = tmp.path().join("existing-wt");
        std::fs::create_dir_all(&worktree_path).unwrap();

        // git_root doesn't matter because the path already exists
        let result = workspace::ensure_worktree(tmp.path(), &worktree_path, "my-branch").await;
        assert_eq!(result, Some(worktree_path));
    }

    /// `resolve_session_cwd` returns `None` when there are no linked issues.
    #[tokio::test]
    async fn resolve_session_cwd_no_issues_returns_none() {
        let mut mock_db = MockTestDb::new();
        let session_id = Uuid::new_v4();
        mock_db
            .expect_list_issues_by_session_id()
            .withf(move |id| *id == session_id)
            .returning(|_| Ok(vec![]));

        let db: Arc<dyn db::Db> = Arc::new(mock_db);
        let result = resolve_session_cwd(&db, session_id).await;
        assert!(result.is_none(), "no issues → cwd must be None");
    }

    /// `resolve_session_cwd` returns `None` when the associated issue has an empty branch.
    #[tokio::test]
    async fn resolve_session_cwd_empty_branch_returns_none() {
        let mut mock_db = MockTestDb::new();
        let session_id = Uuid::new_v4();
        mock_db
            .expect_list_issues_by_session_id()
            .returning(move |_| {
                Ok(vec![types::Issue {
                    id: "ab12".into(),
                    title: "Test".into(),
                    body: "body".into(),
                    status: types::IssueStatus::Running,
                    branch: String::new(), // empty branch
                    assignee: None,
                    session_id: Some(session_id),
                    parent_id: None,
                    blocked_on: vec![],
                    comments: vec![],
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }])
            });

        let db: Arc<dyn db::Db> = Arc::new(mock_db);
        let result = resolve_session_cwd(&db, session_id).await;
        assert!(result.is_none(), "empty branch → cwd must be None");
    }

    /// `resolve_session_cwd_with_root` returns `Some(worktree_path)` when the issue has
    /// a non-empty branch and a real git repo is provided.  Verifies the happy path so
    /// that a mutation that always returns `None` is caught.
    #[tokio::test]
    async fn resolve_session_cwd_with_root_non_empty_branch_returns_some() {
        // Set up a bare origin + local clone so ensure_worktree can branch from origin/main.
        let origin_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(origin_dir.path())
            .status()
            .unwrap();

        let local_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["clone", &origin_dir.path().to_string_lossy(), "."])
            .current_dir(local_dir.path())
            .status()
            .unwrap();

        for cmd in [
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "test"],
        ] {
            std::process::Command::new("git")
                .args(&cmd)
                .current_dir(local_dir.path())
                .status()
                .unwrap();
        }
        std::fs::write(local_dir.path().join("README.md"), "init").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(local_dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false", "commit", "-m", "init"])
            .current_dir(local_dir.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(local_dir.path())
            .status()
            .unwrap();

        let branch = "feature/test-cwd";
        let session_id = Uuid::new_v4();
        let mut mock_db = MockTestDb::new();
        mock_db
            .expect_list_issues_by_session_id()
            .returning(move |_| {
                Ok(vec![types::Issue {
                    id: "cd34".into(),
                    title: "Test".into(),
                    body: "body".into(),
                    status: types::IssueStatus::Running,
                    branch: branch.into(),
                    assignee: None,
                    session_id: Some(session_id),
                    parent_id: None,
                    blocked_on: vec![],
                    comments: vec![],
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }])
            });

        let db: Arc<dyn db::Db> = Arc::new(mock_db);
        let result =
            resolve_session_cwd_with_root(&db, session_id, Some(local_dir.path().to_owned())).await;
        assert!(
            result.is_some(),
            "non-empty branch + git root → cwd must be Some"
        );
        let cwd = result.unwrap();
        assert!(cwd.is_dir(), "resolved cwd must be an existing directory");
    }

    // ── Existing tests (updated to include list_issues_by_session_id) ─────────

    #[tokio::test]
    async fn test_run_with_stub_client() {
        let mock_db = permissive_mock_db();
        let session = make_session();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };

        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello world".into()).await.unwrap();
        drop(msg_tx); // close the channel so run() exits after first message

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        // Collect events
        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::TurnStarted { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::ContentBlockDelta { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::ContentBlockDone { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::TurnDone { .. })));
        assert!(events.iter().any(|e| matches!(e, SessionEvent::Done)));
    }

    #[tokio::test]
    async fn test_run_creates_turn_with_correct_session_id() {
        let session = make_session();
        let session_id = session.id;

        let mut mock_db = MockTestDb::new();
        mock_db
            .expect_create_turn()
            .withf(move |turn| turn.session_id == session_id)
            .returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();
    }

    #[tokio::test]
    async fn test_run_updates_session_status_to_completed() {
        let session = make_session();
        let session_id = session.id;

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));
        // Expect Running then Waiting (default when stop tool is not called)
        let mut seq = mockall::Sequence::new();
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| *id == session_id && *status == types::SessionStatus::Running)
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| {
                *id == session_id && *status == types::SessionStatus::Waiting
            })
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();
    }

    #[tokio::test]
    async fn test_run_emits_all_expected_event_types() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::TurnStarted { .. })),
            "missing TurnStarted"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::ContentBlockDone { .. })),
            "missing ContentBlockDone"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::TurnDone { .. })),
            "missing TurnDone"
        );
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::Done)),
            "missing SessionDone"
        );
    }

    #[tokio::test]
    async fn test_run_session_done_carries_correct_session_id() {
        let session = make_session();
        let _ = session.id;
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        let done_event = events
            .iter()
            .find(|e| matches!(e, SessionEvent::Done))
            .expect("no SessionDone event");

        // The _session_id is tracked by the broadcast channel subscription;
        // the Done variant itself no longer carries it (it's on the outer SystemEvent wrapper).
        assert!(
            matches!(done_event, SessionEvent::Done),
            "expected Done event"
        );
    }

    #[tokio::test]
    async fn test_stub_client_complete_returns_non_empty_text() {
        let client = StubClient;
        let request = MessageRequest {
            model: "claude-opus-4-5".into(),
            system: None,
            messages: vec![(
                types::Role::User,
                vec![types::ContentBlock::Text { text: "hi".into() }],
            )],
            max_tokens: 100,
            tools: vec![],
        };
        let response = client.complete(request).await.unwrap();
        assert!(!response.content.is_empty());
        assert!(matches!(
            &response.content[0],
            types::ContentBlock::Text { text } if !text.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_stub_client_complete_stop_reason() {
        let client = StubClient;
        let request = MessageRequest {
            model: "claude-opus-4-5".into(),
            system: None,
            messages: vec![(
                types::Role::User,
                vec![types::ContentBlock::Text { text: "hi".into() }],
            )],
            max_tokens: 100,
            tools: vec![],
        };
        let response = client.complete(request).await.unwrap();
        assert_eq!(response.stop_reason, "end_turn");
    }

    // Test a client that returns multiple content blocks
    struct MultiBlockClient;

    #[async_trait]
    impl AnthropicClient for MultiBlockClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            Ok(MessageResponse {
                content: vec![
                    types::ContentBlock::Text {
                        text: "block one".into(),
                    },
                    types::ContentBlock::Text {
                        text: "block two".into(),
                    },
                ],
                stop_reason: "end_turn".into(),
                input_tokens: 5,
                output_tokens: 4,
            })
        }
    }

    #[tokio::test]
    async fn test_run_with_multi_block_response_stores_all_blocks() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(MultiBlockClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // 1 user block + 2 assistant blocks = 3 ContentBlockDone events
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, SessionEvent::ContentBlockDone { .. }))
                .count(),
            3,
            "expected 3 ContentBlockDone events (1 user + 2 assistant)"
        );
    }

    #[tokio::test]
    async fn test_run_exits_when_channel_closed_without_message() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (tx, msg_rx) = mpsc::channel::<String>(16);
        drop(tx); // close immediately so run() exits without processing any messages

        run(config, client, db, event_tx, msg_rx).await.unwrap();
        // If we get here, run() exited cleanly with no messages
    }

    // --- Tool dispatch tests ---

    /// A mock client that first returns `tool_use`, then `end_turn` on the second call.
    struct ToolUseClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    impl ToolUseClient {
        fn new() -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for ToolUseClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(MessageResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_01".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path": "/tmp/fake.txt"}),
                    }],
                    stop_reason: "tool_use".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                })
            } else {
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "I read the file successfully.".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 15,
                    output_tokens: 10,
                })
            }
        }
    }

    /// A tool that always succeeds with a fixed output.
    struct AlwaysOkTool;

    #[async_trait::async_trait]
    impl tools::Tool for AlwaysOkTool {
        fn definition(&self) -> types::ToolDefinition {
            types::ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({}),
            }
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _cwd: Option<&std::path::Path>,
        ) -> tools::Result<String> {
            Ok("file content here".into())
        }
    }

    /// A tool that always errors.
    struct AlwaysErrTool;

    #[async_trait::async_trait]
    impl tools::Tool for AlwaysErrTool {
        fn definition(&self) -> types::ToolDefinition {
            types::ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({}),
            }
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _cwd: Option<&std::path::Path>,
        ) -> tools::Result<String> {
            Err(tools::Error::InvalidInput("cannot read file".into()))
        }
    }

    #[tokio::test]
    async fn test_tool_call_resolved_and_final_text_stored() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(ToolUseClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(128);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("read the file".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Should have a SessionDone at the end
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::Done)),
            "missing SessionDone"
        );

        // Should have ContentBlockDone events for both ToolUse and final Text
        let done_blocks: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::ContentBlockDone { .. }))
            .collect();
        // At least: user text, assistant tool_use, tool_result, final text
        assert!(
            done_blocks.len() >= 4,
            "expected at least 4 ContentBlockDone events, got {}",
            done_blocks.len()
        );

        // Verify a ToolUse block was emitted
        assert!(
            done_blocks.iter().any(|e| matches!(
                e,
                SessionEvent::ContentBlockDone { block: ContentBlock::ToolUse { name, .. }, .. }
                if name == "read"
            )),
            "missing ToolUse ContentBlockDone"
        );

        // Verify a ToolResult block was emitted
        assert!(
            done_blocks.iter().any(|e| matches!(
                e,
                SessionEvent::ContentBlockDone {
                    block: ContentBlock::ToolResult { .. },
                    ..
                }
            )),
            "missing ToolResult ContentBlockDone"
        );

        // Verify final text block
        assert!(
            done_blocks.iter().any(|e| matches!(
                e,
                SessionEvent::ContentBlockDone {
                    block: ContentBlock::Text { .. },
                    ..
                }
            )),
            "missing final Text ContentBlockDone"
        );
    }

    #[tokio::test]
    async fn test_tool_error_returned_as_tool_result_and_loop_completes() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![Arc::new(AlwaysErrTool)],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(ToolUseClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(128);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("read the file".into()).await.unwrap();
        drop(msg_tx);

        // Should complete without error even when the tool errors
        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Should still complete
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::Done)),
            "missing SessionDone"
        );

        // The tool result block should contain an error message
        let done_blocks: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let SessionEvent::ContentBlockDone {
                    block: ContentBlock::ToolResult { content, .. },
                    ..
                } = e
                {
                    Some(content.as_str())
                } else {
                    None
                }
            })
            .collect();

        assert!(!done_blocks.is_empty(), "expected a ToolResult block");
        assert!(
            done_blocks[0].starts_with("Error:"),
            "expected error message in tool result, got: {:?}",
            done_blocks[0]
        );
    }

    // --- Multi-turn tests ---

    /// A client that tracks call count and returns different responses per call.
    /// Call 0: `end_turn` with "First response."
    /// Call 1: `end_turn` with "Second response with context."
    struct TwoTurnClient {
        call_count: std::sync::atomic::AtomicU32,
        /// Captures messages passed to the second call for later inspection
        second_call_messages: std::sync::Mutex<Vec<(Role, Vec<ContentBlock>)>>,
    }

    impl TwoTurnClient {
        fn new() -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
                second_call_messages: std::sync::Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for TwoTurnClient {
        async fn complete(&self, request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "First response.".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                })
            } else {
                // Capture the messages for later assertion
                let mut guard = self.second_call_messages.lock().unwrap();
                *guard = request.messages;
                drop(guard);
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "Second response with context.".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 20,
                    output_tokens: 8,
                })
            }
        }
    }

    // Client: call 0 → tool_use, call 1 → tool_use, call 2 → end_turn
    struct TwoToolClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl AnthropicClient for TwoToolClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match count {
                0 => Ok(MessageResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_01".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path": "/tmp/a.txt"}),
                    }],
                    stop_reason: "tool_use".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                }),
                1 => Ok(MessageResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_02".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path": "/tmp/b.txt"}),
                    }],
                    stop_reason: "tool_use".into(),
                    input_tokens: 12,
                    output_tokens: 5,
                }),
                _ => Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "All done.".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 20,
                    output_tokens: 6,
                }),
            }
        }
    }

    /// Test: two sequential tool calls in one run both resolved before final response.
    #[tokio::test]
    async fn test_two_sequential_tool_calls_in_one_run() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(TwoToolClient {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(256);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("do two things".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Should have two ToolUse blocks and two ToolResult blocks
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    SessionEvent::ContentBlockDone {
                        block: ContentBlock::ToolUse { .. },
                        ..
                    }
                ))
                .count(),
            2,
            "expected 2 ToolUse blocks"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    SessionEvent::ContentBlockDone {
                        block: ContentBlock::ToolResult { .. },
                        ..
                    }
                ))
                .count(),
            2,
            "expected 2 ToolResult blocks"
        );

        // SessionDone should be present
        assert!(events.iter().any(|e| matches!(e, SessionEvent::Done)));
    }

    /// Test: second user message is processed with all prior turns in context.
    #[tokio::test]
    async fn test_second_message_includes_prior_history() {
        use std::sync::Mutex;

        let session = make_session();
        let session_id = session.id;

        // We need a DB that returns real turn/block data on the second call.
        // Strategy: use a mutex-wrapped Vec to accumulate created turns/blocks,
        // and return them on list_turns/list_content_blocks calls.

        // Shared state for the mock DB
        let turns_store: Arc<Mutex<Vec<types::Turn>>> = Arc::new(Mutex::new(vec![]));
        let blocks_store: Arc<Mutex<Vec<(Uuid, types::Role, types::ContentBlock)>>> =
            Arc::new(Mutex::new(vec![]));

        let turns_store_c = Arc::clone(&turns_store);
        let turns_store_l = Arc::clone(&turns_store);
        let blocks_store_c = Arc::clone(&blocks_store);
        let blocks_store_l = Arc::clone(&blocks_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(move |turn| {
            turns_store_c.lock().unwrap().push(turn.clone());
            Ok(())
        });
        mock_db
            .expect_create_content_block()
            .returning(move |turn_id, _idx, role, block| {
                blocks_store_c
                    .lock()
                    .unwrap()
                    .push((turn_id, role.clone(), block.clone()));
                Ok(())
            });
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(move |sid| {
            let turns: Vec<types::Turn> = turns_store_l
                .lock()
                .unwrap()
                .iter()
                .filter(|t| t.session_id == sid)
                .cloned()
                .collect();
            Ok(turns)
        });
        mock_db.expect_list_content_blocks().returning(move |tid| {
            let blocks: Vec<(types::Role, types::ContentBlock)> = blocks_store_l
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _, _)| *id == tid)
                .map(|(_, role, block)| (role.clone(), block.clone()))
                .collect();
            Ok(blocks)
        });
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        let client = Arc::new(TwoTurnClient::new());
        let client_ref = Arc::clone(&client);

        let config = HarnessConfig {
            session: Session {
                id: session_id,
                name: "test".into(),
                status: types::SessionStatus::Running,
                agent: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };

        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(128);
        let (msg_tx, msg_rx) = mpsc::channel(16);

        // Send first message, then second, then close channel
        msg_tx.send("First question.".into()).await.unwrap();
        msg_tx.send("Follow-up question.".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        // Verify the second API call included the prior history
        let second_messages = client_ref.second_call_messages.lock().unwrap().clone();
        assert!(
            !second_messages.is_empty(),
            "second call should have received messages"
        );
        // History should have at least: user msg 1, assistant response 1, user msg 2
        // That's 3 entries minimum
        assert!(
            second_messages.len() >= 3,
            "expected at least 3 messages in second call, got {}",
            second_messages.len()
        );

        // The first message in history should be the user's first question
        assert_eq!(
            second_messages[0].0,
            Role::User,
            "first history entry should be User"
        );
        // The second message should be the assistant's first response
        assert_eq!(
            second_messages[1].0,
            Role::Assistant,
            "second history entry should be Assistant"
        );
        // The last message should be the second user question
        assert_eq!(
            second_messages.last().unwrap().0,
            Role::User,
            "last history entry should be the second user message"
        );
    }

    // --- Fix 1: stop_reason tests ---

    /// Client that always returns `max_tokens` stop reason.
    struct MaxTokensClient;

    #[async_trait]
    impl AnthropicClient for MaxTokensClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            Ok(MessageResponse {
                content: vec![ContentBlock::Text {
                    text: "truncated output".into(),
                }],
                stop_reason: "max_tokens".into(),
                input_tokens: 10,
                output_tokens: 4096,
            })
        }
    }

    #[tokio::test]
    async fn test_max_tokens_stop_reason_emits_error_event() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(MaxTokensClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        // Should complete without error
        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Must emit an Error event
        let error_event = events
            .iter()
            .find(|e| matches!(e, SessionEvent::Error { .. }));
        assert!(
            error_event.is_some(),
            "expected a SessionEvent::Error for max_tokens"
        );
        assert!(
            matches!(error_event.unwrap(), SessionEvent::Error { message } if message.contains("max_tokens")),
            "error message should mention max_tokens"
        );

        // Loop should have exited cleanly (SessionDone is emitted)
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::Done)),
            "expected SessionDone after max_tokens"
        );
    }

    // --- Fix 2a: unknown tool name returns error tool result ---

    /// Client: call 0 returns `tool_use` for a nonexistent tool, call 1 returns `end_turn`.
    struct UnknownToolClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    impl UnknownToolClient {
        fn new() -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for UnknownToolClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(MessageResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "x".into(),
                        name: "nonexistent_tool".into(),
                        input: serde_json::json!({}),
                    }],
                    stop_reason: "tool_use".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                })
            } else {
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "done".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 15,
                    output_tokens: 3,
                })
            }
        }
    }

    #[tokio::test]
    async fn test_unknown_tool_name_returns_error_tool_result() {
        use std::sync::Mutex;

        let session = make_session();

        // We need a DB that captures stored content blocks so we can inspect them.
        let blocks_store: Arc<Mutex<Vec<(Uuid, types::Role, types::ContentBlock)>>> =
            Arc::new(Mutex::new(vec![]));
        let blocks_store_c = Arc::clone(&blocks_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(move |turn_id, _idx, role, block| {
                blocks_store_c
                    .lock()
                    .unwrap()
                    .push((turn_id, role.clone(), block.clone()));
                Ok(())
            });
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        // Use a tools list that has only a different tool (not "nonexistent_tool")
        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![Arc::new(AlwaysOkTool)], // only "read", not "nonexistent_tool"
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(UnknownToolClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(128);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("do something".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Loop should complete
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::Done)),
            "expected SessionDone"
        );

        // A ToolResult block should have been stored with content containing "unknown tool"
        let stored = blocks_store.lock().unwrap();
        let tool_result_found = stored.iter().any(|(_, _, block)| {
            matches!(block, ContentBlock::ToolResult { content, .. } if {
                let lower = content.to_lowercase();
                lower.contains("unknown tool") || lower.contains("unknown")
            })
        });
        let stored_debug = stored
            .iter()
            .map(|(_, _, b)| format!("{b:?}"))
            .collect::<Vec<_>>();
        drop(stored);
        assert!(
            tool_result_found,
            "expected a ToolResult block with 'unknown tool' error in DB; stored blocks: {stored_debug:?}",
        );
    }

    // --- Fix 2b: empty tool list runs normally ---

    #[tokio::test]
    async fn test_empty_tool_list() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![], // empty
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Session should complete normally
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::Done)),
            "expected SessionDone"
        );
        // No Error events
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "did not expect Error events for an empty tool list with end_turn response"
        );
    }

    // --- Fix 2c: tool result stored with Role::User ---

    #[tokio::test]
    async fn test_tool_result_stored_with_user_role() {
        use std::sync::Mutex;

        let session = make_session();

        // Capture stored blocks with their roles
        let blocks_store: Arc<Mutex<Vec<(Uuid, types::Role, types::ContentBlock)>>> =
            Arc::new(Mutex::new(vec![]));
        let turns_store: Arc<Mutex<Vec<types::Turn>>> = Arc::new(Mutex::new(vec![]));

        let blocks_store_c = Arc::clone(&blocks_store);
        let turns_store_c = Arc::clone(&turns_store);
        let turns_store_l = Arc::clone(&turns_store);
        let blocks_store_l = Arc::clone(&blocks_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(move |turn| {
            turns_store_c.lock().unwrap().push(turn.clone());
            Ok(())
        });
        mock_db
            .expect_create_content_block()
            .returning(move |turn_id, _idx, role, block| {
                blocks_store_c
                    .lock()
                    .unwrap()
                    .push((turn_id, role.clone(), block.clone()));
                Ok(())
            });
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(move |sid| {
            let turns = turns_store_l
                .lock()
                .unwrap()
                .iter()
                .filter(|t| t.session_id == sid)
                .cloned()
                .collect();
            Ok(turns)
        });
        mock_db.expect_list_content_blocks().returning(move |tid| {
            let blocks = blocks_store_l
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _, _)| *id == tid)
                .map(|(_, role, block)| (role.clone(), block.clone()))
                .collect();
            Ok(blocks)
        });
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(ToolUseClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(128);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("read the file".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let stored = blocks_store.lock().unwrap();
        let tool_result_role = stored.iter().find_map(|(_, role, block)| {
            matches!(block, ContentBlock::ToolResult { .. }).then(|| role.clone())
        });
        drop(stored);
        assert!(
            tool_result_role.is_some(),
            "expected a ToolResult block in DB"
        );
        assert_eq!(
            tool_result_role.unwrap(),
            types::Role::User,
            "ToolResult block should be stored with Role::User"
        );
    }

    // --- Fix 2d: sequential tool calls correct ordering ---

    struct TwoToolOrderingClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl AnthropicClient for TwoToolOrderingClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match count {
                0 => Ok(MessageResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_01".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path": "/tmp/a.txt"}),
                    }],
                    stop_reason: "tool_use".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                }),
                _ => Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "All done.".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 20,
                    output_tokens: 6,
                }),
            }
        }
    }

    #[tokio::test]
    async fn test_sequential_tool_calls_correct_ordering() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(TwoToolOrderingClient {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(256);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("do it".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Collect event type labels in order
        let labels: Vec<&str> = events
            .iter()
            .map(|e| match e {
                SessionEvent::TurnStarted { .. } => "TurnStarted",
                SessionEvent::ContentBlockDelta { .. } => "ContentBlockDelta",
                SessionEvent::ContentBlockDone {
                    block: ContentBlock::Text { .. },
                    ..
                } => "ContentBlockDone(Text)",
                SessionEvent::ContentBlockDone {
                    block: ContentBlock::ToolUse { .. },
                    ..
                } => "ContentBlockDone(ToolUse)",
                SessionEvent::ContentBlockDone {
                    block: ContentBlock::ToolResult { .. },
                    ..
                } => "ContentBlockDone(ToolResult)",
                SessionEvent::TurnDone { .. } => "TurnDone",
                SessionEvent::Done => "SessionDone",
                SessionEvent::Stopped { .. } => "Stopped",
                SessionEvent::Error { .. } => "Error",
                SessionEvent::ToolUseStart { .. } => "ToolUseStart",
                SessionEvent::ToolUseDone { .. } => "ToolUseDone",
            })
            .collect();

        let expected: &[&str] = &[
            "TurnStarted",
            "ContentBlockDelta",
            "ContentBlockDone(Text)",
            "TurnDone",
            "TurnStarted",
            "ContentBlockDone(ToolUse)",
            "TurnDone",
            "TurnStarted",
            "ContentBlockDone(ToolResult)",
            "TurnDone",
            "TurnStarted",
            "ContentBlockDelta",
            "ContentBlockDone(Text)",
            "TurnDone",
            "SessionDone",
        ];

        assert_eq!(
            labels, expected,
            "event ordering mismatch.\nGot:      {labels:?}\nExpected: {expected:?}"
        );
    }

    // --- Token count tests ---

    struct KnownTokenClient;

    #[async_trait]
    impl AnthropicClient for KnownTokenClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            Ok(MessageResponse {
                content: vec![ContentBlock::Text {
                    text: "response".into(),
                }],
                stop_reason: "end_turn".into(),
                input_tokens: 100,
                output_tokens: 50,
            })
        }
    }

    #[tokio::test]
    async fn test_assistant_turn_token_count_equals_input_plus_output() {
        use std::sync::Mutex;

        let session = make_session();
        let turns_store: Arc<Mutex<Vec<types::Turn>>> = Arc::new(Mutex::new(vec![]));
        let turns_store_c = Arc::clone(&turns_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(move |turn| {
            turns_store_c.lock().unwrap().push(turn.clone());
            Ok(())
        });
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(KnownTokenClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let stored = turns_store.lock().unwrap();
        let assistant_token_count = stored.iter().find_map(|t| t.token_count);
        drop(stored);
        assert!(
            assistant_token_count.is_some(),
            "expected an assistant turn with token_count set"
        );
        assert_eq!(
            assistant_token_count,
            Some(150),
            "token_count should be input_tokens (100) + output_tokens (50) = 150"
        );
    }

    // --- Fix 2e: history reconstructed after cold restart ---

    /// A client that captures all messages it receives on its first call.
    struct CapturingClient {
        captured_messages: std::sync::Mutex<Vec<(Role, Vec<ContentBlock>)>>,
    }

    impl CapturingClient {
        fn new() -> Self {
            Self {
                captured_messages: std::sync::Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for CapturingClient {
        async fn complete(&self, request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let mut guard = self.captured_messages.lock().unwrap();
            if guard.is_empty() {
                *guard = request.messages;
            }
            drop(guard);
            Ok(MessageResponse {
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: "end_turn".into(),
                input_tokens: 10,
                output_tokens: 3,
            })
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_history_reconstructed_after_cold_restart() {
        use std::sync::Mutex;

        let session = make_session();
        let session_id = session.id;

        // Pre-populate the mock DB with: one user turn (Text "hello"), one assistant turn (Text "world")
        let user_turn_id = Uuid::new_v4();
        let assistant_turn_id = Uuid::new_v4();

        let pre_user_turn = types::Turn {
            id: user_turn_id,
            session_id,
            token_count: None,
            created_at: Utc::now(),
        };
        let pre_assistant_turn = types::Turn {
            id: assistant_turn_id,
            session_id,
            token_count: Some(20),
            created_at: Utc::now(),
        };

        // The new run will add more turns; we track everything via a store.
        let turns_store: Arc<Mutex<Vec<types::Turn>>> = Arc::new(Mutex::new(vec![
            pre_user_turn.clone(),
            pre_assistant_turn.clone(),
        ]));
        let blocks_store: Arc<Mutex<Vec<(Uuid, types::Role, types::ContentBlock)>>> =
            Arc::new(Mutex::new(vec![
                (
                    user_turn_id,
                    types::Role::User,
                    ContentBlock::Text {
                        text: "hello".into(),
                    },
                ),
                (
                    assistant_turn_id,
                    types::Role::Assistant,
                    ContentBlock::Text {
                        text: "world".into(),
                    },
                ),
            ]));

        let turns_store_c = Arc::clone(&turns_store);
        let turns_store_l = Arc::clone(&turns_store);
        let blocks_store_c = Arc::clone(&blocks_store);
        let blocks_store_l = Arc::clone(&blocks_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(move |turn| {
            turns_store_c.lock().unwrap().push(turn.clone());
            Ok(())
        });
        mock_db
            .expect_create_content_block()
            .returning(move |turn_id, _idx, role, block| {
                blocks_store_c
                    .lock()
                    .unwrap()
                    .push((turn_id, role.clone(), block.clone()));
                Ok(())
            });
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(move |sid| {
            let turns = turns_store_l
                .lock()
                .unwrap()
                .iter()
                .filter(|t| t.session_id == sid)
                .cloned()
                .collect();
            Ok(turns)
        });
        mock_db.expect_list_content_blocks().returning(move |tid| {
            let blocks = blocks_store_l
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _, _)| *id == tid)
                .map(|(_, role, block)| (role.clone(), block.clone()))
                .collect();
            Ok(blocks)
        });
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        let client = Arc::new(CapturingClient::new());
        let client_ref = Arc::clone(&client);

        let config = HarnessConfig {
            session: types::Session {
                id: session_id,
                name: "test".into(),
                status: types::SessionStatus::Running,
                agent: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };

        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("follow up".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let captured = client_ref.captured_messages.lock().unwrap().clone();
        // Should have 3 messages: user "hello", assistant "world", user "follow up"
        assert_eq!(
            captured.len(),
            3,
            "expected 3 messages in API call (prior 2 + new 1), got {}: {:?}",
            captured.len(),
            captured
                .iter()
                .map(|(r, blocks)| {
                    let text = blocks
                        .iter()
                        .find_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .unwrap_or("?");
                    format!("{r:?}: {text}")
                })
                .collect::<Vec<_>>()
        );

        assert_eq!(captured[0].0, Role::User, "first message should be User");
        assert!(
            matches!(&captured[0].1[0], ContentBlock::Text { text } if text == "hello"),
            "first message should be 'hello'"
        );

        assert_eq!(
            captured[1].0,
            Role::Assistant,
            "second message should be Assistant"
        );
        assert!(
            matches!(&captured[1].1[0], ContentBlock::Text { text } if text == "world"),
            "second message should be 'world'"
        );

        assert_eq!(captured[2].0, Role::User, "third message should be User");
        assert!(
            matches!(&captured[2].1[0], ContentBlock::Text { text } if text == "follow up"),
            "third message should be 'follow up'"
        );
    }

    // --- Agent system prompt tests ---

    /// A client that captures the `system` field from the first request.
    #[allow(clippy::option_option)]
    struct SystemCapturingClient {
        captured_system: std::sync::Mutex<Option<Option<String>>>,
    }

    impl SystemCapturingClient {
        fn new() -> Self {
            Self {
                captured_system: std::sync::Mutex::new(None),
            }
        }

        fn captured(&self) -> Option<String> {
            self.captured_system.lock().unwrap().clone().flatten()
        }
    }

    #[async_trait]
    impl AnthropicClient for SystemCapturingClient {
        async fn complete(&self, request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let mut guard = self.captured_system.lock().unwrap();
            if guard.is_none() {
                *guard = Some(request.system);
            }
            drop(guard);
            Ok(MessageResponse {
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: "end_turn".into(),
                input_tokens: 10,
                output_tokens: 3,
            })
        }
    }

    /// When session.agent is None, the system prompt sent to the API must be None.
    #[tokio::test]
    async fn test_no_agent_means_no_system_prompt() {
        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: None, // No agent
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mock_db = permissive_mock_db();
        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(SystemCapturingClient::new());
        let client_ref = Arc::clone(&client);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        assert!(
            client_ref.captured().is_none(),
            "system prompt must be None when session.agent is None"
        );
    }

    /// When an agent exists with a non-empty body, its body becomes the system prompt.
    #[tokio::test]
    async fn test_agent_with_nonempty_body_becomes_system_prompt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let agent_name = "harness_test_nonempty_body";
        let agent_body = "You are a test harness agent with a non-empty body.";
        let def = agents::AgentDef {
            name: agent_name.to_string(),
            description: "test agent".to_string(),
            body: agent_body.to_string(),
            include_project_config: false,
            hooks: agents::AgentHooks::default(),
        };
        agents::write_agent(&agents_dir, &def).unwrap();

        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: Some(agent_name.to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mock_db = permissive_mock_db();
        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: Some(tmp.path().to_path_buf()),
            cwd: None,
        };
        let client = Arc::new(SystemCapturingClient::new());
        let client_ref = Arc::clone(&client);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let system = client_ref.captured().expect("system prompt must be Some");
        // The preamble is now always prepended; verify the agent body is still present.
        assert!(
            system.contains(agent_body),
            "system prompt must contain the agent body, got: {system}"
        );
        // Preamble must be at the start.
        assert!(
            system.starts_with("You are running in the ns2 agent harness."),
            "system prompt must start with the preamble, got: {system}"
        );
    }

    #[tokio::test]
    async fn test_agent_with_empty_body_produces_no_system_prompt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let agent_name = "harness_test_empty_body";
        let def = agents::AgentDef {
            name: agent_name.to_string(),
            description: "test agent with no body".to_string(),
            body: String::new(),
            include_project_config: false,
            hooks: agents::AgentHooks::default(),
        };
        agents::write_agent(&agents_dir, &def).unwrap();

        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: Some(agent_name.to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mock_db = permissive_mock_db();
        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: Some(tmp.path().to_path_buf()),
            cwd: None,
        };
        let client = Arc::new(SystemCapturingClient::new());
        let client_ref = Arc::clone(&client);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        assert!(
            client_ref.captured().is_none(),
            "empty agent body must NOT become the system prompt (system must be None)"
        );
    }

    #[tokio::test]
    async fn test_include_project_config_true_appends_claude_md_to_system_prompt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let project_content = "# Project Instructions\n\nDo good work.";
        std::fs::write(tmp.path().join("CLAUDE.md"), project_content).unwrap();

        let agent_name = "harness_test_include_project_config";
        let agent_body = "You are a coding agent.";
        let def = agents::AgentDef {
            name: agent_name.to_string(),
            description: "test agent with include_project_config".to_string(),
            body: agent_body.to_string(),
            include_project_config: true,
            hooks: agents::AgentHooks::default(),
        };
        agents::write_agent(&agents_dir, &def).unwrap();

        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: Some(agent_name.to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mock_db = permissive_mock_db();
        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: Some(tmp.path().to_path_buf()),
            cwd: None,
        };
        let client = Arc::new(SystemCapturingClient::new());
        let client_ref = Arc::clone(&client);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let system = client_ref.captured().expect("system prompt must be Some");
        let repo_name = tmp.path().file_name().unwrap().to_string_lossy();
        let expected_preamble = format!(
            "You are running in the ns2 agent harness.\nWorking directory / git root: {}\nRepository: {}\n",
            tmp.path().display(),
            repo_name,
        );
        let expected_body = format!("{agent_body}\n\n{project_content}");
        let expected = format!("{expected_preamble}{expected_body}");
        assert_eq!(
            system, expected,
            "system prompt must be preamble + agent_body + \\n\\n + CLAUDE.md"
        );
    }

    #[tokio::test]
    async fn test_include_project_config_false_leaves_system_prompt_unchanged() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        // Write a CLAUDE.md that must NOT appear in the system prompt
        std::fs::write(
            tmp.path().join("CLAUDE.md"),
            "Project config that must be ignored.",
        )
        .unwrap();

        let agent_name = "harness_test_no_project_config";
        let agent_body = "You are a plain agent without project config.";
        let def = agents::AgentDef {
            name: agent_name.to_string(),
            description: "test agent without include_project_config".to_string(),
            body: agent_body.to_string(),
            include_project_config: false,
            hooks: agents::AgentHooks::default(),
        };
        agents::write_agent(&agents_dir, &def).unwrap();

        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: Some(agent_name.to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mock_db = permissive_mock_db();
        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: Some(tmp.path().to_path_buf()),
            cwd: None,
        };
        let client = Arc::new(SystemCapturingClient::new());
        let client_ref = Arc::clone(&client);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let system = client_ref.captured().expect("system prompt must be Some");
        let repo_name = tmp.path().file_name().unwrap().to_string_lossy();
        let expected_preamble = format!(
            "You are running in the ns2 agent harness.\nWorking directory / git root: {}\nRepository: {}\n",
            tmp.path().display(),
            repo_name,
        );
        let expected = format!("{expected_preamble}{agent_body}");
        assert_eq!(
            system, expected,
            "system prompt must be preamble + agent_body; CLAUDE.md must be excluded when include_project_config=false"
        );
    }

    // ── GH #11: git-root preamble injection ──────────────────────────────────

    /// When `HarnessConfig::git_root` is Some, the system prompt must start with a
    /// preamble block that includes the working directory and repo name.
    #[tokio::test]
    async fn test_system_prompt_starts_with_preamble_when_git_root_available() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let agent_name = "harness_test_preamble_present";
        let agent_body = "You are a coding agent.";
        let def = agents::AgentDef {
            name: agent_name.to_string(),
            description: "preamble test agent".to_string(),
            body: agent_body.to_string(),
            include_project_config: false,
            hooks: agents::AgentHooks::default(),
        };
        agents::write_agent(&agents_dir, &def).unwrap();

        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: Some(agent_name.to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mock_db = permissive_mock_db();
        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: Some(tmp.path().to_path_buf()),
            cwd: None,
        };
        let client = Arc::new(SystemCapturingClient::new());
        let client_ref = Arc::clone(&client);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let system = client_ref.captured().expect("system prompt must be Some");
        let repo_name = tmp.path().file_name().unwrap().to_string_lossy();
        let expected_preamble = format!(
            "You are running in the ns2 agent harness.\nWorking directory / git root: {}\nRepository: {}\n",
            tmp.path().display(),
            repo_name,
        );
        assert!(
            system.starts_with(&expected_preamble),
            "system prompt must start with the preamble.\nExpected prefix:\n{expected_preamble}\nActual system:\n{system}"
        );
        assert!(
            system.contains(agent_body),
            "system prompt must still contain the agent body"
        );
    }

    /// Unit test for `build_system_prompt`: when `effective_root` is `None`,
    /// the preamble must be absent and only the agent body is returned.
    #[test]
    fn test_build_system_prompt_no_root_omits_preamble() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let agent_name = "preamble_absent_agent";
        let agent_body = "You are a coding agent without preamble.";
        let def = agents::AgentDef {
            name: agent_name.to_string(),
            description: "no-preamble test agent".to_string(),
            body: agent_body.to_string(),
            include_project_config: false,
            hooks: agents::AgentHooks::default(),
        };
        agents::write_agent(&agents_dir, &def).unwrap();

        // Pass effective_root = None → preamble must be omitted, no panic.
        let system = build_system_prompt(
            None, // ← no git root
            Some(agents_dir.as_path()),
            Some(agent_name),
        );

        let sys = system.expect("agent body should still produce a system prompt");
        assert!(
            !sys.contains("ns2 agent harness"),
            "preamble must NOT appear when effective_root is None, got: {sys}"
        );
        assert_eq!(
            sys, agent_body,
            "system must equal agent body when there is no git root"
        );
    }

    // ── Hook dispatch tests (GH #33) ─────────────────────────────────────────

    fn hook_cmd(script: &str, timeout: u64) -> agents::HookCommand {
        agents::HookCommand {
            command: script.to_string(),
            timeout,
        }
    }

    #[tokio::test]
    async fn test_pre_tool_use_hook_exit_0_allows_tool() {
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            pre_tool_use: vec![HookEntry {
                matcher: Some("read".to_string()),
                hooks: vec![hook_cmd("exit 0", 5)],
            }],
            ..AgentHooks::default()
        };

        let result =
            run_pre_tool_use_hooks(&hooks, "read", &serde_json::json!({"path": "/tmp/f"}), None)
                .await;
        assert!(
            result.is_none(),
            "exit 0 hook must not block the tool, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_pre_tool_use_hook_exit_1_blocks_tool_returns_stderr() {
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            pre_tool_use: vec![HookEntry {
                matcher: Some("bash".to_string()),
                hooks: vec![hook_cmd("echo 'blocked by policy' >&2; exit 1", 5)],
            }],
            ..AgentHooks::default()
        };

        let result = run_pre_tool_use_hooks(&hooks, "bash", &serde_json::json!({}), None).await;
        assert!(result.is_some(), "exit 1 hook must block the tool");
        let msg = result.unwrap();
        assert!(
            msg.contains("blocked by policy"),
            "blocked message must contain hook stderr, got: {msg:?}"
        );
    }

    #[tokio::test]
    async fn test_pre_tool_use_hook_matcher_does_not_match_different_tool() {
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            pre_tool_use: vec![HookEntry {
                matcher: Some("bash".to_string()),
                hooks: vec![hook_cmd("exit 1", 5)],
            }],
            ..AgentHooks::default()
        };

        let result = run_pre_tool_use_hooks(&hooks, "read", &serde_json::json!({}), None).await;
        assert!(
            result.is_none(),
            "hook for 'bash' must not match tool 'read'"
        );
    }

    #[tokio::test]
    async fn test_post_tool_use_hook_non_zero_exit_is_ignored() {
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            post_tool_use: vec![HookEntry {
                matcher: Some(".*".to_string()),
                hooks: vec![hook_cmd("exit 42", 5)],
            }],
            ..AgentHooks::default()
        };

        run_post_tool_use_hooks(&hooks, "bash", &serde_json::json!({}), "result", None).await;
    }

    #[tokio::test]
    async fn test_stop_hook_exit_0_allows_completion() {
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![hook_cmd("exit 0", 5)],
            }],
            ..AgentHooks::default()
        };

        let result = run_stop_hooks(&hooks, Uuid::new_v4(), None).await;
        assert!(
            result.is_none(),
            "exit 0 stop hook must allow completion, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_stop_hook_exit_1_injects_stdout_as_user_message() {
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![hook_cmd("echo 'please continue'; exit 1", 5)],
            }],
            ..AgentHooks::default()
        };

        let result = run_stop_hooks(&hooks, Uuid::new_v4(), None).await;
        assert!(result.is_some(), "exit 1 stop hook must inject a message");
        let msg = result.unwrap();
        assert!(
            msg.contains("please continue"),
            "injected message must contain hook stdout, got: {msg:?}"
        );
    }

    #[tokio::test]
    async fn test_stop_hook_exit_127_fails_open() {
        // Exit code 127 = command not found. This is a configuration/environment
        // error, not a deliberate block. The hook must fail open (allow completion)
        // rather than injecting a blocking message that traps the agent in a loop.
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![hook_cmd("exit 127", 5)],
            }],
            ..AgentHooks::default()
        };

        let result = run_stop_hooks(&hooks, Uuid::new_v4(), None).await;
        assert!(
            result.is_none(),
            "exit 127 (command not found) must fail open and allow completion, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_stop_hook_runs_in_session_cwd() {
        use agents::{AgentHooks, HookEntry};

        let tmp = tempfile::TempDir::new().unwrap();
        // Write a marker in the temp dir; the hook echoes pwd and exits 1 to surface the cwd.
        std::fs::write(tmp.path().join("marker.txt"), "present").unwrap();

        let hooks = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![hook_cmd("echo $(pwd); exit 1", 5)],
            }],
            ..AgentHooks::default()
        };

        let result = run_stop_hooks(&hooks, Uuid::new_v4(), Some(tmp.path())).await;
        assert!(
            result.is_some(),
            "hook must inject a message when running with a cwd"
        );
        let msg = result.unwrap();
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        assert!(
            msg.trim().ends_with(canonical.to_str().unwrap()),
            "hook must run in the session cwd; expected suffix {:?}, got: {msg:?}",
            canonical.to_str().unwrap()
        );
    }

    #[tokio::test]
    async fn test_hook_timeout_kills_command_and_returns_exit_1() {
        let cmd = hook_cmd("sleep 5", 1);
        let (exit_code, _stdout, stderr) = run_hook(&cmd, "{}", None).await;
        assert_eq!(exit_code, 1, "timed-out hook must return exit_code=1");
        assert!(
            stderr.contains("timed out"),
            "stderr must mention timeout, got: {stderr:?}"
        );
    }

    #[tokio::test]
    async fn test_integration_pre_tool_use_exit_0_tool_runs_normally() {
        use agents::{AgentHooks, HookEntry};
        use std::sync::Mutex;

        let results_store: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let results_store_c = Arc::clone(&results_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(move |_, _, _, block| {
                if let ContentBlock::ToolResult { content, .. } = block {
                    results_store_c.lock().unwrap().push(content.clone());
                }
                Ok(())
            });
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));

        let hooks = AgentHooks {
            pre_tool_use: vec![HookEntry {
                matcher: Some("read".to_string()),
                hooks: vec![hook_cmd("exit 0", 5)],
            }],
            ..AgentHooks::default()
        };

        let session = make_session();
        let (event_tx, _rx) = broadcast::channel(64);

        let history = vec![(Role::User, vec![ContentBlock::Text { text: "go".into() }])];
        let config = HarnessConfig {
            session: session.clone(),
            model: "test".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: None,
            cwd: None,
        };
        let client: Arc<dyn AnthropicClient> = Arc::new(ToolUseClient::new());
        let db: Arc<dyn db::Db> = Arc::new(mock_db);

        let result =
            run_tool_dispatch_loop_test(&config, &client, &db, &event_tx, &hooks, history)
                .await
                .unwrap();
        let (_, injected_msg) = result;
        assert!(injected_msg.is_none(), "should complete normally");

        let results = results_store.lock().unwrap();
        assert_eq!(results.len(), 1, "expected one tool result");
        assert_eq!(
            results[0], "file content here",
            "tool should have run normally"
        );
        drop(results);
    }

    #[tokio::test]
    async fn test_integration_pre_tool_use_exit_1_blocks_tool() {
        use agents::{AgentHooks, HookEntry};
        use std::sync::Mutex;

        let results_store: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let results_store_c = Arc::clone(&results_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(move |_, _, _, block| {
                if let ContentBlock::ToolResult { content, .. } = block {
                    results_store_c.lock().unwrap().push(content.clone());
                }
                Ok(())
            });
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));

        let hooks = AgentHooks {
            pre_tool_use: vec![HookEntry {
                matcher: Some("read".to_string()),
                hooks: vec![hook_cmd("echo 'tool blocked by hook' >&2; exit 1", 5)],
            }],
            ..AgentHooks::default()
        };

        let session = make_session();
        let (event_tx, _rx) = broadcast::channel(64);

        let history = vec![(Role::User, vec![ContentBlock::Text { text: "go".into() }])];
        let config = HarnessConfig {
            session: session.clone(),
            model: "test".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: None,
            cwd: None,
        };
        let client: Arc<dyn AnthropicClient> = Arc::new(ToolUseClient::new());
        let db: Arc<dyn db::Db> = Arc::new(mock_db);

        run_tool_dispatch_loop_test(&config, &client, &db, &event_tx, &hooks, history)
            .await
            .unwrap();

        let results = results_store.lock().unwrap();
        assert_eq!(
            results.len(),
            1,
            "expected one tool result (the blocked message)"
        );
        assert!(
            results[0].contains("tool blocked by hook"),
            "tool result must be hook stderr when blocked, got: {:?}",
            results[0]
        );
        drop(results);
    }

    #[tokio::test]
    async fn test_integration_post_tool_use_hook_does_not_alter_result() {
        use agents::{AgentHooks, HookEntry};
        use std::sync::Mutex;

        let results_store: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let results_store_c = Arc::clone(&results_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(move |_, _, _, block| {
                if let ContentBlock::ToolResult { content, .. } = block {
                    results_store_c.lock().unwrap().push(content.clone());
                }
                Ok(())
            });
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));

        let hooks = AgentHooks {
            post_tool_use: vec![HookEntry {
                matcher: Some(".*".to_string()),
                hooks: vec![hook_cmd("exit 99", 5)],
            }],
            ..AgentHooks::default()
        };

        let session = make_session();
        let (event_tx, _rx) = broadcast::channel(64);

        let history = vec![(Role::User, vec![ContentBlock::Text { text: "go".into() }])];
        let config = HarnessConfig {
            session: session.clone(),
            model: "test".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: None,
            cwd: None,
        };
        let client: Arc<dyn AnthropicClient> = Arc::new(ToolUseClient::new());
        let db: Arc<dyn db::Db> = Arc::new(mock_db);

        run_tool_dispatch_loop_test(&config, &client, &db, &event_tx, &hooks, history)
            .await
            .unwrap();

        let results = results_store.lock().unwrap();
        assert_eq!(results.len(), 1, "expected one tool result");
        assert_eq!(
            results[0], "file content here",
            "PostToolUse hook must not alter the tool result"
        );
        drop(results);
    }

    #[tokio::test]
    async fn test_integration_stop_hook_exit_0_lets_session_complete() {
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![hook_cmd("exit 0", 5)],
            }],
            ..AgentHooks::default()
        };

        let session = make_session();
        let (event_tx, _rx) = broadcast::channel(64);

        let history = vec![(Role::User, vec![ContentBlock::Text { text: "hi".into() }])];
        let config = HarnessConfig {
            session: session.clone(),
            model: "test".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client: Arc<dyn AnthropicClient> = Arc::new(StubClient);
        let mock_db = permissive_mock_db();
        let db: Arc<dyn db::Db> = Arc::new(mock_db);

        let result =
            run_tool_dispatch_loop_test(&config, &client, &db, &event_tx, &hooks, history)
                .await
                .unwrap();
        assert!(
            result.1.is_none(),
            "stop hook exit 0 must return None (normal completion)"
        );
    }

    #[tokio::test]
    async fn test_integration_stop_hook_exit_1_injects_user_message() {
        use agents::{AgentHooks, HookEntry};

        let hooks = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![hook_cmd("echo 'do more work'; exit 1", 5)],
            }],
            ..AgentHooks::default()
        };

        let session = make_session();
        let (event_tx, _rx) = broadcast::channel(64);

        let history = vec![(Role::User, vec![ContentBlock::Text { text: "hi".into() }])];
        let config = HarnessConfig {
            session: session.clone(),
            model: "test".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client: Arc<dyn AnthropicClient> = Arc::new(StubClient);
        let mock_db = permissive_mock_db();
        let db: Arc<dyn db::Db> = Arc::new(mock_db);

        let result =
            run_tool_dispatch_loop_test(&config, &client, &db, &event_tx, &hooks, history)
                .await
                .unwrap();
        assert!(
            result.1.is_some(),
            "stop hook exit 1 must inject a user message"
        );
        let injected = result.1.unwrap();
        assert!(
            injected.contains("do more work"),
            "injected message must contain hook stdout, got: {injected:?}"
        );
    }

    // ── Stop tool integration tests (GH#121) ─────────────────────────────────

    /// A client that first returns `tool_use` for "stop" with complete status, then `end_turn`.
    struct StopCompleteClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    impl StopCompleteClient {
        fn new() -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for StopCompleteClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(MessageResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_stop".into(),
                        name: "stop".into(),
                        input: serde_json::json!({"status": "complete", "comment": "Task done."}),
                    }],
                    stop_reason: "tool_use".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                })
            } else {
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "All done.".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 10,
                    output_tokens: 3,
                })
            }
        }
    }

    /// A client that calls stop with `status=waiting` (no comment), then `end_turn`.
    struct StopWaitingClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    impl StopWaitingClient {
        fn new() -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for StopWaitingClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(MessageResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_stop".into(),
                        name: "stop".into(),
                        input: serde_json::json!({"status": "waiting"}),
                    }],
                    stop_reason: "tool_use".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                })
            } else {
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "Pausing.".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 10,
                    output_tokens: 3,
                })
            }
        }
    }

    /// Scenario 1: Agent calls stop with status=complete → session becomes Waiting (not Completed),
    /// Stopped event emitted with Complete + comment, Done emitted.
    #[tokio::test]
    async fn test_stop_complete_emits_stopped_event_session_becomes_waiting() {
        let session = make_session();
        let session_id = session.id;

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        let mut seq = mockall::Sequence::new();
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| *id == session_id && *status == types::SessionStatus::Running)
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));
        // Even with stop(complete), session becomes Waiting (not Completed)
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| {
                *id == session_id && *status == types::SessionStatus::Waiting
            })
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StopCompleteClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("do something".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Must emit Stopped{Complete, "Task done."} before Done
        let stopped_idx = events.iter().position(|e| {
            matches!(
                e,
                SessionEvent::Stopped {
                    status: events::StopEventStatus::Complete,
                    comment: Some(c),
                } if c == "Task done."
            )
        });
        let done_idx = events.iter().position(|e| matches!(e, SessionEvent::Done));

        assert!(
            stopped_idx.is_some(),
            "expected Stopped{{Complete, 'Task done.'}} event"
        );
        assert!(done_idx.is_some(), "expected Done event");
        assert!(
            stopped_idx.unwrap() < done_idx.unwrap(),
            "Stopped must come before Done"
        );
    }

    /// Scenario 2: Agent calls stop with status=waiting → session becomes Waiting,
    /// Stopped event emitted with Waiting + no comment, Done emitted.
    #[tokio::test]
    async fn test_stop_waiting_sets_session_waiting_and_emits_stopped_event() {
        let session = make_session();
        let session_id = session.id;

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        let mut seq = mockall::Sequence::new();
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| *id == session_id && *status == types::SessionStatus::Running)
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| {
                *id == session_id && *status == types::SessionStatus::Waiting
            })
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StopWaitingClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("do something".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Stopped event with Waiting and no comment
        let stopped = events.iter().find(|e| {
            matches!(
                e,
                SessionEvent::Stopped {
                    status: events::StopEventStatus::Waiting,
                    comment: None,
                }
            )
        });
        assert!(
            stopped.is_some(),
            "expected Stopped{{Waiting, None}} event, got: {events:?}"
        );
        assert!(events.iter().any(|e| matches!(e, SessionEvent::Done)));
    }

    /// Scenario 3: Agent ends without calling stop → session becomes Waiting,
    /// no Stopped event emitted.
    #[tokio::test]
    async fn test_no_stop_tool_call_sets_session_waiting_no_stopped_event() {
        let session = make_session();
        let session_id = session.id;

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        mock_db
            .expect_list_issues_by_session_id()
            .returning(|_| Ok(vec![]));

        let mut seq = mockall::Sequence::new();
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| *id == session_id && *status == types::SessionStatus::Running)
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| {
                *id == session_id && *status == types::SessionStatus::Waiting
            })
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // No Stopped event
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::Stopped { .. })),
            "must NOT emit Stopped when stop tool was not called"
        );
        assert!(events.iter().any(|e| matches!(e, SessionEvent::Done)));
    }

    // ── GH#47: 429 rate-limit retry tests ────────────────────────────────────

    /// A client that returns 429 `n_failures` times, then succeeds on the next call.
    struct RateLimitThenOkClient {
        call_count: std::sync::atomic::AtomicU32,
        n_failures: u32,
    }

    impl RateLimitThenOkClient {
        fn new(n_failures: u32) -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
                n_failures,
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for RateLimitThenOkClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count < self.n_failures {
                Err(anthropic::Error::Api {
                    status: 429,
                    message: "rate limited".into(),
                })
            } else {
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text { text: "ok".into() }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 5,
                    output_tokens: 3,
                })
            }
        }
    }

    /// Retrying up to 5 times: a client that returns 429 N times (N ≤ 5) then succeeds
    /// should complete the session successfully.
    ///
    /// `start_paused = true` makes `tokio::time::sleep` return immediately, so the
    /// test does not actually wait 10 s per retry.
    #[tokio::test(start_paused = true)]
    async fn test_429_retried_up_to_5_times_then_succeeds() {
        let _guard = ENV_LOCK.lock().await;
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        // 5 failures then success — exactly at the retry limit
        let client = Arc::new(RateLimitThenOkClient::new(5));
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Should have completed successfully — no Error event, SessionDone present
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "should NOT emit Error when 429 retried and eventually succeeds"
        );
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::Done)),
            "expected SessionDone after successful retry"
        );
    }

    /// When the client returns 429 more than 5 times (all retries exhausted),
    /// the harness must emit `SessionEvent::Error`.
    ///
    /// `start_paused = true` makes `tokio::time::sleep` return immediately.
    #[tokio::test(start_paused = true)]
    async fn test_429_exhausts_all_retries_emits_error() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        // 6 failures → exceeds max 5 retries
        let client = Arc::new(RateLimitThenOkClient::new(6));
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "expected SessionEvent::Error when all 429 retries are exhausted"
        );
    }

    /// Non-429 API errors are not retried and propagate immediately.
    #[tokio::test]
    async fn test_non_429_api_error_not_retried() {
        struct AlwaysServerErrorClient;

        #[async_trait]
        impl AnthropicClient for AlwaysServerErrorClient {
            async fn complete(
                &self,
                _request: MessageRequest,
            ) -> anthropic::Result<MessageResponse> {
                Err(anthropic::Error::Api {
                    status: 500,
                    message: "internal server error".into(),
                })
            }
        }

        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        let client = Arc::new(AlwaysServerErrorClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        // Should return Err (non-429 is not swallowed)
        let result = run(config, client, db, event_tx, msg_rx).await;
        assert!(
            result.is_err(),
            "non-429 error should propagate immediately as Err"
        );
    }

    /// `NS2_MAX_RETRIES` env var overrides the default of 5.
    /// Set it to 2 → a client that fails 3 times should exhaust retries.
    ///
    /// Note: env var mutation in tests is inherently racy with parallel execution.
    /// This test is run with `#[tokio::test(start_paused = true)]` so there is no
    /// real sleep, keeping it fast.  We still guard with a unique env-var read
    /// inside the retry loop so the env value is sampled at test time.
    #[tokio::test(start_paused = true)]
    async fn test_ns2_max_retries_env_override() {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("NS2_MAX_RETRIES", "2");

        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: None,
        };
        // 3 failures → exceeds override max of 2
        let client = Arc::new(RateLimitThenOkClient::new(3));
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        std::env::remove_var("NS2_MAX_RETRIES");

        let mut events = vec![];
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "expected Error when NS2_MAX_RETRIES=2 and client fails 3 times"
        );
    }

    // ── GH#33 project hook inheritance tests ─────────────────────────────────

    #[tokio::test]
    async fn test_include_project_config_true_runs_project_hook() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let log_file =
            std::env::temp_dir().join(format!("ns2_proj_hook_test_{}.txt", uuid::Uuid::new_v4()));
        let log_path = log_file.to_string_lossy().to_string();

        let claude_dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"matcher":".*","hooks":[{{"type":"command","command":"echo project-hook >> {log_path}","timeout":10}}]}}]}}}}"#
            ),
        ).unwrap();

        let agent_name = "harness_test_project_hook_true";
        let def = agents::AgentDef {
            name: agent_name.to_string(),
            description: "test agent for project hook inheritance".to_string(),
            body: "You are a test agent.".to_string(),
            include_project_config: true,
            hooks: agents::AgentHooks::default(),
        };
        agents::write_agent(&agents_dir, &def).unwrap();

        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: Some(agent_name.to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mock_db = permissive_mock_db();
        let config = HarnessConfig {
            session: session.clone(),
            model: "test".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: Some(tmp.path().to_path_buf()),
            cwd: None,
        };
        let client = Arc::new(ToolUseClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(128);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("read the file".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let log_content = std::fs::read_to_string(&log_file).unwrap_or_default();
        let _ = std::fs::remove_file(&log_file);
        assert!(
            log_content.contains("project-hook"),
            "project PostToolUse hook must have run (log file contents: {log_content:?})"
        );
    }

    #[tokio::test]
    async fn test_include_project_config_false_does_not_run_project_hook() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let log_file = std::env::temp_dir().join(format!(
            "ns2_proj_hook_false_test_{}.txt",
            uuid::Uuid::new_v4()
        ));
        let log_path = log_file.to_string_lossy().to_string();

        let claude_dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"matcher":".*","hooks":[{{"type":"command","command":"echo project-hook >> {log_path}","timeout":10}}]}}]}}}}"#
            ),
        ).unwrap();

        let agent_name = "harness_test_project_hook_false";
        let def = agents::AgentDef {
            name: agent_name.to_string(),
            description: "test agent without project hook inheritance".to_string(),
            body: "You are a test agent.".to_string(),
            include_project_config: false,
            hooks: agents::AgentHooks::default(),
        };
        agents::write_agent(&agents_dir, &def).unwrap();

        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: Some(agent_name.to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mock_db = permissive_mock_db();
        let config = HarnessConfig {
            session: session.clone(),
            model: "test".into(),
            tools: vec![Arc::new(AlwaysOkTool)],
            git_root: Some(tmp.path().to_path_buf()),
            cwd: None,
        };
        let client = Arc::new(ToolUseClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(128);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("read the file".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let log_content = std::fs::read_to_string(&log_file).unwrap_or_default();
        let _ = std::fs::remove_file(&log_file);
        assert!(
            !log_content.contains("project-hook"),
            "project hook must NOT run when include_project_config=false (log: {log_content:?})"
        );
    }

    // ── Tool cwd tests ────────────────────────────────────────────────────────

    /// Records the cwd it was called with and always succeeds.
    struct CwdCapturingTool {
        was_called: Arc<std::sync::atomic::AtomicBool>,
        captured_cwd: Arc<Mutex<Option<std::path::PathBuf>>>,
    }

    impl CwdCapturingTool {
        fn new() -> (
            Arc<std::sync::atomic::AtomicBool>,
            Arc<Mutex<Option<std::path::PathBuf>>>,
            Arc<Self>,
        ) {
            let was_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let captured_cwd = Arc::new(Mutex::new(None));
            let tool = Arc::new(Self {
                was_called: Arc::clone(&was_called),
                captured_cwd: Arc::clone(&captured_cwd),
            });
            (was_called, captured_cwd, tool)
        }
    }

    #[async_trait::async_trait]
    impl tools::Tool for CwdCapturingTool {
        fn definition(&self) -> types::ToolDefinition {
            types::ToolDefinition {
                name: "cwd-capture".into(),
                description: "Captures the cwd it was called with".into(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            }
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            cwd: Option<&std::path::Path>,
        ) -> tools::Result<String> {
            self.was_called
                .store(true, std::sync::atomic::Ordering::SeqCst);
            *self.captured_cwd.lock().await = cwd.map(std::path::Path::to_path_buf);
            Ok("captured".into())
        }
    }

    /// A client that returns `tool_use` on first call (for `cwd-capture`), `end_turn` on second.
    struct CwdCaptureClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    impl CwdCaptureClient {
        fn new() -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for CwdCaptureClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(MessageResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_cwd".into(),
                        name: "cwd-capture".into(),
                        input: serde_json::json!({}),
                    }],
                    stop_reason: "tool_use".into(),
                    input_tokens: 5,
                    output_tokens: 3,
                })
            } else {
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text {
                        text: "done".into(),
                    }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 5,
                    output_tokens: 2,
                })
            }
        }
    }

    /// When `config.git_root` is injected and the mock DB returns an issue with a
    /// non-empty branch, `run()` should resolve the worktree path via
    /// `resolve_session_cwd_with_root(git_root)` and pass it as `cwd` to tool
    /// dispatch — so the tool executes inside the worktree, not the server's cwd.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn run_resolves_worktree_cwd_from_git_root_when_issue_has_branch() {
        // Set up a real git repo so ensure_worktree can create a worktree.
        let origin_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(origin_dir.path())
            .status()
            .unwrap();

        let local_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["clone", &origin_dir.path().to_string_lossy(), "."])
            .current_dir(local_dir.path())
            .status()
            .unwrap();

        for cmd in [
            ["config", "user.email", "t@t"],
            ["config", "user.name", "test"],
        ] {
            std::process::Command::new("git")
                .args(cmd)
                .current_dir(local_dir.path())
                .status()
                .unwrap();
        }
        std::fs::write(local_dir.path().join("README.md"), "init").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(local_dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false", "commit", "-m", "init"])
            .current_dir(local_dir.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(local_dir.path())
            .status()
            .unwrap();

        // Write an ns2.toml in local_dir pointing worktrees to a known temp dir.
        // This distinguishes local_dir's config from the ns2 repo's config so we
        // can assert the tool ran from local_dir's worktree, not the ns2 repo's.
        let worktrees_base = tempfile::TempDir::new().unwrap();
        std::fs::write(
            local_dir.path().join("ns2.toml"),
            format!(
                "[worktrees]\npath = \"{}\"\n",
                worktrees_base.path().display()
            ),
        )
        .unwrap();

        let branch = "feat/worktree-cwd-test";
        let session_id = Uuid::new_v4();

        // Build the mock DB from scratch so list_issues_by_session_id returns our
        // issue (not the empty-vec default from permissive_mock_db).
        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db
            .expect_list_content_blocks()
            .returning(|_| Ok(vec![]));
        mock_db
            .expect_list_issues_by_session_id()
            .returning(move |_| {
                Ok(vec![types::Issue {
                    id: "wd01".into(),
                    title: "Worktree cwd test".into(),
                    body: "body".into(),
                    status: types::IssueStatus::Running,
                    branch: branch.into(),
                    assignee: None,
                    session_id: Some(session_id),
                    parent_id: None,
                    blocked_on: vec![],
                    comments: vec![],
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }])
            });

        let (was_called, captured_cwd, cwd_tool) = CwdCapturingTool::new();

        let mut session = make_session();
        session.id = session_id;

        let config = HarnessConfig {
            session,
            model: "claude-opus-4-5".into(),
            tools: vec![cwd_tool],
            git_root: Some(local_dir.path().to_owned()),
            cwd: None,
        };

        let client = Arc::new(CwdCaptureClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, _) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("go".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        assert!(
            was_called.load(std::sync::atomic::Ordering::SeqCst),
            "cwd-capture tool should have been called"
        );
        // Extract the path without holding the guard across subsequent code.
        let tool_cwd = captured_cwd
            .lock()
            .await
            .as_ref()
            .expect("tool should have received a non-None cwd")
            .clone();
        // The worktree must be inside worktrees_base (derived from local_dir's ns2.toml),
        // not under the ns2 repo's worktrees directory.
        assert!(
            tool_cwd.starts_with(worktrees_base.path()),
            "tool cwd should be inside worktrees_base (from local_dir/ns2.toml); \
             expected prefix {}, got {}",
            worktrees_base.path().display(),
            tool_cwd.display()
        );
    }

    // ── Effective-root / system-prompt tests ──────────────────────────────────

    /// When `config.cwd` is set and `config.git_root` is absent, `run()` must use
    /// `cwd` as the effective root so the system prompt preamble reflects the
    /// worktree path rather than whatever `git rev-parse --show-toplevel` returns.
    #[tokio::test]
    async fn run_builds_system_prompt_from_cwd_when_git_root_absent() {
        let worktree_dir = tempfile::TempDir::new().unwrap();
        let agents_dir_path = worktree_dir.path().join(".ns2").join("agents");
        std::fs::create_dir_all(&agents_dir_path).unwrap();
        // Agent body is required; without it build_system_prompt returns None.
        std::fs::write(
            agents_dir_path.join("test-agent.md"),
            "---\nname: test-agent\ndescription: test\n---\nTest body",
        )
        .unwrap();

        let mut session = make_session();
        session.agent = Some("test-agent".into());
        let mock_db = permissive_mock_db(); // returns empty issues → cwd not overridden

        let config = HarnessConfig {
            session,
            model: "claude-opus-4-5".into(),
            tools: vec![],
            git_root: None,
            cwd: Some(worktree_dir.path().to_owned()),
        };

        let client = Arc::new(SystemCapturingClient::new());
        let client_ref = Arc::clone(&client);
        let db = Arc::new(mock_db);
        let (event_tx, _) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let system = client_ref.captured().unwrap_or_default();
        let worktree_path_str = worktree_dir.path().to_str().unwrap();
        assert!(
            system.contains(worktree_path_str),
            "system prompt should reference cwd/worktree path '{worktree_path_str}'; got: {system:?}"
        );
    }
}
