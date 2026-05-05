use chrono::Utc;
use std::sync::Arc;
use tokio::sync::broadcast;
use types::{ContentBlock, ContentBlockDelta, Role, Turn};
use events::SessionEvent;
use uuid::Uuid;

/// Load conversation history from the DB for a session.
/// Returns turns in order, each as `(Role, Vec<ContentBlock>)`.
/// Turns with mixed roles are grouped by the role stored on each block;
/// consecutive blocks with the same role are merged into one entry.
pub(crate) async fn load_history(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
) -> crate::Result<Vec<(Role, Vec<ContentBlock>)>> {
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
pub(crate) async fn persist_user_message(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
    message: &str,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> crate::Result<Turn> {
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
