// Created: 2026-11-09 by Constructor Tech
//! Open Responses request -> Anthropic Messages request.

use llm_gateway_sdk::{
    CreateResponseBody, FunctionCallItem, FunctionTool, IncludeField, InputContent,
    InputContentPart, InputFile, InputImage, InputItem, LlmGatewayError, MessageItem,
    NamedToolChoice, ReasoningConfig, ReasoningContentPart, ReasoningEffort, ReasoningItem,
    ResponseInput, Role as InputRole, ServiceTier, TextFormatKind, Tool, ToolChoice as InputChoice,
    ToolChoiceMode, TruncationStrategy, WebSearchTool,
};
use serde_json::{Map, Value, json};

use crate::schema;
use crate::settings::{
    AnthropicJsonOutputFormat, AnthropicOutputEffort, AnthropicProviderSettingsV1,
    AnthropicServiceTier, AnthropicThinking, AnthropicThinkingDisplay, AnthropicToolChoice,
};
use crate::wire::{
    CacheControl, ContentBlockParam, MediaSource, MessageParam, MessagesRequest, Metadata,
    OutputConfig, OutputFormat, Role, Thinking, ToolChoice, WEB_SEARCH_TOOL_NAME,
    WEB_SEARCH_TOOL_TYPE,
};

/// Prefix of Anthropic-specific tools, items, and blocks (`anthropic:<type>`).
pub const ANTHROPIC_PREFIX: &str = "anthropic:";

/// Web search options forwarded from `cf_gears:web_search` to Anthropic.
const WEB_SEARCH_OPTIONS: [&str; 3] = ["blocked_domains", "max_uses", "user_location"];

const PDF_MEDIA_TYPE: &str = "application/pdf";

/// The resolved model the request is built for.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub provider_model_id: &'a str,
    /// The model's maximum output tokens, the last `max_tokens` fallback.
    pub max_output_tokens: Option<u32>,
    pub settings: &'a AnthropicProviderSettingsV1,
}

/// A built Messages request.
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    pub request: MessagesRequest,
    /// Request parameters Anthropic has no equivalent for, dropped under
    /// `drop_unsupported_params`.
    pub ignored_parameters: Vec<String>,
}

/// Build the Messages request for `body`.
///
/// # Errors
///
/// - `unsupported_parameter` for a parameter Anthropic lacks, unless
///   `drop_unsupported_params` is set.
/// - `capability_not_supported` for input or options Anthropic cannot take.
/// - `validation_error` for malformed input or a missing `max_tokens`.
pub fn build(
    target: &Target<'_>,
    body: &CreateResponseBody,
    stream: bool,
) -> Result<Built, LlmGatewayError> {
    let settings = target.settings;
    let ignored_parameters = unsupported_parameters(body)?;

    let mut conversation = Conversation::default();
    if let Some(system) = settings.system.as_ref().filter(|s| !s.is_empty()) {
        conversation
            .system
            .push(ContentBlockParam::text(system.clone()));
    }
    if let Some(instructions) = body.instructions.as_ref().filter(|s| !s.is_empty()) {
        conversation
            .system
            .push(ContentBlockParam::text(instructions.clone()));
    }
    match &body.input {
        Some(ResponseInput::Text(text)) => {
            conversation.push(Role::User, ContentBlockParam::text(text));
        }
        Some(ResponseInput::Items(items)) => {
            for (index, item) in items.iter().enumerate() {
                conversation.add_item(item, index)?;
            }
        }
        None => {}
    }

    let tools = body
        .tools
        .iter()
        .flatten()
        .enumerate()
        .map(|(index, tool)| convert_tool(tool, index))
        .collect::<Result<Vec<_>, _>>()?;
    let tool_choice = if tools.is_empty() {
        None
    } else {
        tool_choice(body, settings)
    };
    let (thinking, effort) = reasoning(body.reasoning.as_ref(), settings);

    let request = MessagesRequest {
        model: target.provider_model_id.to_owned(),
        max_tokens: max_tokens(target, body)?,
        messages: conversation.messages,
        system: conversation.system,
        stop_sequences: settings.stop_sequences.clone(),
        tools,
        tool_choice,
        thinking,
        output_config: output_config(body, settings, effort)?,
        metadata: body
            .safety_identifier
            .clone()
            .map(|user_id| Metadata { user_id }),
        service_tier: service_tier(body, settings),
        inference_geo: settings.inference_geo.clone(),
        container: settings.container.clone(),
        cache_control: body
            .prompt_cache_key
            .as_ref()
            .map(|_| CacheControl::Ephemeral),
        stream: stream.then_some(true),
        extra: body
            .extra_fields
            .iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    };
    Ok(Built {
        request,
        ignored_parameters,
    })
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

fn unsupported_parameters(body: &CreateResponseBody) -> Result<Vec<String>, LlmGatewayError> {
    let checks = [
        ("temperature", body.temperature.is_some()),
        ("top_p", body.top_p.is_some()),
        ("seed", body.seed.is_some()),
        ("presence_penalty", body.presence_penalty.is_some()),
        ("frequency_penalty", body.frequency_penalty.is_some()),
        ("top_logprobs", body.top_logprobs.is_some()),
        ("max_tool_calls", body.max_tool_calls.is_some()),
        (
            "truncation",
            body.truncation == Some(TruncationStrategy::Auto),
        ),
        (
            "include",
            body.include
                .iter()
                .flatten()
                .any(|f| *f == IncludeField::MessageOutputTextLogprobs),
        ),
    ];
    let params: Vec<String> = checks
        .into_iter()
        .filter(|(_, present)| *present)
        .map(|(name, _)| name.to_owned())
        .collect();
    match params.first() {
        Some(first) if body.drop_unsupported_params != Some(true) => {
            Err(LlmGatewayError::unsupported_parameter(first.clone()))
        }
        _ => Ok(params),
    }
}

fn max_tokens(target: &Target<'_>, body: &CreateResponseBody) -> Result<u32, LlmGatewayError> {
    body.max_output_tokens
        .or(target.settings.max_tokens)
        .or(target.max_output_tokens)
        .ok_or_else(|| {
            LlmGatewayError::validation_param(
                "max_output_tokens is required: the model has no default output limit",
                "max_output_tokens",
            )
        })
}

fn service_tier(
    body: &CreateResponseBody,
    settings: &AnthropicProviderSettingsV1,
) -> Option<AnthropicServiceTier> {
    match body.service_tier {
        Some(ServiceTier::Auto) => Some(AnthropicServiceTier::Auto),
        Some(ServiceTier::Default) => Some(AnthropicServiceTier::StandardOnly),
        None => settings.service_tier,
    }
}

fn tool_choice(
    body: &CreateResponseBody,
    settings: &AnthropicProviderSettingsV1,
) -> Option<ToolChoice> {
    let disable = (body.parallel_tool_calls == Some(false)).then_some(true);
    let choice = match &body.tool_choice {
        Some(InputChoice::Mode(ToolChoiceMode::Auto)) => ToolChoice::Auto {
            disable_parallel_tool_use: disable,
        },
        Some(InputChoice::Mode(ToolChoiceMode::Required)) => ToolChoice::Any {
            disable_parallel_tool_use: disable,
        },
        Some(InputChoice::Mode(ToolChoiceMode::None)) => ToolChoice::None,
        Some(InputChoice::Named(NamedToolChoice::Function { name })) => ToolChoice::Tool {
            name: name.clone(),
            disable_parallel_tool_use: disable,
        },
        None => match &settings.tool_choice {
            Some(default) => settings_tool_choice(default, disable),
            None => ToolChoice::Auto {
                disable_parallel_tool_use: Some(disable?),
            },
        },
    };
    Some(choice)
}

fn settings_tool_choice(choice: &AnthropicToolChoice, disable: Option<bool>) -> ToolChoice {
    match choice {
        AnthropicToolChoice::Auto {
            disable_parallel_tool_use,
        } => ToolChoice::Auto {
            disable_parallel_tool_use: disable.or(*disable_parallel_tool_use),
        },
        AnthropicToolChoice::Any {
            disable_parallel_tool_use,
        } => ToolChoice::Any {
            disable_parallel_tool_use: disable.or(*disable_parallel_tool_use),
        },
        AnthropicToolChoice::Tool {
            name,
            disable_parallel_tool_use,
        } => ToolChoice::Tool {
            name: name.clone(),
            disable_parallel_tool_use: disable.or(*disable_parallel_tool_use),
        },
        AnthropicToolChoice::None => ToolChoice::None,
    }
}

/// `thinking` and `output_config.effort` from the reasoning config, falling
/// back to the model settings.
fn reasoning(
    config: Option<&ReasoningConfig>,
    settings: &AnthropicProviderSettingsV1,
) -> (Option<Thinking>, Option<AnthropicOutputEffort>) {
    let default_thinking = settings.thinking.as_ref().map(settings_thinking);
    let default_effort = settings.output_config.as_ref().and_then(|c| c.effort);
    let Some(config) = config else {
        return (default_thinking, default_effort);
    };
    let display = config.summary.map(|_| AnthropicThinkingDisplay::Summarized);
    let effort = config.effort.and_then(effort_level);
    match (config.effort, config.budget_tokens) {
        (Some(ReasoningEffort::None), _) => (Some(Thinking::Disabled), None),
        (_, Some(budget_tokens)) => (
            Some(Thinking::Enabled {
                budget_tokens,
                display,
            }),
            effort.or(default_effort),
        ),
        (Some(_), None) => (Some(Thinking::Adaptive { display }), effort),
        (None, None) => {
            let thinking = match (default_thinking, display) {
                (Some(Thinking::Enabled { budget_tokens, .. }), Some(_)) => {
                    Some(Thinking::Enabled {
                        budget_tokens,
                        display,
                    })
                }
                (Some(Thinking::Adaptive { .. }) | None, Some(_)) => {
                    Some(Thinking::Adaptive { display })
                }
                (thinking, _) => thinking,
            };
            (thinking, default_effort)
        }
    }
}

fn effort_level(effort: ReasoningEffort) -> Option<AnthropicOutputEffort> {
    match effort {
        ReasoningEffort::None => None,
        ReasoningEffort::Low => Some(AnthropicOutputEffort::Low),
        ReasoningEffort::Medium => Some(AnthropicOutputEffort::Medium),
        ReasoningEffort::High => Some(AnthropicOutputEffort::High),
        ReasoningEffort::XHigh => Some(AnthropicOutputEffort::XHigh),
    }
}

fn settings_thinking(thinking: &AnthropicThinking) -> Thinking {
    match thinking {
        AnthropicThinking::Enabled {
            budget_tokens,
            display,
        } => Thinking::Enabled {
            budget_tokens: *budget_tokens,
            display: *display,
        },
        AnthropicThinking::Disabled => Thinking::Disabled,
        AnthropicThinking::Adaptive { display } => Thinking::Adaptive { display: *display },
    }
}

fn output_config(
    body: &CreateResponseBody,
    settings: &AnthropicProviderSettingsV1,
    effort: Option<AnthropicOutputEffort>,
) -> Result<Option<OutputConfig>, LlmGatewayError> {
    let format = match body.text.as_ref().map(|t| &t.format) {
        Some(TextFormatKind::JsonSchema { schema, .. }) => {
            let schema = schema
                .clone()
                .unwrap_or_else(|| json!({ "type": "object" }));
            Some(OutputFormat::JsonSchema {
                schema: schema::adapt(&schema)?,
            })
        }
        Some(TextFormatKind::JsonObject) => {
            return Err(LlmGatewayError::capability_not_supported(
                "text.format json_object",
            ));
        }
        Some(TextFormatKind::Text) => None,
        None => settings
            .output_config
            .as_ref()
            .and_then(|c| c.format.as_ref())
            .map(
                |AnthropicJsonOutputFormat::JsonSchema { schema }| OutputFormat::JsonSchema {
                    schema: schema.clone(),
                },
            ),
    };
    Ok((effort.is_some() || format.is_some()).then_some(OutputConfig { effort, format }))
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn convert_tool(tool: &Tool, index: usize) -> Result<Value, LlmGatewayError> {
    match tool {
        Tool::Function(function) => Ok(function_tool(function)),
        Tool::WebSearch(web_search) => Ok(web_search_tool(web_search)),
        Tool::Other(ext) => passthrough(&ext.0).ok_or_else(|| {
            LlmGatewayError::validation_param(
                format!(
                    "tool type {:?} is not supported by the Anthropic provider",
                    ext.kind().unwrap_or_default()
                ),
                format!("tools[{index}]"),
            )
        }),
        Tool::Reference(_) | Tool::InlineGts(_) => Err(LlmGatewayError::internal(
            "GTS tool definitions must be resolved before dispatch",
        )),
        Tool::ImageGeneration(_) => Err(LlmGatewayError::capability_not_supported(
            "image_generation",
        )),
    }
}

fn function_tool(function: &FunctionTool) -> Value {
    let mut tool = Map::new();
    tool.insert("name".to_owned(), json!(function.name));
    if let Some(description) = &function.description {
        tool.insert("description".to_owned(), json!(description));
    }
    tool.insert(
        "input_schema".to_owned(),
        function
            .parameters
            .clone()
            .unwrap_or_else(|| json!({ "type": "object" })),
    );
    if let Some(strict) = function.strict {
        tool.insert("strict".to_owned(), json!(strict));
    }
    Value::Object(tool)
}

fn web_search_tool(web_search: &WebSearchTool) -> Value {
    let mut tool = Map::new();
    tool.insert("type".to_owned(), json!(WEB_SEARCH_TOOL_TYPE));
    tool.insert("name".to_owned(), json!(WEB_SEARCH_TOOL_NAME));
    if let Some(domains) = &web_search.allowed_domains {
        tool.insert("allowed_domains".to_owned(), json!(domains));
    }
    for key in WEB_SEARCH_OPTIONS {
        if let Some(value) = web_search.options.get(key) {
            tool.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(tool)
}

/// An `anthropic:<type>` value with the prefix stripped from `type`.
pub fn passthrough(value: &Value) -> Option<Value> {
    let kind = value
        .get("type")?
        .as_str()?
        .strip_prefix(ANTHROPIC_PREFIX)?;
    let mut object = value.as_object()?.clone();
    object.insert("type".to_owned(), json!(kind));
    Some(Value::Object(object))
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Conversation {
    system: Vec<ContentBlockParam>,
    messages: Vec<MessageParam>,
}

impl Conversation {
    /// Append a block, merging it into the last message when the role matches.
    fn push(&mut self, role: Role, block: ContentBlockParam) {
        match self.messages.last_mut() {
            Some(last) if last.role == role => last.content.push(block),
            _ => self.messages.push(MessageParam {
                role,
                content: vec![block],
            }),
        }
    }

    fn add_item(&mut self, item: &InputItem, index: usize) -> Result<(), LlmGatewayError> {
        match item {
            InputItem::Message(message) => self.add_message(message, index),
            InputItem::FunctionCall(call) => {
                self.push(Role::Assistant, tool_use(call, index)?);
                Ok(())
            }
            InputItem::FunctionCallOutput(output) => {
                let content = content_blocks(&output.output, &format!("input[{index}].output"))?;
                self.push(
                    Role::User,
                    ContentBlockParam::ToolResult {
                        tool_use_id: output.call_id.clone(),
                        content,
                    },
                );
                Ok(())
            }
            InputItem::Reasoning(reasoning) => {
                if let Some(block) = reasoning_block(reasoning) {
                    self.push(Role::Assistant, block);
                }
                Ok(())
            }
            InputItem::ItemReference(_) => {
                Err(LlmGatewayError::capability_not_supported("item_reference"))
            }
            InputItem::Other(ext) => {
                if ext.kind() == Some("cf_gears:web_search_call") {
                    return Ok(());
                }
                let block = passthrough(&ext.0).ok_or_else(|| {
                    LlmGatewayError::validation_param(
                        format!(
                            "input item type {:?} is not supported by the Anthropic provider",
                            ext.kind().unwrap_or_default()
                        ),
                        format!("input[{index}]"),
                    )
                })?;
                self.push(Role::Assistant, ContentBlockParam::Raw(block));
                Ok(())
            }
        }
    }

    fn add_message(&mut self, message: &MessageItem, index: usize) -> Result<(), LlmGatewayError> {
        let param = format!("input[{index}].content");
        let blocks = content_blocks(&message.content, &param)?;
        match message.role {
            InputRole::User => blocks.into_iter().for_each(|b| self.push(Role::User, b)),
            InputRole::Assistant => blocks
                .into_iter()
                .for_each(|b| self.push(Role::Assistant, b)),
            InputRole::System | InputRole::Developer => {
                if blocks
                    .iter()
                    .any(|b| !matches!(b, ContentBlockParam::Text { .. }))
                {
                    return Err(LlmGatewayError::validation_param(
                        "system and developer messages accept only text",
                        param,
                    ));
                }
                self.system.extend(blocks);
            }
        }
        Ok(())
    }
}

fn tool_use(call: &FunctionCallItem, index: usize) -> Result<ContentBlockParam, LlmGatewayError> {
    let input = if call.arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&call.arguments).map_err(|_| {
            LlmGatewayError::validation_param(
                "function call arguments must be valid JSON",
                format!("input[{index}].arguments"),
            )
        })?
    };
    Ok(ContentBlockParam::ToolUse {
        id: call.call_id.clone(),
        name: call.name.clone(),
        input,
    })
}

/// A thinking block from a reasoning item Anthropic issued; `None` for items
/// without a signature.
fn reasoning_block(reasoning: &ReasoningItem) -> Option<ContentBlockParam> {
    let signature = reasoning.encrypted_content.clone()?;
    let block = match &reasoning.content {
        Some(parts) => ContentBlockParam::Thinking {
            thinking: parts
                .iter()
                .map(|ReasoningContentPart::ReasoningText { text }| text.as_str())
                .collect(),
            signature,
        },
        None => ContentBlockParam::RedactedThinking { data: signature },
    };
    Some(block)
}

fn content_blocks(
    content: &InputContent,
    param: &str,
) -> Result<Vec<ContentBlockParam>, LlmGatewayError> {
    match content {
        InputContent::Text(text) => Ok(vec![ContentBlockParam::text(text)]),
        InputContent::Parts(parts) => parts
            .iter()
            .enumerate()
            .map(|(index, part)| content_part(part, &format!("{param}[{index}]")))
            .collect(),
    }
}

fn content_part(
    part: &InputContentPart,
    param: &str,
) -> Result<ContentBlockParam, LlmGatewayError> {
    match part {
        InputContentPart::InputText(text) => Ok(ContentBlockParam::text(&text.text)),
        InputContentPart::InputImage(image) => image_block(image, param),
        InputContentPart::InputFile(file) => document_block(file, param),
        InputContentPart::InputAudio(_) => {
            Err(LlmGatewayError::capability_not_supported("input_audio"))
        }
        InputContentPart::InputVideo(_) => {
            Err(LlmGatewayError::capability_not_supported("input_video"))
        }
        // Output parts replayed in assistant messages.
        InputContentPart::Other(ext) => {
            let text = match ext.kind() {
                Some("output_text") => ext.0.get("text"),
                Some("refusal") => ext.0.get("refusal"),
                _ => None,
            };
            text.and_then(Value::as_str)
                .map(ContentBlockParam::text)
                .ok_or_else(|| {
                    LlmGatewayError::validation_param(
                        format!(
                            "content part type {:?} is not supported by the Anthropic provider",
                            ext.kind().unwrap_or_default()
                        ),
                        param,
                    )
                })
        }
    }
}

fn image_block(image: &InputImage, param: &str) -> Result<ContentBlockParam, LlmGatewayError> {
    if let Some(url) = &image.image_url {
        return Ok(ContentBlockParam::Image {
            source: media_source(url, None, param)?,
        });
    }
    if image.file_id.is_some() {
        return Err(LlmGatewayError::capability_not_supported(
            "input_image file_id",
        ));
    }
    Err(LlmGatewayError::validation_param(
        "input_image requires image_url",
        param,
    ))
}

fn document_block(file: &InputFile, param: &str) -> Result<ContentBlockParam, LlmGatewayError> {
    let source = if let Some(data) = &file.file_data {
        media_source(data, Some(PDF_MEDIA_TYPE), param)?
    } else if let Some(url) = &file.file_url {
        media_source(url, None, param)?
    } else if file.file_id.is_some() {
        return Err(LlmGatewayError::capability_not_supported(
            "input_file file_id",
        ));
    } else {
        return Err(LlmGatewayError::validation_param(
            "input_file requires file_data or file_url",
            param,
        ));
    };
    Ok(ContentBlockParam::Document {
        source,
        title: file.filename.clone(),
    })
}

/// Source for a data URI, an http(s) URL, or — when `raw_base64_type` is set
/// — bare base64 data of that media type.
fn media_source(
    value: &str,
    raw_base64_type: Option<&str>,
    param: &str,
) -> Result<MediaSource, LlmGatewayError> {
    if let Some(rest) = value.strip_prefix("data:") {
        let (media_type, data) = rest
            .split_once(',')
            .and_then(|(header, data)| Some((header.strip_suffix(";base64")?, data)))
            .ok_or_else(|| {
                LlmGatewayError::validation_param("only base64 data URIs are supported", param)
            })?;
        return Ok(MediaSource::Base64 {
            media_type: media_type.to_owned(),
            data: data.to_owned(),
        });
    }
    if value.starts_with("https://") || value.starts_with("http://") {
        return Ok(MediaSource::Url {
            url: value.to_owned(),
        });
    }
    match raw_base64_type {
        Some(media_type) => Ok(MediaSource::Base64 {
            media_type: media_type.to_owned(),
            data: value.to_owned(),
        }),
        None => Err(LlmGatewayError::validation_param(
            "expected a data URI or an http(s) URL",
            param,
        )),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "request_tests.rs"]
mod request_tests;
