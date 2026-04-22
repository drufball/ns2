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
        tools: vec![],
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

#[tokio::test]
async fn test_tool_use_streaming_response() {
    // Simulate Anthropic streaming a tool_use block with input_json_delta events
    let tool_use_sse = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":0}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"read\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"/tmp/test.txt\\\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":15}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_use_sse),
        )
        .mount(&mock_server)
        .await;

    let mut req = make_request();
    req.tools = vec![types::ToolDefinition {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    }];

    let client = Client::with_base_url("test-key".into(), mock_server.uri());
    let response = client.complete(req).await.unwrap();

    assert_eq!(response.stop_reason, "tool_use");
    assert_eq!(response.content.len(), 1, "expected 1 content block");
    match &response.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "toolu_01");
            assert_eq!(name, "read");
            assert_eq!(input["path"], "/tmp/test.txt");
        }
        other => panic!("expected ToolUse block, got: {:?}", other),
    }
    assert_eq!(response.input_tokens, 20);
    assert_eq!(response.output_tokens, 15);
}

#[tokio::test]
async fn test_tool_definitions_sent_in_request() {
    // Verify that tool definitions are included in the request body
    use wiremock::matchers::body_partial_json;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(serde_json::json!({
            "tools": [
                {
                    "name": "read",
                    "description": "Read the contents of a file",
                    "input_schema": {"type": "object"}
                }
            ]
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(FAKE_SSE),
        )
        .mount(&mock_server)
        .await;

    let mut req = make_request();
    req.tools = vec![types::ToolDefinition {
        name: "read".into(),
        description: "Read the contents of a file".into(),
        input_schema: serde_json::json!({"type": "object"}),
    }];

    let client = Client::with_base_url("test-key".into(), mock_server.uri());
    // If the mock doesn't match (no tools in body), wiremock returns 404 and we'd get an error
    let response = client.complete(req).await.unwrap();
    assert!(!response.content.is_empty());
}

#[tokio::test]
async fn test_malformed_tool_input_json_returns_error() {
    // A stream with an invalid partial_json fragment should return an error,
    // not a ContentBlock with empty input.
    let bad_json_sse = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_bad\",\"name\":\"read\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{invalid\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(bad_json_sse),
        )
        .mount(&mock_server)
        .await;

    let client = Client::with_base_url("test-key".into(), mock_server.uri());
    let result = client.complete(make_request()).await;
    assert!(result.is_err(), "expected error for malformed tool input JSON, got: {:?}", result);
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("parse error") || err_str.contains("invalid tool input JSON"),
        "expected parse error in message, got: {err_str}"
    );
}

#[tokio::test]
async fn test_text_content_block_start_does_not_corrupt_output() {
    // ContentBlockStart for a text block hits the catch-all arm in BlockAssembler::process.
    // This test ensures that arm is present and harmless: the text assembled from deltas
    // must match exactly, with no extra empty block inserted by a mishandled start event.
    let text_block_sse = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":8,\"output_tokens\":0}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(text_block_sse),
        )
        .mount(&mock_server)
        .await;

    let client = Client::with_base_url("test-key".into(), mock_server.uri());
    let response = client.complete(make_request()).await.unwrap();

    assert_eq!(response.content.len(), 1, "expected exactly one content block");
    match &response.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "hello world"),
        other => panic!("expected Text block, got: {:?}", other),
    }
    assert_eq!(response.input_tokens, 8);
    assert_eq!(response.output_tokens, 3);
}

#[tokio::test]
async fn test_tool_result_in_message_history_serializes_correctly() {
    // Verify that ToolResult content blocks serialize with the correct wire format
    use wiremock::matchers::body_partial_json;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(serde_json::json!({
            "messages": [
                {
                    "role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "toolu_01", "content": "file contents here"}]
                }
            ]
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(FAKE_SSE),
        )
        .mount(&mock_server)
        .await;

    let req = MessageRequest {
        model: "claude-opus-4-5".into(),
        system: None,
        messages: vec![(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_01".into(),
                content: "file contents here".into(),
            }],
        )],
        max_tokens: 1024,
        tools: vec![],
    };

    let client = Client::with_base_url("test-key".into(), mock_server.uri());
    let response = client.complete(req).await.unwrap();
    assert!(!response.content.is_empty());
}
