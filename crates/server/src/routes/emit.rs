use axum::{extract::State, Json};
use serde::Deserialize;

use crate::state::AppState;
use crate::routes::Result;

/// Request body for `POST /events/emit`.
#[derive(Debug, Deserialize)]
pub struct EmitEventRequest {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// `POST /events/emit` — emit a custom event on the global event bus.
///
/// Emits a `SystemEvent::Custom { event_type, payload }` so that any subscriber
/// (hooks, SSE clients) can react to it.
pub async fn emit_event(
    State(state): State<AppState>,
    Json(req): Json<EmitEventRequest>,
) -> Result<Json<serde_json::Value>> {
    state.event_bus.send(events::SystemEvent::Custom {
        event_type: req.event_type,
        payload: req.payload,
    });
    Ok(Json(serde_json::json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // ── Scenario 4: POST /events/emit returns 200 ─────────────────────────────

    #[tokio::test]
    async fn post_events_emit_returns_200() {
        let state = crate::tests::test_state().await;
        let app = crate::build_router(state.clone());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/events/emit")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"type":"custom.test","payload":{"key":"value"}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let body =
            axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
    }

    // ── Scenario 5: POST /events/emit emits SystemEvent::Custom on bus ────────

    #[tokio::test]
    async fn post_events_emit_sends_custom_event_on_bus() {
        let state = crate::tests::test_state().await;
        let mut rx = state.event_bus.subscribe();
        let app = crate::build_router(state.clone());

        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/events/emit")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"type":"custom.test","payload":{"answer":42}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

        let event = rx.try_recv().expect("should have received a custom event");
        match event {
            events::SystemEvent::Custom {
                ref event_type,
                ref payload,
            } => {
                assert_eq!(event_type, "custom.test");
                assert_eq!(payload["answer"], 42);
            }
            other => panic!("expected Custom event, got: {other:?}"),
        }
    }

    // ── Scenario 4b: POST /events/emit with empty payload ────────────────────

    #[tokio::test]
    async fn post_events_emit_with_no_payload_defaults_to_empty_object() {
        let state = crate::tests::test_state().await;
        let mut rx = state.event_bus.subscribe();
        let app = crate::build_router(state.clone());

        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/events/emit")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"type":"custom.test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

        let event = rx.try_recv().expect("should have received a custom event");
        match event {
            events::SystemEvent::Custom {
                ref event_type,
                ref payload,
            } => {
                assert_eq!(event_type, "custom.test");
                assert!(
                    payload.is_null() || payload.as_object().is_some_and(|o| o.is_empty()),
                    "payload should be null or empty object when not specified, got: {payload}"
                );
            }
            other => panic!("expected Custom event, got: {other:?}"),
        }
    }
}
