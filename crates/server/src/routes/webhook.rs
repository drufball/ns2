use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use hooks::HookSource;

use crate::state::AppState;

/// `POST /webhooks/:hook_id`
///
/// Receives an external webhook event, validates the HMAC signature (if the
/// hook has a secret configured), parses the payload and publishes a
/// [`events::SystemEvent::External`] to the event bus.
pub async fn receive_webhook(
    State(state): State<AppState>,
    Path(hook_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // 1. Look up the hook.
    let hook = match state.hook_store.get_hook(&hook_id).await {
        Ok(h) => h,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "not found" })),
            )
                .into_response();
        }
    };

    // 2. Must be an External hook that is enabled.
    let secret = match &hook.source {
        HookSource::External { secret } => {
            if !hook.enabled {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "not found" })),
                )
                    .into_response();
            }
            secret.clone()
        }
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "not found" })),
            )
                .into_response();
        }
    };

    // Also check enabled (in case it slipped through above — belt-and-suspenders).
    if !hook.enabled {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        )
            .into_response();
    }

    // 3. HMAC verification (only when secret is Some).
    if let Some(secret_str) = &secret {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        // Read the X-Hub-Signature-256 header.
        let sig_header = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // Must be "sha256=<hex>"
        let hex_digest = match sig_header.strip_prefix("sha256=") {
            Some(h) => h,
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "missing or invalid signature" })),
                )
                    .into_response();
            }
        };

        // Decode hex digest.
        let expected_bytes = match hex::decode(hex_digest) {
            Ok(b) => b,
            Err(_) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "invalid signature encoding" })),
                )
                    .into_response();
            }
        };

        // Compute HMAC and verify in constant time.
        let mut mac = match HmacSha256::new_from_slice(secret_str.as_bytes()) {
            Ok(m) => m,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal error" })),
                )
                    .into_response();
            }
        };
        mac.update(&body);
        if mac.verify_slice(&expected_bytes).is_err() {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "signature mismatch" })),
            )
                .into_response();
        }
    }

    // 4. Parse body as JSON.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid JSON payload" })),
            )
                .into_response();
        }
    };

    // 5. Emit event.
    state
        .event_bus
        .send(events::SystemEvent::External { hook_id, payload });

    // 6. Return 200 OK.
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}
