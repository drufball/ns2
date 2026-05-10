use db::HookStore;
use events::EventBus;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use uuid::Uuid;

/// Central application state shared across all request handlers.
///
/// Owns the message-sender map (mpsc channels for delivering messages to live
/// harness tasks), the global event bus, and supporting infrastructure. This is
/// the single source of truth for all maps; no other module may hold a mutable
/// reference to them.
#[derive(Clone)]
pub struct AppState {
    pub(crate) db: Arc<dyn db::Db>,
    pub(crate) issue_service: issues::IssueService,
    /// Maps session id → mpsc sender for delivering messages to the live harness.
    pub(crate) msg_senders:
        Arc<tokio::sync::Mutex<HashMap<Uuid, tokio::sync::mpsc::Sender<String>>>>,
    /// Set of session ids for which a harness spawn is currently in flight.
    pub(crate) spawning: Arc<tokio::sync::Mutex<HashSet<Uuid>>>,
    pub(crate) client: Arc<dyn anthropic::AnthropicClient>,
    pub(crate) tools: Vec<Arc<dyn tools::Tool>>,
    pub(crate) model: String,
    /// Global event bus.  All session and issue events flow through this bus.
    pub(crate) event_bus: EventBus,
    /// Hook store for CRUD operations on hooks.
    pub(crate) hook_store: Arc<dyn HookStore>,
    /// Event store for CRUD operations on named events.
    pub(crate) event_store: Arc<dyn db::EventStore>,
}
