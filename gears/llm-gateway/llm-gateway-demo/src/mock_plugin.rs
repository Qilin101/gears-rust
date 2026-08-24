//! Mock [`LlmGatewayProviderPluginClientV1`] standing in for an `OpenAI` provider
//! integration. It does not call any network -- it narrows the settings from
//! [`ProviderCallCtx`] and echoes a canned response so the wiring is visible.

use async_trait::async_trait;
use llm_gateway_sdk::models::content::{OutputContentPart, OutputText};
use llm_gateway_sdk::models::core::ResponseResource;
use llm_gateway_sdk::models::core::{
    InputTokensDetails, OutputTokensDetails, ReasoningConfig, ResponseInput, ResponseStatus, Role,
    TextFormat, ToolChoice, ToolChoiceMode, TruncationStrategy, Usage,
};
use llm_gateway_sdk::models::items::{ItemStatus, MessageOutput, OutputItem};
use llm_gateway_sdk::models::plugin::{
    MediaInputMode, ProviderCallCtx, ProviderPluginCapabilities,
};
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayError,
    LlmGatewayProviderPluginClientV1, ResponseEventStream,
};
use model_registry_sdk::OpenAiSettingsV1;

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
        // Narrow the GTS model-info envelope to the provider's own typed view
        // via the registry SDK cast utility. This validates `gts_type` against
        // `OpenAiSettingsV1`'s GTS id and gives typed field access -- the real
        // OpenAI adapter would read connection routing this way.
        let info = call.typed_info::<OpenAiSettingsV1>().map_err(|e| {
            LlmGatewayError::internal(format!("failed to narrow OpenAI model info: {e}"))
        })?;

        let prompt = match body.input {
            Some(ResponseInput::Text(t)) => t,
            Some(ResponseInput::Items(_)) => "<structured input>".to_owned(),
            None => String::new(),
        };

        let reply = format!(
            "echo from provider_model_id={} via oagw_alias={}: {prompt}",
            info.provider_model_id, info.provider_settings.oagw_alias,
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
