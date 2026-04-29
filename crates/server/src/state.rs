use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use types::SessionEvent;
use uuid::Uuid;

/// Central application state shared across all request handlers.
///
/// Owns the session registry (broadcast channels for SSE streaming) and the
/// message-sender map (mpsc channels for delivering messages to live harness
/// tasks). This is the single source of truth for both maps; no other module
/// may hold a mutable reference to them.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Arc<dyn db::Db>,
    pub(crate) issue_service: issues::IssueService,
    /// Maps session id → broadcast sender for SSE streaming.
    pub(crate) sessions:
        Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::broadcast::Sender<SessionEvent>>>>,
    /// Maps session id → mpsc sender for delivering messages to the live harness.
    pub(crate) msg_senders:
        Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::mpsc::Sender<String>>>>,
    /// Set of session ids for which a harness spawn is currently in flight.
    pub(crate) spawning: Arc<tokio::sync::Mutex<HashSet<Uuid>>>,
    pub(crate) client: Arc<dyn anthropic::AnthropicClient>,
    pub(crate) tools: Vec<Arc<dyn tools::Tool>>,
    pub(crate) model: String,
}
