use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use db::EventKind;

use crate::state::AppState;

/// `POST /webhooks/:event_id`
///
/// Receives an external webhook event, validates the HMAC signature (if the
/// named event has a secret configured), parses the payload and publishes a
/// [`events::SystemEvent::External`] to the event bus.
pub async fn receive_webhook(
    State(state): State<AppState>,
    Path(event_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // 1. Look up the NamedEvent.
    let Ok(event) = state.event_store.get_event(&event_id).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        )
            .into_response();
    };

    // 2. Must be a Webhook kind event.
    let EventKind::Webhook { ref secret } = event.kind else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        )
            .into_response();
    };

    // 3. Must be enabled.
    if !event.enabled {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        )
            .into_response();
    }

    let secret = secret.clone();

    // 4. HMAC verification (only when secret is Some).
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
        let Some(hex_digest) = sig_header.strip_prefix("sha256=") else {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "missing or invalid signature" })),
            )
                .into_response();
        };

        // Decode hex digest.
        let Ok(expected_bytes) = hex::decode(hex_digest) else {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "invalid signature encoding" })),
            )
                .into_response();
        };

        // Compute HMAC and verify in constant time.
        let Ok(mut mac) = HmacSha256::new_from_slice(secret_str.as_bytes()) else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "internal error" })),
            )
                .into_response();
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

    // 5. Parse body as JSON.
    let Ok(payload): Result<serde_json::Value, _> = serde_json::from_slice(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid JSON payload" })),
        )
            .into_response();
    };

    // 6. Emit event.
    state.event_bus.send(events::SystemEvent::External {
        event_id: event.id.clone(),
        event_name: event.name.clone(),
        payload,
    });

    // 7. Return 200 OK.
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}
