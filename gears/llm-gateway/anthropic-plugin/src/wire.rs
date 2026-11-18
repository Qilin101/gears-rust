// Created: 2026-11-09 by Constructor Tech
//! Anthropic Messages API wire types (`POST /v1/messages`).
//!
//! Only the shapes the plugin builds or inspects are typed. Response content
//! blocks stay raw JSON and are decoded per `type` ([`Block::parse`]), so
//! unknown and passthrough blocks keep every field.

use oagw_sdk::error::StreamingError;
use oagw_sdk::sse::{FromServerEvent, ServerEvent};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::settings::{AnthropicOutputEffort, AnthropicServiceTier, AnthropicThinkingDisplay};

/// API version sent when the model settings leave `anthropic_version` empty.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2026-10-21";

/// Path of the Messages endpoint under the OAGW upstream alias.
pub const MESSAGES_PATH: &str = "v1/messages";

/// Server tool name of Anthropic web search.
pub const WEB_SEARCH_TOOL_NAME: &str = "web_search";

/// Web search tool version the Gateway web search tool maps to.
pub const WEB_SEARCH_TOOL_TYPE: &str = "web_search_20250305";

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// `POST /v1/messages` request body.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<MessageParam>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub system: Vec<ContentBlockParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Custom, built-in, and passthrough tools; their shapes differ, so they
    /// stay JSON.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<AnthropicServiceTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_geo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Provider-specific top-level fields the plugin does not build.
    #[serde(skip)]
    pub extra: Map<String, Value>,
}

impl MessagesRequest {
    /// The JSON body sent to Anthropic.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] when the request cannot be serialized.
    pub fn to_body(&self) -> Result<Value, serde_json::Error> {
        let mut body = serde_json::to_value(self)?;
        if let Value::Object(map) = &mut body {
            for (key, value) in &self.extra {
                map.insert(key.clone(), value.clone());
            }
        }
        Ok(body)
    }
}

/// Message author role on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One conversation message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MessageParam {
    pub role: Role,
    pub content: Vec<ContentBlockParam>,
}

/// Request content block.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockParam {
    Text {
        text: String,
    },
    Image {
        source: MediaSource,
    },
    Document {
        source: MediaSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlockParam>,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    /// A block forwarded verbatim (an `anthropic:<type>` item with the prefix
    /// stripped).
    #[serde(untagged)]
    Raw(Value),
}

impl ContentBlockParam {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// Image or document source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// `tool_choice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolChoice {
    Auto {
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Any {
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Tool {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    None,
}

/// `thinking`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Thinking {
    Enabled {
        budget_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<AnthropicThinkingDisplay>,
    },
    Disabled,
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<AnthropicThinkingDisplay>,
    },
}

/// `output_config`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct OutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<AnthropicOutputEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<OutputFormat>,
}

/// `output_config.format`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputFormat {
    JsonSchema { schema: Value },
}

/// `metadata`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Metadata {
    pub user_id: String,
}

/// `cache_control`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CacheControl {
    Ephemeral,
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// `Message` response object.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Details of a `refusal` stop: `category` and `explanation`.
    #[serde(default)]
    pub stop_details: Option<Value>,
    #[serde(default)]
    pub usage: Value,
}

/// Token counts read from a raw `usage` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct UsageCounts {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens_details: Option<OutputTokensDetails>,
}

impl UsageCounts {
    /// Counts from a raw `usage` object; absent or malformed fields are `None`.
    #[must_use]
    pub fn from_raw(raw: &Value) -> Self {
        Self::deserialize(raw).unwrap_or_default()
    }
}

/// `usage.output_tokens_details`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct OutputTokensDetails {
    #[serde(default)]
    pub thinking_tokens: u64,
}

/// A response content block, decoded by `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text {
        text: String,
        citations: Vec<Value>,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// `server_tool_use` for web search.
    WebSearchCall {
        id: String,
        query: Option<String>,
    },
    /// `web_search_tool_result`.
    WebSearchResult {
        tool_use_id: String,
        results: Vec<WebSearchResultItem>,
        failed: bool,
    },
    /// Any other block, kept verbatim.
    Other(Value),
}

/// One `web_search_result` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WebSearchResultItem {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Deserialize)]
struct TextFields {
    #[serde(default)]
    text: String,
    #[serde(default)]
    citations: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct ThinkingFields {
    #[serde(default)]
    thinking: String,
    #[serde(default)]
    signature: String,
}

#[derive(Deserialize)]
struct RedactedFields {
    data: String,
}

#[derive(Deserialize)]
struct ToolUseFields {
    id: String,
    name: String,
    #[serde(default)]
    input: Value,
}

#[derive(Deserialize)]
struct WebSearchResultFields {
    tool_use_id: String,
    #[serde(default)]
    content: Value,
}

impl Block {
    /// Decode a raw content block; blocks that do not match their expected
    /// shape become [`Block::Other`].
    #[must_use]
    pub fn parse(raw: Value) -> Self {
        let kind = raw.get("type").and_then(Value::as_str).unwrap_or_default();
        let decoded = match kind {
            "text" => TextFields::deserialize(&raw).ok().map(|f| Self::Text {
                text: f.text,
                citations: f.citations.unwrap_or_default(),
            }),
            "thinking" => ThinkingFields::deserialize(&raw)
                .ok()
                .map(|f| Self::Thinking {
                    thinking: f.thinking,
                    signature: f.signature,
                }),
            "redacted_thinking" => RedactedFields::deserialize(&raw)
                .ok()
                .map(|f| Self::RedactedThinking { data: f.data }),
            "tool_use" => ToolUseFields::deserialize(&raw)
                .ok()
                .map(|f| Self::ToolUse {
                    id: f.id,
                    name: f.name,
                    input: f.input,
                }),
            "server_tool_use" => ToolUseFields::deserialize(&raw)
                .ok()
                .filter(|f| f.name == WEB_SEARCH_TOOL_NAME)
                .map(|f| Self::WebSearchCall {
                    id: f.id,
                    query: f
                        .input
                        .get("query")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }),
            "web_search_tool_result" => {
                WebSearchResultFields::deserialize(&raw)
                    .ok()
                    .map(|f| Self::WebSearchResult {
                        tool_use_id: f.tool_use_id,
                        failed: !f.content.is_array(),
                        results: Vec::<WebSearchResultItem>::deserialize(&f.content)
                            .unwrap_or_default(),
                    })
            }
            _ => None,
        };
        decoded.unwrap_or(Self::Other(raw))
    }
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// A Messages API SSE event, decoded from the event `data`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: Message,
    },
    MessageDelta {
        #[serde(default)]
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Value,
    },
    MessageStop,
    ContentBlockStart {
        index: u32,
        content_block: Value,
    },
    ContentBlockDelta {
        index: u32,
        delta: BlockDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    Error {
        error: ErrorObject,
    },
    /// `ping` and event types this plugin does not handle.
    #[serde(other)]
    Ignored,
}

/// `message_delta.delta`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct MessageDeltaBody {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_details: Option<Value>,
}

/// `content_block_delta.delta`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    CitationsDelta {
        citation: Value,
    },
    #[serde(other)]
    Ignored,
}

impl FromServerEvent for StreamEvent {
    fn from_server_event(event: ServerEvent) -> Result<Self, StreamingError> {
        if event.data.trim().is_empty() {
            return Ok(Self::Ignored);
        }
        event.json().map_err(|e| StreamingError::ServerEventsParse {
            detail: format!("anthropic stream event: {e}"),
        })
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Error response body: `{"type": "error", "error": {...}, "request_id": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorObject,
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Error object of an error response or an SSE `error` event.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ErrorObject {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub message: String,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "wire_tests.rs"]
mod wire_tests;
