use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use types::{ContentBlock, Role};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("api error: {status} {message}")]
    Api { status: u16, message: String },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct MessageRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<(types::Role, Vec<types::ContentBlock>)>,
    pub max_tokens: u32,
    pub tools: Vec<types::ToolDefinition>,
}

#[derive(Debug, Clone)]
pub struct MessageResponse {
    pub content: Vec<types::ContentBlock>,
    pub stop_reason: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[async_trait::async_trait]
pub trait AnthropicClient: Send + Sync {
    async fn complete(&self, request: MessageRequest) -> Result<MessageResponse>;
}

pub struct Client {
    api_key: String,
    base_url: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://api.anthropic.com".into(),
            http: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(api_key: String, base_url: String) -> Self {
        Self {
            api_key,
            base_url,
            http: reqwest::Client::new(),
        }
    }
}

// --- Wire protocol types for Anthropic SSE events ---

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiEvent {
    MessageStart { message: ApiMessage },
    ContentBlockStart { index: u32, content_block: ApiContentBlock },
    Ping,
    ContentBlockDelta { index: u32, delta: ApiDelta },
    ContentBlockStop { index: u32 },
    MessageDelta { delta: ApiMessageDelta, usage: ApiUsage },
    MessageStop,
    #[allow(dead_code)]
    Error { error: ApiError },
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    usage: ApiUsage,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    #[serde(default)]
    input_tokens: u32,
    output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiContentBlock {
    Text {
        #[allow(dead_code)]
        text: String,
    },
    ToolUse { id: String, name: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ApiMessageDelta {
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ApiError {
    message: String,
}

// --- Request body types ---

#[derive(Debug, Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ApiRequestMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiToolDefinition>,
}

#[derive(Debug, Serialize)]
struct ApiToolDefinition {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

impl From<types::ToolDefinition> for ApiToolDefinition {
    fn from(t: types::ToolDefinition) -> Self {
        Self {
            name: t.name,
            description: t.description,
            input_schema: t.input_schema,
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiRequestMessage {
    role: String,
    content: Vec<ApiContent>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiContent {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
}

// --- Block assembler ---

struct ToolUseAccumulator {
    id: String,
    name: String,
    partial_json: String,
}

struct BlockAssembler {
    texts: HashMap<u32, String>,
    tool_uses: HashMap<u32, ToolUseAccumulator>,
    finished_tool_uses: Vec<(u32, ContentBlock)>,
    input_tokens: u32,
    output_tokens: u32,
    stop_reason: String,
}

impl BlockAssembler {
    fn new() -> Self {
        Self {
            texts: Default::default(),
            tool_uses: Default::default(),
            finished_tool_uses: Default::default(),
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: "end_turn".into(),
        }
    }

    fn process(&mut self, event: ApiEvent) -> Result<()> {
        match event {
            ApiEvent::MessageStart { message } => {
                self.input_tokens = message.usage.input_tokens;
            }
            ApiEvent::ContentBlockStart {
                index,
                content_block: ApiContentBlock::ToolUse { id, name },
            } => {
                self.tool_uses.insert(index, ToolUseAccumulator { id, name, partial_json: String::new() });
            }
            ApiEvent::ContentBlockStart { .. } => {}
            ApiEvent::ContentBlockDelta {
                index,
                delta: ApiDelta::TextDelta { text },
            } => {
                self.texts.entry(index).or_default().push_str(&text);
            }
            ApiEvent::ContentBlockDelta {
                index,
                delta: ApiDelta::InputJsonDelta { partial_json },
            } => {
                if let Some(acc) = self.tool_uses.get_mut(&index) {
                    acc.partial_json.push_str(&partial_json);
                }
            }
            ApiEvent::ContentBlockStop { index } => {
                if let Some(acc) = self.tool_uses.remove(&index) {
                    let input = serde_json::from_str(&acc.partial_json)
                        .map_err(|e| Error::Parse(format!("invalid tool input JSON: {e}")))?;
                    self.finished_tool_uses.push((
                        index,
                        ContentBlock::ToolUse { id: acc.id, name: acc.name, input },
                    ));
                }
            }
            ApiEvent::MessageDelta { delta, usage } => {
                if let Some(reason) = delta.stop_reason {
                    self.stop_reason = reason;
                }
                if let Some(out) = usage.output_tokens {
                    self.output_tokens = out;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> MessageResponse {
        let mut blocks: Vec<(u32, ContentBlock)> = self
            .texts
            .into_iter()
            .map(|(i, text)| (i, ContentBlock::Text { text }))
            .collect();
        blocks.extend(self.finished_tool_uses);
        blocks.sort_by_key(|(i, _)| *i);
        MessageResponse {
            content: blocks.into_iter().map(|(_, b)| b).collect(),
            stop_reason: self.stop_reason,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        }
    }
}

// --- AnthropicClient impl ---

#[async_trait::async_trait]
impl AnthropicClient for Client {
    async fn complete(&self, request: MessageRequest) -> Result<MessageResponse> {
        let messages: Vec<ApiRequestMessage> = request
            .messages
            .into_iter()
            .map(|(role, blocks)| {
                let content = blocks
                    .into_iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => ApiContent::Text { text },
                        ContentBlock::ToolUse { id, name, input } => {
                            ApiContent::ToolUse { id, name, input }
                        }
                        ContentBlock::ToolResult { tool_use_id, content } => {
                            ApiContent::ToolResult { tool_use_id, content }
                        }
                    })
                    .collect();
                ApiRequestMessage {
                    role: match role {
                        Role::User => "user".into(),
                        Role::Assistant => "assistant".into(),
                    },
                    content,
                }
            })
            .collect();

        let tools: Vec<ApiToolDefinition> = request.tools.into_iter().map(Into::into).collect();

        let body = ApiRequest {
            model: request.model,
            max_tokens: request.max_tokens,
            system: request.system,
            messages,
            stream: true,
            tools,
        };

        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(Error::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Api { status, message: text });
        }

        // Parse SSE stream
        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut assembler = BlockAssembler::new();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(Error::Http)?;
            if let Ok(s) = std::str::from_utf8(&chunk) {
                buffer.push_str(s);
            }
            while let Some(pos) = buffer.find("\n\n") {
                let line = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();
                for part in line.lines() {
                    if let Some(data) = part.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            return Ok(assembler.finish());
                        }
                        if let Ok(event) = serde_json::from_str::<ApiEvent>(data) {
                            assembler.process(event)?;
                        }
                    }
                }
            }
        }

        Ok(assembler.finish())
    }
}
