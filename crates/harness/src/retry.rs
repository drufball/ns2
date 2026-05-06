use anthropic::{AnthropicClient, MessageRequest, MessageResponse};
use std::sync::Arc;

/// Read the max-retry count from `NS2_MAX_RETRIES` (default 5).
pub fn max_retries() -> u32 {
    std::env::var("NS2_MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(5)
}

/// Returns `true` if the error is an Anthropic 429 rate-limit response.
pub const fn is_rate_limit(err: &anthropic::Error) -> bool {
    matches!(err, anthropic::Error::Api { status: 429, .. })
}

/// Call `client.complete(request)` with exponential-backoff retry on 429.
///
/// - Retries up to `NS2_MAX_RETRIES` times (default 5).
/// - Initial delay: 10 s, doubling each retry, capped at 120 s.
/// - Non-429 errors propagate immediately without retrying.
///
/// In tests using `#[tokio::test(start_paused = true)]`, `tokio::time::sleep`
/// returns instantly, so no real wall-clock time is consumed.
pub async fn complete_with_retry(
    client: &Arc<dyn AnthropicClient>,
    request: MessageRequest,
) -> anthropic::Result<MessageResponse> {
    use tokio::time::{sleep, Duration};
    const MAX_DELAY_MS: u64 = 120_000;

    let retries = max_retries();
    let mut delay_ms: u64 = 10_000;

    let mut attempt = 0u32;
    loop {
        match client.complete(request.clone()).await {
            Ok(resp) => return Ok(resp),
            Err(err) if is_rate_limit(&err) && attempt < retries => {
                attempt += 1;
                tracing::warn!(
                    attempt,
                    retries,
                    delay_ms,
                    "Anthropic 429 rate-limit; retrying after {delay_ms}ms"
                );
                sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(MAX_DELAY_MS);
            }
            Err(err) => return Err(err),
        }
    }
}
