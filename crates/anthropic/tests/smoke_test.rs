use anthropic::{AnthropicClient, Client, MessageRequest};
use types::{ContentBlock, Role};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FAKE_SSE: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "data: {\"type\":\"ping\"}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from mock!\"}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

fn make_request() -> MessageRequest {
    MessageRequest {
        model: "claude-opus-4-5".into(),
        system: None,
        messages: vec![(Role::User, vec![ContentBlock::Text { text: "hello".into() }])],
        max_tokens: 1024,
    }
}

#[tokio::test]
async fn smoke_test_anthropic_client() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(FAKE_SSE),
        )
        .mount(&mock_server)
        .await;

    let client = Client::with_base_url("test-key".into(), mock_server.uri());
    let response = client.complete(make_request()).await.unwrap();
    assert!(!response.content.is_empty());
    if let ContentBlock::Text { text } = &response.content[0] {
        assert_eq!(text, "Hello from mock!");
    } else {
        panic!("expected text block");
    }
    assert_eq!(response.stop_reason, "end_turn");
    assert_eq!(response.input_tokens, 10);
    assert_eq!(response.output_tokens, 5);
}

#[tokio::test]
async fn test_empty_content_response() {
    // A valid stream with no content_block_delta events -> empty content vec
    let empty_sse = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":0}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(empty_sse),
        )
        .mount(&mock_server)
        .await;

    let client = Client::with_base_url("test-key".into(), mock_server.uri());
    let response = client.complete(make_request()).await.unwrap();
    assert!(response.content.is_empty(), "expected no content blocks");
    assert_eq!(response.stop_reason, "end_turn");
}

#[tokio::test]
async fn test_server_error_returns_err() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string("{\"error\":{\"message\":\"invalid api key\"}}"),
        )
        .mount(&mock_server)
        .await;

    let client = Client::with_base_url("bad-key".into(), mock_server.uri());
    let result = client.complete(make_request()).await;
    assert!(result.is_err(), "expected error for non-200 response");
}

#[tokio::test]
async fn test_client_new_does_not_panic() {
    // Just verify construction doesn't panic
    let _client = Client::new("any-key".into());
}

#[tokio::test]
async fn test_client_with_base_url_does_not_panic() {
    let _client = Client::with_base_url("any-key".into(), "http://localhost:9999".into());
}

#[tokio::test]
async fn test_multi_block_response() {
    let multi_block_sse = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"First block\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Second block\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":6}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(multi_block_sse),
        )
        .mount(&mock_server)
        .await;

    let client = Client::with_base_url("test-key".into(), mock_server.uri());
    let response = client.complete(make_request()).await.unwrap();
    assert_eq!(response.content.len(), 2, "expected 2 content blocks");
    assert!(matches!(&response.content[0], ContentBlock::Text { text } if text == "First block"));
    assert!(matches!(&response.content[1], ContentBlock::Text { text } if text == "Second block"));
}
