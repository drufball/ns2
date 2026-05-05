use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use events::{EventBus, SessionEvent};
use uuid::Uuid;

/// Central application state shared across all request handlers.
///
/// Owns the session registry (broadcast channels for SSE streaming), the
/// message-sender map (mpsc channels for delivering messages to live harness
/// tasks), and the global event bus. This is the single source of truth for
/// all maps; no other module may hold a mutable reference to them.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Arc<dyn db::Db>,
    pub(crate) issue_service: issues::IssueService,
    /// Maps session id → broadcast sender for SSE streaming (kept for backward
    /// compat with `/sessions/:id/events`; will be removed in the next issue).
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
    /// Global event bus.  All session events are wrapped in `SystemEvent::Session`
    /// and published here in addition to the per-session channel.
    pub(crate) event_bus: EventBus,
}
