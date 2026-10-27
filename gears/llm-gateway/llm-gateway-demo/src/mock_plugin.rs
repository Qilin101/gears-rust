//! Mock [`LlmGatewayProviderPluginClientV1`] standing in for an `OpenAI` provider
//! integration. It does not call any network: it narrows `provider_settings`
//! from [`ProviderCallCtx`] to its own settings type and echoes a canned
//! response so the wiring is visible.
//!
//! The settings type is the plugin's provider type: a GTS type derived from
//! Model Registry's provider base type (`ProviderTypeV1`). Its schema is what
//! the plugin publishes in Types Registry and what Model Registry validates
//! every model's `provider_settings` against.

use async_trait::async_trait;
use llm_gateway_sdk::models::content::{OutputContentPart, OutputText};
use llm_gateway_sdk::models::core::ResponseResource;
use llm_gateway_sdk::models::core::{
    InputTokensDetails, OutputTokensDetails, ReasoningConfig, ResponseInput, ResponseStatus,
    TextFormat, ToolChoice, ToolChoiceMode, TruncationStrategy, Usage,
};
use llm_gateway_sdk::models::items::{ItemStatus, MessageOutput, OutputItem};
use llm_gateway_sdk::models::plugin::{
    MediaInputMode, ProviderCallCtx, ProviderPluginCapabilities,
};
use llm_gateway_sdk::models::role::Role;
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayError,
    LlmGatewayProviderPluginClientV1, ResponseEventStream,
};
use model_registry_sdk::ProviderTypeV1;
use serde::{Deserialize, Serialize};
use toolkit_gts::gts_type_schema;

/// Cost table of the demo `OpenAI` provider type, in micro-credits per 1k
/// tokens. Provider-specific by design: Model Registry stores it opaquely.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OpenAiCost {
    pub input_per_1k_micro: Option<u64>,
    pub output_per_1k_micro: Option<u64>,
}

/// The demo `OpenAI` provider type: the shape of `provider_settings` for every
/// model whose provider has this `gts_type`.
#[derive(Debug)]
#[gts_type_schema(
    dir_path = "schemas",
    base = ProviderTypeV1,
    type_id = gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~"),
    description = "Demo OpenAI provider settings: the OAGW upstream alias and the cost table",
    properties = "oagw_alias,cost"
)]
pub struct OpenAiProviderSettingsV1 {
    /// The OAGW upstream alias the plugin sends provider calls through.
    pub oagw_alias: String,
    pub cost: OpenAiCost,
}

/// A mock `OpenAI` provider plugin.
pub struct MockOpenAiPlugin;

#[async_trait]
impl LlmGatewayProviderPluginClientV1 for MockOpenAiPlugin {
    fn capabilities(&self) -> ProviderPluginCapabilities {
        ProviderPluginCapabilities {
            streaming_transport: true,
            media_input: MediaInputMode::UrlForward,
        }
    }

    async fn create_response(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        call: &ProviderCallCtx,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError> {
        // Narrow the opaque settings document to this plugin's typed view. This
        // checks `gts_type` against the plugin's provider type id and gives
        // typed field access; the real adapter reads its routing from here.
        let settings = call
            .typed_settings::<OpenAiProviderSettingsV1>()
            .map_err(|e| {
                LlmGatewayError::internal(format!("failed to narrow OpenAI settings: {e}"))
            })?;
        let info = &call.model_info;

        let prompt = match body.input {
            Some(ResponseInput::Text(t)) => t,
            Some(ResponseInput::Items(_)) => "<structured input>".to_owned(),
            None => String::new(),
        };

        let reply = format!(
            "echo from provider_model_id={} via oagw_alias={} base_url={} secret_ref={}: {prompt}",
            info.provider_model_id,
            settings.oagw_alias,
            info.base_url.as_deref().unwrap_or("<plugin default>"),
            info.secret_ref.as_deref().unwrap_or("<none>"),
        );

        Ok(mock_response(&reply, &body.model))
    }

    async fn create_response_stream(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _call: &ProviderCallCtx,
        _body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        Err(LlmGatewayError::capability_not_supported(
            "streaming not implemented in demo",
        ))
    }

    async fn create_embedding(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _call: &ProviderCallCtx,
        _req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        Err(LlmGatewayError::capability_not_supported(
            "embeddings not implemented in demo",
        ))
    }
}

/// Build a completed `ResponseResource` carrying a single assistant message.
fn mock_response(text: &str, model: &str) -> ResponseResource {
    let message = MessageOutput {
        id: "msg_demo_0001".to_owned(),
        status: ItemStatus::Completed,
        role: Role::Assistant,
        content: vec![OutputContentPart::OutputText(OutputText {
            text: text.to_owned(),
            annotations: Vec::new(),
            logprobs: None,
        })],
    };

    ResponseResource {
        id: "resp_demo_0001".to_owned(),
        object: "response".to_owned(),
        created_at: 0,
        completed_at: Some(0),
        status: ResponseStatus::Completed,
        incomplete_details: None,
        model: model.to_owned(),
        instructions: None,
        output: vec![OutputItem::Message(message)],
        error: None,
        tools: Vec::new(),
        tool_choice: ToolChoice::Mode(ToolChoiceMode::Auto),
        truncation: TruncationStrategy::Disabled,
        parallel_tool_calls: true,
        text: TextFormat::default(),
        top_p: 1.0,
        presence_penalty: 0.0,
        frequency_penalty: 0.0,
        top_logprobs: 0,
        temperature: 1.0,
        reasoning: ReasoningConfig::default(),
        usage: Some(Usage {
            input_tokens: 8,
            output_tokens: 12,
            total_tokens: 20,
            input_tokens_details: InputTokensDetails { cached_tokens: 0 },
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: 0,
            },
        }),
        max_output_tokens: None,
        max_tool_calls: None,
        service_tier: "auto".to_owned(),
        metadata: None,
        safety_identifier: None,
        prompt_cache_key: None,
    }
}
