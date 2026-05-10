use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use db::Error as StoreError;
use hooks::generate_event_id;
use serde::Deserialize;
use types::{Event, EventKind};

use crate::state::AppState;

// ── Request types ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateNamedEventRequest {
    pub name: String,
    pub kind: EventKind,
    pub description: Option<String>,
}

// ── Error conversion ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct EventApiError(StoreError);

impl IntoResponse for EventApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, msg) = match &self.0 {
            StoreError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            e => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<StoreError> for EventApiError {
    fn from(e: StoreError) -> Self {
        Self(e)
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `POST /named-events`
pub async fn create_event(
    State(state): State<AppState>,
    Json(req): Json<CreateNamedEventRequest>,
) -> impl IntoResponse {
    // Validate timer schedule
    if let EventKind::Timer { ref schedule } = req.kind {
        if let Err(e) = hooks::cron::next_after(schedule, Utc::now()) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("invalid cron schedule: {e}") })),
            )
                .into_response();
        }
    }

    let event = Event {
        id: generate_event_id(),
        name: req.name,
        kind: req.kind,
        description: req.description,
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    if let Err(e) = state.event_store.create_event(&event).await {
        return EventApiError(e).into_response();
    }

    (StatusCode::CREATED, Json(event)).into_response()
}

/// `GET /named-events`
pub async fn list_events(State(state): State<AppState>) -> impl IntoResponse {
    match state.event_store.list_events().await {
        Ok(events) => Json(events).into_response(),
        Err(e) => EventApiError(e).into_response(),
    }
}

/// `GET /named-events/:id`
pub async fn get_event(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.event_store.get_event(&id).await {
        Ok(event) => Json(event).into_response(),
        Err(e) => EventApiError(e).into_response(),
    }
}

/// `DELETE /named-events/:id`
pub async fn delete_event(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.event_store.delete_event(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => EventApiError(e).into_response(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // Named event route integration tests are in server/src/lib.rs test module.
}
