//! Models for the provider-plugin interface.
//!
//! A provider plugin (see [`crate::plugin_api::LlmGatewayProviderPluginClientV1`])
//! isolates one LLM provider behind a common trait. These types carry the
//! per-call context the core Gateway hands to a plugin and the integration-level
//! capabilities a plugin reports. Per-model feature capabilities (vision,
//! function calling, streaming, …) are owned by Model Registry
//! (`ModelCapabilities`) and validated by the core before dispatch — they are
//! deliberately not duplicated here.

use model_registry_sdk::ModelInfoV1;
use serde::{Deserialize, Serialize};

/// Per-call context the core Gateway passes to every provider-plugin method.
///
/// Carries what translation and transport need without the plugin reaching into
/// Model Registry itself. The core builds it from the resolved model and the
/// request being served.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderCallCtx {
    /// The resolved model info the core fetched from Model Registry, returned
    /// unchanged. Its `gts_type` is the provider type and the authoritative
    /// routing key; it equals the resolving plugin's declared `provider_type`.
    /// `provider_settings` is the JSON document the plugin narrows to its own
    /// settings type via [`ProviderCallCtx::typed_settings`]; `base_url` and
    /// `secret_ref` are the general connection settings. `OAGW` injects
    /// credentials and applies circuit breaking; the plugin never reads or
    /// stores provider credentials.
    pub model_info: ModelInfoV1,

    /// Gateway request correlation id (the response `id`), propagated for
    /// tracing across usage, error, and audit events.
    pub request_id: String,
}

impl ProviderCallCtx {
    /// Narrow `provider_settings` to the plugin's settings type `P`: the struct
    /// the plugin declared as its provider type, derived from
    /// `gts.cf.genai.model.provider.v1~` (`ProviderTypeV1`). The model's
    /// `gts_type` must equal `P`'s GTS type id.
    ///
    /// # Errors
    ///
    /// - [`gts::NarrowError::SchemaId`] when `model_info.gts_type` is not `P`'s
    ///   type id (the plugin was handed a model of another provider type).
    /// - [`gts::NarrowError::Deserialize`] when `provider_settings` does not
    ///   deserialize into `P`.
    pub fn typed_settings<P>(&self) -> Result<P, gts::NarrowError>
    where
        P: gts::GtsSchema,
        for<'de> P: gts::GtsDeserialize<'de>,
    {
        gts::try_narrow::<P>(
            self.model_info.gts_type.as_ref(),
            self.model_info.provider_settings.clone(),
        )
    }
}

/// How a provider plugin consumes media referenced by input content parts
/// (`input_image`, `input_file`, `input_audio`, `input_video`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaInputMode {
    /// External / base64 URLs are forwarded to the provider as-is; the core
    /// performs no `FileStorage` prefetch.
    UrlForward,
    /// The core must fetch bytes from `FileStorage` before dispatch and hand the
    /// plugin resolved content.
    PrefetchRequired,
}

/// Integration-level capabilities a provider plugin reports.
///
/// These describe how the provider *integration* behaves — concerns Model
/// Registry does not track. Per-model feature capabilities live in
/// `ModelInfoV1.capabilities` / `supported_api` and are validated by the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderPluginCapabilities {
    /// Whether the provider integration can stream responses at all
    /// (independent of any individual model's `streaming` flag).
    pub streaming_transport: bool,

    /// How the plugin expects media input to be delivered.
    pub media_input: MediaInputMode,
}
