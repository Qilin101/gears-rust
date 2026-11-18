// Created: 2026-11-09 by Constructor Tech
//! Open Responses request -> Anthropic Messages request.

use llm_gateway_sdk::{
    CreateResponseBody, FunctionCallItem, FunctionTool, ImageDetail, IncludeField, InputContent,
    InputContentPart, InputFile, InputImage, InputItem, LlmGatewayError, MessageItem,
    NamedToolChoice, ReasoningConfig, ReasoningContentPart, ReasoningEffort, ReasoningItem,
    ResponseInput, Role as InputRole, ServiceTier, TextFormatKind, Tool, ToolChoice as InputChoice,
    ToolChoiceMode, TruncationStrategy, WebSearchTool,
};
use serde_json::{Map, Value, json};

use crate::guard;
use crate::response::Echo;
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

/// Type prefixes of the Anthropic built-in tools that may be forwarded: tools
/// the consumer runs, which keep no state at Anthropic.
const CLIENT_TOOL_PREFIXES: [&str; 4] = ["bash_", "text_editor_", "memory_", "computer_"];

/// Top-level request fields `extra_fields` cannot set: the fields the plugin
/// builds, and `container`, which refers to state kept at Anthropic.
const RESERVED_FIELDS: [&str; 15] = [
    "model",
    "max_tokens",
    "messages",
    "system",
    "stop_sequences",
    "tools",
    "tool_choice",
    "thinking",
    "output_config",
    "metadata",
    "service_tier",
    "inference_geo",
    "container",
    "cache_control",
    "stream",
];

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
    /// The values sent where they differ from the request.
    pub echo: Echo,
}

/// Build the Messages request for `body`.
///
/// # Errors
///
/// - `unsupported_parameter` for a parameter Anthropic lacks, unless
///   `drop_unsupported_params` is set.
/// - `capability_not_supported` for input, tools, or options Anthropic cannot
///   take.
/// - `unsupported_schema` for a `text.format` schema Anthropic cannot express.
/// - `validation_error` for malformed input, a missing `max_tokens`, or an
///   `extra_fields` entry that sets a field the plugin builds.
pub fn build(
    target: &Target<'_>,
    body: &CreateResponseBody,
    stream: bool,
) -> Result<Built, LlmGatewayError> {
    let settings = target.settings;
    let extra = extra_fields(body)?;
    let mut unsupported = unsupported_parameters(body);

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

    unsupported.append(&mut conversation.unsupported);

    let tools = body
        .tools
        .iter()
        .flatten()
        .enumerate()
        .map(|(index, tool)| convert_tool(tool, index, &mut unsupported))
        .collect::<Result<Vec<_>, _>>()?;
    let (thinking, effort) = reasoning(body.reasoning.as_ref(), settings)?;
    let mut tool_choice = if tools.is_empty() {
        check_choice_without_tools(body)?;
        None
    } else {
        check_named_choice(body)?;
        tool_choice(body, settings)
    };
    // Anthropic rejects forced tool use while thinking is on.
    if matches!(
        thinking,
        Some(Thinking::Enabled { .. } | Thinking::Adaptive { .. })
    ) && let Some(
        ToolChoice::Any {
            disable_parallel_tool_use,
        }
        | ToolChoice::Tool {
            disable_parallel_tool_use,
            ..
        },
    ) = tool_choice
    {
        unsupported.push("tool_choice".to_owned());
        tool_choice = Some(ToolChoice::Auto {
            disable_parallel_tool_use,
        });
    }
    let ignored_parameters = apply_drop_policy(body, unsupported)?;

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
        cache_control: body
            .prompt_cache_key
            .as_ref()
            .map(|_| CacheControl::Ephemeral),
        stream: stream.then_some(true),
        extra,
    };
    let echo = echo(&request);
    Ok(Built {
        request,
        ignored_parameters,
        echo,
    })
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

/// `extra_fields` entries; none may set a reserved field.
fn extra_fields(body: &CreateResponseBody) -> Result<Map<String, Value>, LlmGatewayError> {
    let fields = body.extra_fields.iter().flatten();
    if let Some(key) = RESERVED_FIELDS
        .iter()
        .find(|key| fields.clone().any(|(k, _)| k == *key))
    {
        return Err(LlmGatewayError::validation_param(
            format!("extra_fields cannot set {key}"),
            format!("extra_fields.{key}"),
        ));
    }
    Ok(fields.map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Reject the unsupported parameters, or drop them under
/// `drop_unsupported_params` and return them.
fn apply_drop_policy(
    body: &CreateResponseBody,
    unsupported: Vec<String>,
) -> Result<Vec<String>, LlmGatewayError> {
    match unsupported.first() {
        Some(first) if body.drop_unsupported_params != Some(true) => {
            Err(LlmGatewayError::unsupported_parameter(first.clone()))
        }
        _ => Ok(unsupported),
    }
}

/// Request values the response reports as sent.
fn echo(request: &MessagesRequest) -> Echo {
    let (tool_choice, disable_parallel) = match &request.tool_choice {
        Some(ToolChoice::Auto {
            disable_parallel_tool_use,
        }) => (
            Some(InputChoice::Mode(ToolChoiceMode::Auto)),
            *disable_parallel_tool_use,
        ),
        Some(ToolChoice::Any {
            disable_parallel_tool_use,
        }) => (
            Some(InputChoice::Mode(ToolChoiceMode::Required)),
            *disable_parallel_tool_use,
        ),
        Some(ToolChoice::Tool {
            name,
            disable_parallel_tool_use,
        }) => (
            Some(InputChoice::Named(NamedToolChoice::Function {
                name: name.clone(),
            })),
            *disable_parallel_tool_use,
        ),
        Some(ToolChoice::None) => (Some(InputChoice::Mode(ToolChoiceMode::None)), None),
        None => (None, None),
    };
    Echo {
        max_output_tokens: Some(request.max_tokens),
        tool_choice,
        parallel_tool_calls: disable_parallel.map(|disable| !disable),
        service_tier: request.service_tier.map(|tier| match tier {
            AnthropicServiceTier::Auto => "auto".to_owned(),
            AnthropicServiceTier::StandardOnly => "default".to_owned(),
        }),
    }
}

fn unsupported_parameters(body: &CreateResponseBody) -> Vec<String> {
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
        (
            "text.verbosity",
            body.text.as_ref().is_some_and(|t| t.verbosity.is_some()),
        ),
    ];
    checks
        .into_iter()
        .filter(|(_, present)| *present)
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// A forced tool choice needs tools.
fn check_choice_without_tools(body: &CreateResponseBody) -> Result<(), LlmGatewayError> {
    match &body.tool_choice {
        Some(InputChoice::Mode(ToolChoiceMode::Required) | InputChoice::Named(_)) => {
            Err(LlmGatewayError::validation_param(
                "tool_choice forces a tool call but the request has no tools",
                "tool_choice",
            ))
        }
        _ => Ok(()),
    }
}

/// A named tool choice must name a function tool of the request.
fn check_named_choice(body: &CreateResponseBody) -> Result<(), LlmGatewayError> {
    let Some(InputChoice::Named(NamedToolChoice::Function { name })) = &body.tool_choice else {
        return Ok(());
    };
    let defined = body
        .tools
        .iter()
        .flatten()
        .any(|tool| matches!(tool, Tool::Function(f) if f.name == *name));
    if defined {
        Ok(())
    } else {
        Err(LlmGatewayError::validation_param(
            format!("tool_choice names function {name:?}, which is not among the tools"),
            "tool_choice",
        ))
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
) -> Result<(Option<Thinking>, Option<AnthropicOutputEffort>), LlmGatewayError> {
    let default_thinking = settings.thinking.as_ref().map(settings_thinking);
    let default_effort = settings.output_config.as_ref().and_then(|c| c.effort);
    let Some(config) = config else {
        return Ok((default_thinking, default_effort));
    };
    let display = config.summary.map(|_| AnthropicThinkingDisplay::Summarized);
    let effort = config.effort.and_then(effort_level);
    Ok(match (config.effort, config.budget_tokens) {
        (Some(ReasoningEffort::None), Some(_)) => {
            return Err(LlmGatewayError::validation_param(
                "reasoning.budget_tokens cannot be combined with effort none",
                "reasoning.budget_tokens",
            ));
        }
        (Some(ReasoningEffort::None), None) => (Some(Thinking::Disabled), None),
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
    })
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
        None => match settings
            .output_config
            .as_ref()
            .and_then(|c| c.format.as_ref())
        {
            Some(AnthropicJsonOutputFormat::JsonSchema { schema }) => {
                Some(OutputFormat::JsonSchema {
                    schema: schema::adapt(schema)?,
                })
            }
            None => None,
        },
    };
    Ok((effort.is_some() || format.is_some()).then_some(OutputConfig { effort, format }))
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn convert_tool(
    tool: &Tool,
    index: usize,
    unsupported: &mut Vec<String>,
) -> Result<Value, LlmGatewayError> {
    match tool {
        Tool::Function(function) => Ok(function_tool(function)),
        Tool::WebSearch(web_search) => Ok(web_search_tool(web_search, index, unsupported)),
        Tool::Other(ext) => {
            let tool = passthrough(&ext.0).ok_or_else(|| {
                LlmGatewayError::validation_param(
                    format!(
                        "tool type {:?} is not supported by the Anthropic provider",
                        ext.kind().unwrap_or_default()
                    ),
                    format!("tools[{index}]"),
                )
            })?;
            let kind = tool.get("type").and_then(Value::as_str).unwrap_or_default();
            if CLIENT_TOOL_PREFIXES
                .iter()
                .any(|prefix| kind.starts_with(prefix))
            {
                guard::check_value(&tool, &format!("tools[{index}]"))?;
                Ok(tool)
            } else {
                Err(LlmGatewayError::capability_not_supported(format!(
                    "{ANTHROPIC_PREFIX}{kind}"
                )))
            }
        }
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

fn web_search_tool(
    web_search: &WebSearchTool,
    index: usize,
    unsupported: &mut Vec<String>,
) -> Value {
    let mut tool = Map::new();
    tool.insert("type".to_owned(), json!(WEB_SEARCH_TOOL_TYPE));
    tool.insert("name".to_owned(), json!(WEB_SEARCH_TOOL_NAME));
    if let Some(domains) = &web_search.allowed_domains {
        tool.insert("allowed_domains".to_owned(), json!(domains));
    }
    for (key, value) in &web_search.options {
        if WEB_SEARCH_OPTIONS.contains(&key.as_str()) {
            tool.insert(key.clone(), value.clone());
        } else {
            unsupported.push(format!("tools[{index}].{key}"));
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
    /// Whether the last message ends with the content of a `message` item.
    ends_with_message_item: bool,
    /// Input options Anthropic has no equivalent for.
    unsupported: Vec<String>,
}

impl Conversation {
    /// Append a block, merging it into the last message when the role matches.
    fn push(&mut self, role: Role, block: ContentBlockParam) {
        self.ends_with_message_item = false;
        match self.messages.last_mut() {
            Some(last) if last.role == role => last.content.push(block),
            _ => self.messages.push(MessageParam {
                role,
                content: vec![block],
            }),
        }
    }

    /// Append the content of a `message` item. It joins the last message of
    /// the same role unless that one already ends with a `message` item:
    /// consecutive message items stay separate messages.
    fn push_message(&mut self, role: Role, blocks: Vec<ContentBlockParam>) {
        if blocks.is_empty() {
            return;
        }
        match self.messages.last_mut() {
            Some(last) if last.role == role && !self.ends_with_message_item => {
                last.content.extend(blocks);
            }
            _ => self.messages.push(MessageParam {
                role,
                content: blocks,
            }),
        }
        self.ends_with_message_item = true;
    }

    fn add_item(&mut self, item: &InputItem, index: usize) -> Result<(), LlmGatewayError> {
        match item {
            InputItem::Message(message) => self.add_message(message, index),
            InputItem::FunctionCall(call) => {
                self.push(Role::Assistant, tool_use(call, index)?);
                Ok(())
            }
            InputItem::FunctionCallOutput(output) => {
                let content = content_blocks(
                    &output.output,
                    &format!("input[{index}].output"),
                    &mut self.unsupported,
                )?;
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
                guard::check_value(&block, &format!("input[{index}]"))?;
                self.push(Role::Assistant, ContentBlockParam::Raw(block));
                Ok(())
            }
        }
    }

    fn add_message(&mut self, message: &MessageItem, index: usize) -> Result<(), LlmGatewayError> {
        let param = format!("input[{index}].content");
        let blocks = content_blocks(&message.content, &param, &mut self.unsupported)?;
        match message.role {
            InputRole::User => self.push_message(Role::User, blocks),
            InputRole::Assistant => self.push_message(Role::Assistant, blocks),
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
        serde_json::from_str::<Value>(&call.arguments)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| {
                LlmGatewayError::validation_param(
                    "function call arguments must be a JSON object",
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
    unsupported: &mut Vec<String>,
) -> Result<Vec<ContentBlockParam>, LlmGatewayError> {
    match content {
        InputContent::Text(text) => Ok(vec![ContentBlockParam::text(text)]),
        InputContent::Parts(parts) => parts
            .iter()
            .enumerate()
            .map(|(index, part)| content_part(part, &format!("{param}[{index}]"), unsupported))
            .collect(),
    }
}

fn content_part(
    part: &InputContentPart,
    param: &str,
    unsupported: &mut Vec<String>,
) -> Result<ContentBlockParam, LlmGatewayError> {
    match part {
        InputContentPart::InputText(text) => Ok(ContentBlockParam::text(&text.text)),
        InputContentPart::InputImage(image) => {
            if matches!(image.detail, Some(ImageDetail::Low | ImageDetail::High)) {
                unsupported.push(format!("{param}.detail"));
            }
            image_block(image, param)
        }
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
            source: media_source(url, param)?,
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
        data_uri(data, param)?
    } else if let Some(url) = &file.file_url {
        media_source(url, param)?
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

/// Source for a base64 data URI or an http(s) URL.
fn media_source(value: &str, param: &str) -> Result<MediaSource, LlmGatewayError> {
    if value.starts_with("data:") {
        return data_uri(value, param);
    }
    if value.starts_with("https://") || value.starts_with("http://") {
        return Ok(MediaSource::Url {
            url: value.to_owned(),
        });
    }
    Err(LlmGatewayError::validation_param(
        "expected a data URI or an http(s) URL",
        param,
    ))
}

/// Source for `data:<media type>;base64,<data>`.
fn data_uri(value: &str, param: &str) -> Result<MediaSource, LlmGatewayError> {
    value
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .and_then(|(header, data)| Some((header.strip_suffix(";base64")?, data)))
        .filter(|(media_type, data)| media_type.contains('/') && is_base64(data))
        .map(|(media_type, data)| MediaSource::Base64 {
            media_type: media_type.to_owned(),
            data: data.to_owned(),
        })
        .ok_or_else(|| {
            LlmGatewayError::validation_param(
                "expected a base64 data URI: data:<media type>;base64,<data>",
                param,
            )
        })
}

fn is_base64(data: &str) -> bool {
    !data.is_empty()
        && data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "request_tests.rs"]
mod request_tests;
