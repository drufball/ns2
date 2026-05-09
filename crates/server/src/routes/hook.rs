use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use db::Error as StoreError;
use hooks::{generate_hook_id, Hook, HookAction, HookFilter, HookSource};
use serde::Deserialize;

use crate::state::AppState;

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateHookRequest {
    pub name: String,
    pub source: HookSource,
    pub filter: Option<HookFilter>,
    pub action: HookAction,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub created_by: Option<String>,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateHookRequest {
    pub name: Option<String>,
    pub action: Option<HookAction>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ListHooksQuery {
    pub enabled: Option<bool>,
    pub source_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListExecutionsQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
}

const fn default_limit() -> usize {
    20
}

// ── Error conversion ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct HookApiError(StoreError);

impl IntoResponse for HookApiError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let (status, msg) = match &self.0 {
            StoreError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            e => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<StoreError> for HookApiError {
    fn from(e: StoreError) -> Self {
        Self(e)
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn create_hook(
    State(state): State<AppState>,
    Json(req): Json<CreateHookRequest>,
) -> impl IntoResponse {
    // Validate timer hook schedule
    if let HookSource::Timer { ref schedule } = req.source {
        if let Err(e) = hooks::cron::next_after(schedule, chrono::Utc::now()) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    }

    let hook = Hook {
        id: generate_hook_id(),
        name: req.name,
        source: req.source,
        filter: req.filter,
        action: req.action,
        enabled: req.enabled,
        created_by: req.created_by,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    if let Err(e) = state.hook_store.create_hook(&hook).await {
        return HookApiError(e).into_response();
    }

    (StatusCode::CREATED, Json(hook)).into_response()
}

pub async fn list_hooks(
    State(state): State<AppState>,
    Query(q): Query<ListHooksQuery>,
) -> impl IntoResponse {
    match state
        .hook_store
        .list_hooks(q.enabled, q.source_type.as_deref())
        .await
    {
        Ok(hooks) => Json(hooks).into_response(),
        Err(e) => HookApiError(e).into_response(),
    }
}

pub async fn get_hook(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match state.hook_store.get_hook(&id).await {
        Ok(hook) => Json(hook).into_response(),
        Err(e) => HookApiError(e).into_response(),
    }
}

pub async fn update_hook(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateHookRequest>,
) -> impl IntoResponse {
    let mut hook = match state.hook_store.get_hook(&id).await {
        Ok(h) => h,
        Err(e) => return HookApiError(e).into_response(),
    };

    if let Some(name) = req.name {
        hook.name = name;
    }
    if let Some(action) = req.action {
        hook.action = action;
    }
    if let Some(enabled) = req.enabled {
        hook.enabled = enabled;
    }
    // filter: explicit null clears it, absent leaves unchanged
    // We can't easily distinguish absent vs explicit null with Option<serde_json::Value>
    // without a custom deserializer. Use a sentinel: if the key is present, apply.
    // For now: if filter field is present in the JSON (even null), treat it.
    // Since Option<Value> will be None for both absent and null, we skip here.
    // Callers that want to clear filter should send the full hook update.

    hook.updated_at = Utc::now();

    if let Err(e) = state.hook_store.update_hook(&hook).await {
        return HookApiError(e).into_response();
    }

    Json(hook).into_response()
}

pub async fn delete_hook(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.hook_store.delete_hook(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => HookApiError(e).into_response(),
    }
}

pub async fn list_executions(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ListExecutionsQuery>,
) -> impl IntoResponse {
    // First verify the hook exists
    if let Err(e) = state.hook_store.get_hook(&id).await {
        return HookApiError(e).into_response();
    }
    match state.hook_store.list_executions(&id, q.limit).await {
        Ok(execs) => Json(execs).into_response(),
        Err(e) => HookApiError(e).into_response(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // Hook route integration tests are in server/src/lib.rs test module.
}
