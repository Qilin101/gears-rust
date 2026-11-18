// Created: 2026-11-09 by Constructor Tech
//! Anthropic Messages response -> Open Responses response resource.
//!
//! [`OutputBuilder`] turns content blocks into output items for both the sync
//! and the streaming path, so a stream's final snapshot matches the sync
//! response for the same blocks.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use llm_gateway_sdk::{
    Annotation, CreateResponseBody, Extension, FunctionCallItem, IncompleteDetails,
    InputTokensDetails, ItemStatus, MessageOutput, OutputContentPart, OutputItem, OutputText,
    OutputTokensDetails, ProviderDetails, ReasoningContentPart, ReasoningOutput, ResponseResource,
    ResponseStatus, Role, ServiceTier, ToolChoice, ToolChoiceMode, TruncationStrategy, UrlCitation,
    Usage, WebSearchCallOutput, WebSearchResult,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};

use crate::request::ANTHROPIC_PREFIX;
use crate::wire::{Block, Message, UsageCounts};

/// Gateway-side facts of one call that the response echoes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Gateway request id; the response `id` and the output item id prefix.
    pub request_id: String,
    pub created_at: i64,
    pub ignored_parameters: Vec<String>,
    /// Anthropic's `request-id` response header.
    pub provider_request_id: Option<String>,
}

/// Current Unix time in seconds.
#[must_use]
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or_default()
}

/// A response resource with request echoes and no output yet.
#[must_use]
pub fn base_resource(
    body: &CreateResponseBody,
    envelope: &Envelope,
    status: ResponseStatus,
) -> ResponseResource {
    ResponseResource {
        id: envelope.request_id.clone(),
        object: "response".to_owned(),
        created_at: envelope.created_at,
        completed_at: None,
        status,
        incomplete_details: None,
        model: body.model.clone(),
        instructions: body.instructions.clone(),
        output: Vec::new(),
        error: None,
        tools: body.tools.clone().unwrap_or_default(),
        tool_choice: body
            .tool_choice
            .clone()
            .unwrap_or(ToolChoice::Mode(ToolChoiceMode::Auto)),
        truncation: body.truncation.unwrap_or(TruncationStrategy::Disabled),
        parallel_tool_calls: body.parallel_tool_calls.unwrap_or(true),
        text: body.text.clone().unwrap_or_default(),
        top_p: body.top_p.unwrap_or(1.0),
        presence_penalty: body.presence_penalty.unwrap_or_default(),
        frequency_penalty: body.frequency_penalty.unwrap_or_default(),
        top_logprobs: body.top_logprobs.unwrap_or_default(),
        temperature: body.temperature.unwrap_or(1.0),
        reasoning: body.reasoning.clone().unwrap_or_default(),
        usage: None,
        max_output_tokens: body.max_output_tokens,
        max_tool_calls: body.max_tool_calls,
        service_tier: match body.service_tier {
            Some(ServiceTier::Default) => "default",
            Some(ServiceTier::Auto) | None => "auto",
        }
        .to_owned(),
        metadata: body.metadata.clone(),
        safety_identifier: body.safety_identifier.clone(),
        prompt_cache_key: body.prompt_cache_key.clone(),
        ignored_parameters: envelope.ignored_parameters.clone(),
        provider_details: None,
    }
}

/// The final response resource for a sync Messages response.
#[must_use]
pub fn to_resource(
    body: &CreateResponseBody,
    envelope: &Envelope,
    message: Message,
) -> ResponseResource {
    let mut builder = OutputBuilder::new(&envelope.request_id);
    for block in message.content {
        builder.push(Block::parse(block));
    }
    finish(
        base_resource(body, envelope, ResponseStatus::Completed),
        envelope,
        builder.into_items(),
        message.stop_reason.as_deref(),
        &message.usage,
    )
}

/// Fill a resource with the output, status, usage, and provider details.
#[must_use]
pub fn finish(
    mut resource: ResponseResource,
    envelope: &Envelope,
    output: Vec<OutputItem>,
    stop_reason: Option<&str>,
    raw_usage: &Value,
) -> ResponseResource {
    let (status, incomplete_details) = status_for(stop_reason);
    resource.status = status;
    resource.incomplete_details = incomplete_details;
    resource.output = output;
    resource.usage = Some(usage(raw_usage, true));
    resource.completed_at = Some(now_unix());
    resource.provider_details = Some(provider_details(stop_reason, envelope));
    resource
}

#[must_use]
pub fn provider_details(stop_reason: Option<&str>, envelope: &Envelope) -> ProviderDetails {
    let mut details = ProviderDetails::default();
    if let Some(stop_reason) = stop_reason {
        details = details.with_stop_reason(stop_reason);
    }
    if let Some(id) = &envelope.provider_request_id {
        details = details.with_provider_request_id(id.clone());
    }
    details
}

/// Normalized status for an Anthropic `stop_reason`.
#[must_use]
pub fn status_for(stop_reason: Option<&str>) -> (ResponseStatus, Option<IncompleteDetails>) {
    let reason = match stop_reason {
        Some("max_tokens" | "model_context_window_exceeded") => {
            IncompleteDetails::MAX_OUTPUT_TOKENS
        }
        Some("refusal") => IncompleteDetails::CONTENT_FILTER,
        _ => return (ResponseStatus::Completed, None),
    };
    (
        ResponseStatus::Incomplete,
        Some(IncompleteDetails {
            reason: reason.to_owned(),
        }),
    )
}

/// Usage from a raw Anthropic `usage` object; `reported` is false for an
/// estimate from an interrupted stream. Cost is not computed yet.
#[must_use]
pub fn usage(raw: &Value, reported: bool) -> Usage {
    let counts = UsageCounts::from_raw(raw);
    let cache_read = counts.cache_read_input_tokens.unwrap_or_default();
    let input = counts
        .input_tokens
        .unwrap_or_default()
        .saturating_add(counts.cache_creation_input_tokens.unwrap_or_default())
        .saturating_add(cache_read);
    let output = counts.output_tokens.unwrap_or_default();
    let usage = if reported {
        Usage::reported(input, output, Decimal::ZERO)
    } else if input == 0 && output == 0 {
        Usage::unknown()
    } else {
        Usage::estimated(input, output, Decimal::ZERO)
    };
    usage
        .with_details(
            InputTokensDetails {
                cached_tokens: cache_read,
            },
            OutputTokensDetails {
                reasoning_tokens: counts
                    .output_tokens_details
                    .map(|d| d.thinking_tokens)
                    .unwrap_or_default(),
            },
        )
        .with_provider_raw(raw.clone())
}

/// `anthropic:<type>` copy of a raw value.
fn prefixed(raw: &Value) -> Value {
    let mut raw = raw.clone();
    if let Some(object) = raw.as_object_mut() {
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = format!("{ANTHROPIC_PREFIX}{kind}");
        object.insert("type".to_owned(), json!(kind));
    }
    raw
}

// ---------------------------------------------------------------------------
// OutputBuilder
// ---------------------------------------------------------------------------

/// What kind of block is about to be pushed, for [`OutputBuilder::peek`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind<'a> {
    Text,
    ToolUse,
    Reasoning,
    WebSearchCall,
    WebSearchResult { tool_use_id: &'a str },
    Other,
}

impl Block {
    #[must_use]
    pub fn kind(&self) -> BlockKind<'_> {
        match self {
            Self::Text { .. } => BlockKind::Text,
            Self::ToolUse { .. } => BlockKind::ToolUse,
            Self::Thinking { .. } | Self::RedactedThinking { .. } => BlockKind::Reasoning,
            Self::WebSearchCall { .. } => BlockKind::WebSearchCall,
            Self::WebSearchResult { tool_use_id, .. } => BlockKind::WebSearchResult { tool_use_id },
            Self::Other(_) => BlockKind::Other,
        }
    }
}

/// Where a block lands in the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub output_index: u32,
    pub item_id: String,
    /// Index of the content part within a message item (text blocks only).
    pub content_index: u32,
    /// Whether the block starts a new output item.
    pub new_item: bool,
}

/// Accumulates output items from content blocks, in order.
#[derive(Debug, Clone)]
pub struct OutputBuilder {
    request_id: String,
    items: Vec<OutputItem>,
    /// The last item is a message that the next text block extends.
    message_open: bool,
    /// Web search item index by `server_tool_use` id.
    web_searches: HashMap<String, usize>,
}

impl OutputBuilder {
    #[must_use]
    pub fn new(request_id: &str) -> Self {
        Self {
            request_id: request_id.to_owned(),
            items: Vec::new(),
            message_open: false,
            web_searches: HashMap::new(),
        }
    }

    #[must_use]
    pub fn items(&self) -> &[OutputItem] {
        &self.items
    }

    #[must_use]
    pub fn into_items(self) -> Vec<OutputItem> {
        self.items
    }

    /// Where a block of `kind` would land if pushed now.
    #[must_use]
    pub fn peek(&self, kind: BlockKind<'_>) -> Placement {
        let existing = match kind {
            BlockKind::Text if self.message_open => self.items.len().checked_sub(1),
            BlockKind::WebSearchResult { tool_use_id } => {
                self.web_searches.get(tool_use_id).copied()
            }
            _ => None,
        };
        let new_item = existing.is_none();
        let index = existing.unwrap_or(self.items.len());
        let content_index = match self.items.get(index) {
            Some(OutputItem::Message(message)) if kind == BlockKind::Text => message.content.len(),
            _ => 0,
        };
        Placement {
            output_index: to_u32(index),
            item_id: self.item_id(kind, index),
            content_index: to_u32(content_index),
            new_item,
        }
    }

    /// Add a completed block.
    pub fn push(&mut self, block: Block) -> Placement {
        let placement = self.peek(block.kind());
        let item_id = placement.item_id.clone();
        self.message_open = matches!(block, Block::Text { .. });
        match block {
            Block::Text { text, citations } => {
                let part = OutputContentPart::OutputText(OutputText {
                    annotations: citations.iter().map(|c| annotation(c, &text)).collect(),
                    text,
                    logprobs: None,
                });
                if let Some(OutputItem::Message(message)) =
                    self.items.get_mut(placement.output_index as usize)
                    && !placement.new_item
                {
                    message.content.push(part);
                } else {
                    self.items.push(OutputItem::Message(MessageOutput {
                        id: item_id,
                        status: ItemStatus::Completed,
                        role: Role::Assistant,
                        content: vec![part],
                    }));
                }
            }
            Block::ToolUse { id, name, input } => {
                self.items.push(OutputItem::FunctionCall(FunctionCallItem {
                    id: Some(item_id),
                    status: Some(ItemStatus::Completed),
                    call_id: id,
                    name,
                    arguments: input.to_string(),
                }));
            }
            Block::Thinking {
                thinking,
                signature,
            } => self.items.push(OutputItem::Reasoning(ReasoningOutput {
                id: item_id,
                summary: Some(Vec::new()),
                content: Some(vec![ReasoningContentPart::ReasoningText { text: thinking }]),
                encrypted_content: Some(signature),
            })),
            Block::RedactedThinking { data } => {
                self.items.push(OutputItem::Reasoning(ReasoningOutput {
                    id: item_id,
                    summary: Some(Vec::new()),
                    content: None,
                    encrypted_content: Some(data),
                }));
            }
            Block::WebSearchCall { id, query } => {
                self.web_searches.insert(id, self.items.len());
                self.items.push(OutputItem::WebSearchCall(
                    WebSearchCallOutput::new(item_id, ItemStatus::Completed)
                        .with_queries(query.into_iter().collect()),
                ));
            }
            Block::WebSearchResult {
                results, failed, ..
            } => self.add_web_search_results(&placement, item_id, results, failed),
            Block::Other(raw) => self
                .items
                .push(OutputItem::Other(Extension(prefixed(&raw)))),
        }
        placement
    }

    fn add_web_search_results(
        &mut self,
        placement: &Placement,
        item_id: String,
        results: Vec<crate::wire::WebSearchResultItem>,
        failed: bool,
    ) {
        let results: Vec<WebSearchResult> = results
            .into_iter()
            .map(|r| {
                let result = WebSearchResult::new(r.url);
                match r.title {
                    Some(title) => result.with_title(title),
                    None => result,
                }
            })
            .collect();
        let status = if failed {
            ItemStatus::Incomplete
        } else {
            ItemStatus::Completed
        };
        if let Some(OutputItem::WebSearchCall(call)) =
            self.items.get_mut(placement.output_index as usize)
            && !placement.new_item
        {
            call.results = results;
            call.status = status;
        } else {
            self.items.push(OutputItem::WebSearchCall(
                WebSearchCallOutput::new(item_id, status).with_results(results),
            ));
        }
    }

    fn item_id(&self, kind: BlockKind<'_>, index: usize) -> String {
        let prefix = match kind {
            BlockKind::Text => "msg",
            BlockKind::ToolUse => "fc",
            BlockKind::Reasoning => "rs",
            BlockKind::WebSearchCall | BlockKind::WebSearchResult { .. } => "ws",
            BlockKind::Other => "item",
        };
        format!("{prefix}_{}_{index}", self.request_id)
    }
}

/// Annotation for a text citation. Anthropic cites a whole text block, so a
/// URL citation spans the full part.
fn annotation(citation: &Value, text: &str) -> Annotation {
    let kind = citation.get("type").and_then(Value::as_str);
    let url = citation.get("url").and_then(Value::as_str);
    match (kind, url) {
        (Some("web_search_result_location"), Some(url)) => Annotation::UrlCitation(UrlCitation {
            url: url.to_owned(),
            title: citation
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            start_index: 0,
            end_index: to_u32(text.chars().count()),
        }),
        _ => Annotation::Other(Extension(prefixed(citation))),
    }
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "response_tests.rs"]
mod response_tests;
