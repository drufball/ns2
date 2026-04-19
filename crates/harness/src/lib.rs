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
    Anthropic(String),
}

pub type Result<T> = std::result::Result<T, Error>;

// Message request/response types live here since they're the trait contract
#[derive(Debug, Clone)]
pub struct MessageRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<(types::Role, Vec<ContentBlock>)>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct MessageResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[async_trait]
pub trait AnthropicClient: Send + Sync {
    async fn complete(&self, request: MessageRequest) -> Result<MessageResponse>;
}

pub struct StubClient;

#[async_trait]
impl AnthropicClient for StubClient {
    async fn complete(&self, _request: MessageRequest) -> Result<MessageResponse> {
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
}

pub async fn run<D: db::Db + 'static>(
    config: HarnessConfig,
    client: Arc<dyn AnthropicClient>,
    db: Arc<D>,
    event_tx: broadcast::Sender<SessionEvent>,
    mut msg_rx: mpsc::Receiver<String>,
) -> Result<()> {
    // Wait for the initial message (already queued before run() is called)
    // Use try_recv since the message was put in the channel before spawn
    let initial_message = msg_rx.try_recv().unwrap_or_else(|_| "hello".into());

    // Persist user message as a turn before making the API call
    let user_turn = Turn {
        id: Uuid::new_v4(),
        session_id: config.session.id,
        token_count: None,
        created_at: Utc::now(),
    };
    db.create_turn(&user_turn).await?;
    let user_block = ContentBlock::Text { text: initial_message.clone() };
    db.create_content_block(user_turn.id, 0, &Role::User, &user_block).await?;
    let _ = event_tx.send(SessionEvent::TurnStarted { turn: user_turn.clone() });
    let _ = event_tx.send(SessionEvent::ContentBlockDelta {
        turn_id: user_turn.id,
        index: 0,
        delta: ContentBlockDelta::TextDelta { text: initial_message.clone() },
    });
    let _ = event_tx.send(SessionEvent::ContentBlockDone {
        turn_id: user_turn.id,
        index: 0,
        block: user_block,
    });
    let _ = event_tx.send(SessionEvent::TurnDone { turn_id: user_turn.id });

    let request = MessageRequest {
        model: config.model.clone(),
        system: config.system.clone(),
        messages: vec![(
            types::Role::User,
            vec![ContentBlock::Text { text: initial_message }],
        )],
        max_tokens: 4096,
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
        // Emit delta if it's text
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
    db.update_session_status(config.session.id, SessionStatus::Completed).await?;
    let _ = event_tx.send(SessionEvent::SessionDone { session_id: config.session.id });

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

    #[tokio::test]
    async fn test_run_with_stub_client() {
        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));

        let session = types::Session {
            id: Uuid::new_v4(),
            name: "test".into(),
            status: types::SessionStatus::Running,
            agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
        };

        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello world".into()).await.unwrap();

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

    // Helper: build a session and run harness with a MockTestDb
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

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();

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
        mock_db
            .expect_update_session_status()
            .withf(move |id, status| {
                *id == session_id && *status == types::SessionStatus::Completed
            })
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();

        run(config, client, db, event_tx, msg_rx).await.unwrap();
    }

    #[tokio::test]
    async fn test_run_emits_all_expected_event_types() {
        let session = make_session();

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();

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

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();

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
        };
        let response = client.complete(request).await.unwrap();
        assert_eq!(response.stop_reason, "end_turn");
    }

    // Test a client that returns multiple content blocks
    struct MultiBlockClient;

    #[async_trait]
    impl AnthropicClient for MultiBlockClient {
        async fn complete(&self, _request: MessageRequest) -> Result<MessageResponse> {
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

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        // 1 user block + 2 assistant blocks = 3 total
        mock_db
            .expect_create_content_block()
            .times(3)
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
        };
        let client = Arc::new(MultiBlockClient);
        let db = Arc::new(mock_db);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (msg_tx, msg_rx) = mpsc::channel(16);
        msg_tx.send("hello".into()).await.unwrap();

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
    async fn test_run_uses_fallback_message_when_channel_empty() {
        let session = make_session();

        let mut mock_db = MockTestDb::new();
        mock_db.expect_create_turn().returning(|_| Ok(()));
        mock_db
            .expect_create_content_block()
            .returning(|_, _, _, _| Ok(()));
        mock_db
            .expect_update_session_status()
            .returning(|_, _| Ok(()));

        let config = HarnessConfig {
            session: session.clone(),
            model: "claude-opus-4-5".into(),
            system: None,
        };
        let client = Arc::new(StubClient);
        let db = Arc::new(mock_db);
        let (event_tx, _rx) = broadcast::channel(64);
        let (_msg_tx, msg_rx) = mpsc::channel(16);
        // Don't send any message — run() should fall back to "hello"

        // Should not panic/error
        run(config, client, db, event_tx, msg_rx).await.unwrap();
    }
}
