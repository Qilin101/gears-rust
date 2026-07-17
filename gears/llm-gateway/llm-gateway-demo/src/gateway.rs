//! Demo [`LlmGatewayClientV1`] implementation.
//!
//! `create_response` performs the core orchestration the design calls for:
//! resolve the model via Model Registry, select the provider plugin by the
//! model's `gts_type`, build a [`ProviderCallCtx`], and delegate. Streaming and
//! embeddings are out of scope for this demo.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gts::GtsTypeId;
use llm_gateway_sdk::models::plugin::ProviderCallCtx;
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayClientV1, LlmGatewayError,
    LlmGatewayProviderPluginClient, ResponseEventStream, ResponseResource,
};
use model_registry_sdk::{ModelRegistryClientV1, ModelRegistryError};
use toolkit_security::SecurityContext;

/// Demo gateway holding its dependencies as SDK trait objects -- exactly how the
/// real gear would resolve them from `ClientHub`.
pub struct DemoGateway {
    registry: Arc<dyn ModelRegistryClientV1>,
    /// Provider plugins keyed by the Model Registry provider `gts_type` each
    /// one declares it serves.
    plugins: HashMap<GtsTypeId, Arc<dyn LlmGatewayProviderPluginClient>>,
}

impl DemoGateway {
    pub fn new(
        registry: Arc<dyn ModelRegistryClientV1>,
        plugins: HashMap<GtsTypeId, Arc<dyn LlmGatewayProviderPluginClient>>,
    ) -> Self {
        Self { registry, plugins }
    }
}

#[async_trait]
impl LlmGatewayClientV1 for DemoGateway {
    async fn create_response(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError> {
        // 1. Resolve the model from Model Registry.
        let model = self
            .registry
            .get_tenant_model(ctx, &body.model)
            .await
            .map_err(map_registry_err)?;

        // 2. Select the provider plugin by the model's provider identity.
        let provider_type = model.info.gts_type.clone();
        let plugin = self.plugins.get(&provider_type).ok_or_else(|| {
            LlmGatewayError::provider_error(format!(
                "no provider plugin registered for {}",
                provider_type.as_ref()
            ))
        })?;

        // 3. Build the per-call context from the resolved model.
        let call = ProviderCallCtx {
            model_info: model.info.clone(),
            request_id: "resp_demo_0001".to_owned(),
        };

        // 4. Delegate translation + transport to the plugin.
        plugin.create_response(ctx, &call, body).await
    }

    async fn create_response_stream(
        &self,
        _ctx: &SecurityContext,
        _body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        Err(LlmGatewayError::capability_not_supported(
            "streaming not implemented in demo",
        ))
    }

    async fn create_embedding(
        &self,
        _ctx: &SecurityContext,
        _req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        Err(LlmGatewayError::capability_not_supported(
            "embeddings not implemented in demo",
        ))
    }
}

fn map_registry_err(err: ModelRegistryError) -> LlmGatewayError {
    match err {
        ModelRegistryError::ModelNotFound { canonical_id } => {
            LlmGatewayError::model_not_found(canonical_id)
        }
        ModelRegistryError::ModelNotApproved { canonical_id } => {
            LlmGatewayError::model_not_approved(canonical_id)
        }
        other => LlmGatewayError::internal(other.to_string()),
    }
}
