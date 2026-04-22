use anthropic::{AnthropicClient, MessageRequest, MessageResponse};
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use types::{ContentBlock, ContentBlockDelta, Role, Session, SessionEvent, SessionStatus, Turn};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db error: {0}")]
    Db(#[from] db::Error),
    #[error("anthropic error: {0}")]
    Anthropic(#[from] anthropic::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct StubClient;

#[async_trait]
impl AnthropicClient for StubClient {
    async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
        Ok(MessageResponse {
            content: vec![ContentBlock::Text {
                text: "Hello! I'm a stub assistant.".into(),
            }],
            stop_reason: "end_turn".into(),
            input_tokens: 10,
            output_tokens: 8,
        })
    }
}

pub struct HarnessConfig {
    pub session: Session,
    pub model: String,
    pub system: Option<String>,
    pub tools: Vec<Arc<dyn tools::Tool>>,
}

/// Load conversation history from the DB for a session.
/// Returns turns in order, each as `(Role, Vec<ContentBlock>)`.
/// Turns with mixed roles are grouped by the role stored on each block;
/// consecutive blocks with the same role are merged into one entry.
async fn load_history(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
) -> Result<Vec<(Role, Vec<ContentBlock>)>> {
    let turns = db.list_turns(session_id).await?;
    let mut history: Vec<(Role, Vec<ContentBlock>)> = Vec::new();

    for turn in &turns {
        let blocks = db.list_content_blocks(turn.id).await?;
        if blocks.is_empty() {
            continue;
        }
        // Each turn is stored with a consistent role; group all blocks under one entry.
        // If blocks have mixed roles (shouldn't happen in practice), group by first role.
        let role = blocks[0].0.clone();
        let content: Vec<ContentBlock> = blocks.into_iter().map(|(_, b)| b).collect();

        // Merge with previous entry if same role
        if let Some(last) = history.last_mut() {
            if last.0 == role {
                last.1.extend(content);
                continue;
            }
        }
        history.push((role, content));
    }

    Ok(history)
}

/// Persist a user message as a turn+block in the DB and emit events.
async fn persist_user_message(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
    message: &str,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> Result<Turn> {
    let user_turn = Turn {
        id: Uuid::new_v4(),
        session_id,
        token_count: None,
        created_at: Utc::now(),
    };
    db.create_turn(&user_turn).await?;
    let user_block = ContentBlock::Text { text: message.to_string() };
    db.create_content_block(user_turn.id, 0, &Role::User, &user_block).await?;
    let _ = event_tx.send(SessionEvent::TurnStarted { turn: user_turn.clone() });
    let _ = event_tx.send(SessionEvent::ContentBlockDelta {
        turn_id: user_turn.id,
        index: 0,
        delta: ContentBlockDelta::TextDelta { text: message.to_string() },
    });
    let _ = event_tx.send(SessionEvent::ContentBlockDone {
        turn_id: user_turn.id,
        index: 0,
        block: user_block,
    });
    let _ = event_tx.send(SessionEvent::TurnDone { turn_id: user_turn.id });
    Ok(user_turn)
}

/// Run the tool dispatch loop for a single LLM turn sequence.
/// `messages` should already contain the full history including the new user message.
async fn run_tool_dispatch_loop(
    config: &HarnessConfig,
    client: &Arc<dyn AnthropicClient>,
    db: &Arc<dyn db::Db>,
    event_tx: &broadcast::Sender<SessionEvent>,
    mut messages: Vec<(Role, Vec<ContentBlock>)>,
) -> Result<()> {
    let tool_definitions: Vec<types::ToolDefinition> =
        config.tools.iter().map(|t| t.definition()).collect();

    loop {
        let request = MessageRequest {
            model: config.model.clone(),
            system: config.system.clone(),
            messages: messages.clone(),
            max_tokens: 4096,
            tools: tool_definitions.clone(),
        };

        let response = client.complete(request).await?;

        // Create assistant turn in DB
        let turn = Turn {
            id: Uuid::new_v4(),
            session_id: config.session.id,
            token_count: Some((response.input_tokens + response.output_tokens) as i64),
            created_at: Utc::now(),
        };
        db.create_turn(&turn).await?;
        let _ = event_tx.send(SessionEvent::TurnStarted { turn: turn.clone() });

        // Store and emit content blocks
        for (index, block) in response.content.iter().enumerate() {
            let index = index as u32;
            if let ContentBlock::Text { text } = block {
                let _ = event_tx.send(SessionEvent::ContentBlockDelta {
                    turn_id: turn.id,
                    index,
                    delta: ContentBlockDelta::TextDelta { text: text.clone() },
                });
            }
            db.create_content_block(turn.id, index as i64, &Role::Assistant, block).await?;
            let _ = event_tx.send(SessionEvent::ContentBlockDone {
                turn_id: turn.id,
                index,
                block: block.clone(),
            });
        }

        let _ = event_tx.send(SessionEvent::TurnDone { turn_id: turn.id });

        match response.stop_reason.as_str() {
            "tool_use" => { /* dispatch tools below */ }
            "end_turn" => break,
            "max_tokens" => {
                let _ = event_tx.send(SessionEvent::Error {
                    message: "session hit max_tokens limit".to_string(),
                });
                break;
            }
            other => {
                tracing::warn!("unknown stop_reason: {other}");
                break;
            }
        }

        // Add assistant turn to history
        messages.push((Role::Assistant, response.content.clone()));

        // Execute tool calls and build tool result turn
        let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();

        for block in &response.content {
            if let ContentBlock::ToolUse { id, name, input } = block {
                let result = match config.tools.iter().find(|t| t.definition().name == *name) {
                    Some(tool) => match tool.execute(input.clone()).await {
                        Ok(output) => output,
                        Err(e) => format!("Error: {e}"),
                    },
                    None => format!("Error: unknown tool '{name}'"),
                };
                tool_result_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: result,
                });
            }
        }

        // Store tool result turn in DB
        let tool_result_turn = Turn {
            id: Uuid::new_v4(),
            session_id: config.session.id,
            token_count: None,
            created_at: Utc::now(),
        };
        db.create_turn(&tool_result_turn).await?;
        let _ = event_tx.send(SessionEvent::TurnStarted { turn: tool_result_turn.clone() });

        for (index, block) in tool_result_blocks.iter().enumerate() {
            let index = index as u32;
            db.create_content_block(tool_result_turn.id, index as i64, &Role::User, block)
                .await?;
            let _ = event_tx.send(SessionEvent::ContentBlockDone {
                turn_id: tool_result_turn.id,
                index,
                block: block.clone(),
            });
        }

        let _ = event_tx.send(SessionEvent::TurnDone { turn_id: tool_result_turn.id });

        // Add tool result turn to history and loop
        messages.push((Role::User, tool_result_blocks));
    }

    Ok(())
}

pub async fn run(
    config: HarnessConfig,
    client: Arc<dyn AnthropicClient>,
    db: Arc<dyn db::Db>,
    event_tx: broadcast::Sender<SessionEvent>,
    mut msg_rx: mpsc::Receiver<String>,
) -> Result<()> {
    loop {
        // Wait for the next user message. When the sender is dropped, recv() returns None → exit.
        let message = match msg_rx.recv().await {
            Some(m) => m,
            None => break,
        };

        // Transition session to Running
        db.update_session_status(config.session.id, SessionStatus::Running).await?;

        // Persist user message and emit events
        persist_user_message(&db, config.session.id, &message, &event_tx).await?;

        // Load full conversation history from DB (including the message we just stored)
        let history = load_history(&db, config.session.id).await?;

        // Run the tool dispatch loop
        run_tool_dispatch_loop(&config, &client, &db, &event_tx, history).await?;

        // Mark session completed and emit done event
        db.update_session_status(config.session.id, SessionStatus::Completed).await?;
        let _ = event_tx.send(SessionEvent::SessionDone { session_id: config.session.id });

        // Loop back to wait for next message
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockall::mock;
    use tokio::sync::{broadcast, mpsc};

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
    // and returns empty lists for list_turns / list_content_blocks.
    fn permissive_mock_db() -> MockTestDb {
        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db.expect_create_content_block().returning(|_, _, _, _| Ok(()));
        mock_db.expect_update_session_status().returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db.expect_list_content_blocks().returning(|_| Ok(vec![]));
        mock_db
    }

    #[tokio::test]
    async fn test_run_with_stub_client() {
        let mock_db = permissive_mock_db();
        let session = make_session();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![],
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

        assert!(events.iter().any(|e| matches!(e, SessionEvent::TurnStarted { .. })));
        assert!(events.iter().any(|e| matches!(e, SessionEvent::ContentBlockDelta { .. })));
        assert!(events.iter().any(|e| matches!(e, SessionEvent::ContentBlockDone { .. })));
        assert!(events.iter().any(|e| matches!(e, SessionEvent::TurnDone { .. })));
        assert!(events.iter().any(|e| matches!(e, SessionEvent::SessionDone { .. })));
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
        mock_db.expect_create_content_block().returning(|_, _, _, _| Ok(()));
        mock_db.expect_update_session_status().returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db.expect_list_content_blocks().returning(|_| Ok(vec![]));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![],
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
        mock_db.expect_create_content_block().returning(|_, _, _, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db.expect_list_content_blocks().returning(|_| Ok(vec![]));
        // Expect Running then Completed
        let mut seq = mockall::Sequence::new();
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| {
                *id == session_id && *status == types::SessionStatus::Running
            })
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| {
                *id == session_id && *status == types::SessionStatus::Completed
            })
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![],
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
            system: None,
            tools: vec![],
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
            events.iter().any(|e| matches!(e, SessionEvent::TurnStarted { .. })),
            "missing TurnStarted"
        );
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::ContentBlockDone { .. })),
            "missing ContentBlockDone"
        );
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::TurnDone { .. })),
            "missing TurnDone"
        );
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::SessionDone { .. })),
            "missing SessionDone"
        );
    }

    #[tokio::test]
    async fn test_run_session_done_carries_correct_session_id() {
        let session = make_session();
        let session_id = session.id;
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![],
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
            .find(|e| matches!(e, SessionEvent::SessionDone { .. }))
            .expect("no SessionDone event");

        assert!(
            matches!(done_event, SessionEvent::SessionDone { session_id: sid } if *sid == session_id)
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
                    types::ContentBlock::Text { text: "block one".into() },
                    types::ContentBlock::Text { text: "block two".into() },
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
            system: None,
            tools: vec![],
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

        let done_blocks: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::ContentBlockDone { .. }))
            .collect();
        // 1 user block + 2 assistant blocks = 3 ContentBlockDone events
        assert_eq!(done_blocks.len(), 3, "expected 3 ContentBlockDone events (1 user + 2 assistant)");
    }

    #[tokio::test]
    async fn test_run_exits_when_channel_closed_without_message() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![],
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

    /// A mock client that first returns tool_use, then end_turn on the second call.
    struct ToolUseClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    impl ToolUseClient {
        fn new() -> Self {
            Self { call_count: std::sync::atomic::AtomicU32::new(0) }
        }
    }

    #[async_trait]
    impl AnthropicClient for ToolUseClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

        async fn execute(&self, _input: serde_json::Value) -> tools::Result<String> {
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

        async fn execute(&self, _input: serde_json::Value) -> tools::Result<String> {
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
            system: None,
            tools: vec![Arc::new(AlwaysOkTool)],
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
            events.iter().any(|e| matches!(e, SessionEvent::SessionDone { .. })),
            "missing SessionDone"
        );

        // Should have ContentBlockDone events for both ToolUse and final Text
        let done_blocks: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::ContentBlockDone { .. }))
            .collect();
        // At least: user text, assistant tool_use, tool_result, final text
        assert!(done_blocks.len() >= 4, "expected at least 4 ContentBlockDone events, got {}", done_blocks.len());

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
                SessionEvent::ContentBlockDone { block: ContentBlock::ToolResult { .. }, .. }
            )),
            "missing ToolResult ContentBlockDone"
        );

        // Verify final text block
        assert!(
            done_blocks.iter().any(|e| matches!(
                e,
                SessionEvent::ContentBlockDone { block: ContentBlock::Text { .. }, .. }
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
            system: None,
            tools: vec![Arc::new(AlwaysErrTool)],
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
            events.iter().any(|e| matches!(e, SessionEvent::SessionDone { .. })),
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
    /// Call 0: end_turn with "First response."
    /// Call 1: end_turn with "Second response with context."
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
            let count = self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(MessageResponse {
                    content: vec![ContentBlock::Text { text: "First response.".into() }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                })
            } else {
                // Capture the messages for later assertion
                let mut guard = self.second_call_messages.lock().unwrap();
                *guard = request.messages;
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

    /// Test: two sequential tool calls in one run both resolved before final response.
    #[tokio::test]
    async fn test_two_sequential_tool_calls_in_one_run() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        // Client: call 0 → tool_use, call 1 → tool_use, call 2 → end_turn
        struct TwoToolClient {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait]
        impl AnthropicClient for TwoToolClient {
            async fn complete(
                &self,
                _request: MessageRequest,
            ) -> anthropic::Result<MessageResponse> {
                let count =
                    self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![Arc::new(AlwaysOkTool)],
        };
        let client = Arc::new(TwoToolClient { call_count: std::sync::atomic::AtomicU32::new(0) });
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
        let tool_use_blocks: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    SessionEvent::ContentBlockDone { block: ContentBlock::ToolUse { .. }, .. }
                )
            })
            .collect();
        assert_eq!(tool_use_blocks.len(), 2, "expected 2 ToolUse blocks");

        let tool_result_blocks: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    SessionEvent::ContentBlockDone { block: ContentBlock::ToolResult { .. }, .. }
                )
            })
            .collect();
        assert_eq!(tool_result_blocks.len(), 2, "expected 2 ToolResult blocks");

        // SessionDone should be present
        assert!(events.iter().any(|e| matches!(e, SessionEvent::SessionDone { .. })));
    }

    /// Test: second user message is processed with all prior turns in context.
    #[tokio::test]
    async fn test_second_message_includes_prior_history() {
        let session = make_session();
        let session_id = session.id;

        // We need a DB that returns real turn/block data on the second call.
        // Strategy: use a mutex-wrapped Vec to accumulate created turns/blocks,
        // and return them on list_turns/list_content_blocks calls.
        use std::sync::Mutex;

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
        mock_db.expect_create_content_block().returning(move |turn_id, _idx, role, block| {
            blocks_store_c.lock().unwrap().push((turn_id, role.clone(), block.clone()));
            Ok(())
        });
        mock_db.expect_update_session_status().returning(|_, _| Ok(()));
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
            system: None,
            tools: vec![],
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

    /// Client that always returns max_tokens stop reason.
    struct MaxTokensClient;

    #[async_trait]
    impl AnthropicClient for MaxTokensClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            Ok(MessageResponse {
                content: vec![ContentBlock::Text { text: "truncated output".into() }],
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
            system: None,
            tools: vec![],
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
        let error_event = events.iter().find(|e| matches!(e, SessionEvent::Error { .. }));
        assert!(error_event.is_some(), "expected a SessionEvent::Error for max_tokens");
        assert!(
            matches!(error_event.unwrap(), SessionEvent::Error { message } if message.contains("max_tokens")),
            "error message should mention max_tokens"
        );

        // Loop should have exited cleanly (SessionDone is emitted)
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::SessionDone { .. })),
            "expected SessionDone after max_tokens"
        );
    }

    // --- Fix 2a: unknown tool name returns error tool result ---

    /// Client: call 0 returns tool_use for a nonexistent tool, call 1 returns end_turn.
    struct UnknownToolClient {
        call_count: std::sync::atomic::AtomicU32,
    }

    impl UnknownToolClient {
        fn new() -> Self {
            Self { call_count: std::sync::atomic::AtomicU32::new(0) }
        }
    }

    #[async_trait]
    impl AnthropicClient for UnknownToolClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let count = self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                    content: vec![ContentBlock::Text { text: "done".into() }],
                    stop_reason: "end_turn".into(),
                    input_tokens: 15,
                    output_tokens: 3,
                })
            }
        }
    }

    #[tokio::test]
    async fn test_unknown_tool_name_returns_error_tool_result() {
        let session = make_session();

        // We need a DB that captures stored content blocks so we can inspect them.
        use std::sync::Mutex;
        let blocks_store: Arc<Mutex<Vec<(Uuid, types::Role, types::ContentBlock)>>> =
            Arc::new(Mutex::new(vec![]));
        let blocks_store_c = Arc::clone(&blocks_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db.expect_create_content_block().returning(move |turn_id, _idx, role, block| {
            blocks_store_c.lock().unwrap().push((turn_id, role.clone(), block.clone()));
            Ok(())
        });
        mock_db.expect_update_session_status().returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db.expect_list_content_blocks().returning(|_| Ok(vec![]));

        // Use a tools list that has only a different tool (not "nonexistent_tool")
        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![Arc::new(AlwaysOkTool)], // only "read", not "nonexistent_tool"
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
            events.iter().any(|e| matches!(e, SessionEvent::SessionDone { .. })),
            "expected SessionDone"
        );

        // A ToolResult block should have been stored with content containing "unknown tool"
        let stored = blocks_store.lock().unwrap();
        let tool_result_with_error = stored.iter().find(|(_, _, block)| {
            matches!(block, ContentBlock::ToolResult { content, .. } if {
                let lower = content.to_lowercase();
                lower.contains("unknown tool") || lower.contains("unknown")
            })
        });
        assert!(
            tool_result_with_error.is_some(),
            "expected a ToolResult block with 'unknown tool' error in DB; stored blocks: {:?}",
            stored.iter().map(|(_, _, b)| b).collect::<Vec<_>>()
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
            system: None,
            tools: vec![], // empty
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
            events.iter().any(|e| matches!(e, SessionEvent::SessionDone { .. })),
            "expected SessionDone"
        );
        // No Error events
        assert!(
            !events.iter().any(|e| matches!(e, SessionEvent::Error { .. })),
            "did not expect Error events for an empty tool list with end_turn response"
        );
    }

    // --- Fix 2c: tool result stored with Role::User ---

    #[tokio::test]
    async fn test_tool_result_stored_with_user_role() {
        let session = make_session();

        use std::sync::Mutex;
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
        mock_db.expect_create_content_block().returning(move |turn_id, _idx, role, block| {
            blocks_store_c.lock().unwrap().push((turn_id, role.clone(), block.clone()));
            Ok(())
        });
        mock_db.expect_update_session_status().returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(move |sid| {
            let turns = turns_store_l.lock().unwrap()
                .iter()
                .filter(|t| t.session_id == sid)
                .cloned()
                .collect();
            Ok(turns)
        });
        mock_db.expect_list_content_blocks().returning(move |tid| {
            let blocks = blocks_store_l.lock().unwrap()
                .iter()
                .filter(|(id, _, _)| *id == tid)
                .map(|(_, role, block)| (role.clone(), block.clone()))
                .collect();
            Ok(blocks)
        });

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![Arc::new(AlwaysOkTool)],
        };
        let client = Arc::new(ToolUseClient::new());
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(128);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("read the file".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let stored = blocks_store.lock().unwrap();
        let tool_result_entry = stored.iter().find(|(_, _, block)| {
            matches!(block, ContentBlock::ToolResult { .. })
        });
        assert!(tool_result_entry.is_some(), "expected a ToolResult block in DB");

        let (_, role, _) = tool_result_entry.unwrap();
        assert_eq!(*role, types::Role::User, "ToolResult block should be stored with Role::User");
    }

    // --- Fix 2d: sequential tool calls correct ordering ---

    #[tokio::test]
    async fn test_sequential_tool_calls_correct_ordering() {
        let session = make_session();
        let mock_db = permissive_mock_db();

        struct TwoToolOrderingClient {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait]
        impl AnthropicClient for TwoToolOrderingClient {
            async fn complete(
                &self,
                _request: MessageRequest,
            ) -> anthropic::Result<MessageResponse> {
                let count = self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                        content: vec![ContentBlock::Text { text: "All done.".into() }],
                        stop_reason: "end_turn".into(),
                        input_tokens: 20,
                        output_tokens: 6,
                    }),
                }
            }
        }

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![Arc::new(AlwaysOkTool)],
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
        let labels: Vec<&str> = events.iter().map(|e| match e {
            SessionEvent::TurnStarted { .. } => "TurnStarted",
            SessionEvent::ContentBlockDelta { .. } => "ContentBlockDelta",
            SessionEvent::ContentBlockDone { block: ContentBlock::Text { .. }, .. } => "ContentBlockDone(Text)",
            SessionEvent::ContentBlockDone { block: ContentBlock::ToolUse { .. }, .. } => "ContentBlockDone(ToolUse)",
            SessionEvent::ContentBlockDone { block: ContentBlock::ToolResult { .. }, .. } => "ContentBlockDone(ToolResult)",
            SessionEvent::TurnDone { .. } => "TurnDone",
            SessionEvent::SessionDone { .. } => "SessionDone",
            SessionEvent::Error { .. } => "Error",
        }).collect();

        // Expected sequence (one tool call then final response):
        // 1. TurnStarted (user)
        // 2. ContentBlockDelta (user text)
        // 3. ContentBlockDone (user text)
        // 4. TurnDone (user)
        // 5. TurnStarted (assistant first response)
        // 6. ContentBlockDone (ToolUse)
        // 7. TurnDone (assistant)
        // 8. TurnStarted (tool result)
        // 9. ContentBlockDone (ToolResult)
        // 10. TurnDone (tool result)
        // 11. TurnStarted (final assistant)
        // 12. ContentBlockDelta (final text)
        // 13. ContentBlockDone (final Text)
        // 14. TurnDone (final assistant)
        // 15. SessionDone
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
            "event ordering mismatch.\nGot:      {:?}\nExpected: {:?}",
            labels, expected
        );
    }

    // --- Token count tests ---

    struct KnownTokenClient;

    #[async_trait]
    impl AnthropicClient for KnownTokenClient {
        async fn complete(&self, _request: MessageRequest) -> anthropic::Result<MessageResponse> {
            Ok(MessageResponse {
                content: vec![ContentBlock::Text { text: "response".into() }],
                stop_reason: "end_turn".into(),
                input_tokens: 100,
                output_tokens: 50,
            })
        }
    }

    #[tokio::test]
    async fn test_assistant_turn_token_count_equals_input_plus_output() {
        let session = make_session();

        use std::sync::Mutex;
        let turns_store: Arc<Mutex<Vec<types::Turn>>> = Arc::new(Mutex::new(vec![]));
        let turns_store_c = Arc::clone(&turns_store);

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(move |turn| {
            turns_store_c.lock().unwrap().push(turn.clone());
            Ok(())
        });
        mock_db.expect_create_content_block().returning(|_, _, _, _| Ok(()));
        mock_db.expect_update_session_status().returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(|_| Ok(vec![]));
        mock_db.expect_list_content_blocks().returning(|_| Ok(vec![]));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
            tools: vec![],
        };
        let client = Arc::new(KnownTokenClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();
        drop(msg_tx);

        run(config, client, db, event_tx, msg_rx).await.unwrap();

        let stored = turns_store.lock().unwrap();
        let assistant_turn = stored.iter().find(|t| t.token_count.is_some());
        assert!(assistant_turn.is_some(), "expected an assistant turn with token_count set");
        assert_eq!(
            assistant_turn.unwrap().token_count,
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
            Self { captured_messages: std::sync::Mutex::new(vec![]) }
        }
    }

    #[async_trait]
    impl AnthropicClient for CapturingClient {
        async fn complete(&self, request: MessageRequest) -> anthropic::Result<MessageResponse> {
            let mut guard = self.captured_messages.lock().unwrap();
            if guard.is_empty() {
                *guard = request.messages;
            }
            Ok(MessageResponse {
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: "end_turn".into(),
                input_tokens: 10,
                output_tokens: 3,
            })
        }
    }

    #[tokio::test]
    async fn test_history_reconstructed_after_cold_restart() {
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
        use std::sync::Mutex;
        let turns_store: Arc<Mutex<Vec<types::Turn>>> =
            Arc::new(Mutex::new(vec![pre_user_turn.clone(), pre_assistant_turn.clone()]));
        let blocks_store: Arc<Mutex<Vec<(Uuid, types::Role, types::ContentBlock)>>> =
            Arc::new(Mutex::new(vec![
                (user_turn_id, types::Role::User, ContentBlock::Text { text: "hello".into() }),
                (assistant_turn_id, types::Role::Assistant, ContentBlock::Text { text: "world".into() }),
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
        mock_db.expect_create_content_block().returning(move |turn_id, _idx, role, block| {
            blocks_store_c.lock().unwrap().push((turn_id, role.clone(), block.clone()));
            Ok(())
        });
        mock_db.expect_update_session_status().returning(|_, _| Ok(()));
        mock_db.expect_list_turns().returning(move |sid| {
            let turns = turns_store_l.lock().unwrap()
                .iter()
                .filter(|t| t.session_id == sid)
                .cloned()
                .collect();
            Ok(turns)
        });
        mock_db.expect_list_content_blocks().returning(move |tid| {
            let blocks = blocks_store_l.lock().unwrap()
                .iter()
                .filter(|(id, _, _)| *id == tid)
                .map(|(_, role, block)| (role.clone(), block.clone()))
                .collect();
            Ok(blocks)
        });

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
            system: None,
            tools: vec![],
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
            captured.len(), 3,
            "expected 3 messages in API call (prior 2 + new 1), got {}: {:?}",
            captured.len(),
            captured.iter().map(|(r, blocks)| {
                let text = blocks.iter().find_map(|b| if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }).unwrap_or("?");
                format!("{:?}: {}", r, text)
            }).collect::<Vec<_>>()
        );

        assert_eq!(captured[0].0, Role::User, "first message should be User");
        assert!(
            matches!(&captured[0].1[0], ContentBlock::Text { text } if text == "hello"),
            "first message should be 'hello'"
        );

        assert_eq!(captured[1].0, Role::Assistant, "second message should be Assistant");
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
}
