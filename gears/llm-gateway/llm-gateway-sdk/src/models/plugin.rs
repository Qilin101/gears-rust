//! Models for the provider-plugin interface.
//!
//! A provider plugin (see [`crate::plugin_api::LlmGatewayProviderPluginClient`])
//! isolates one LLM provider behind a common trait. These types carry the
//! per-call context the core Gateway hands to a plugin and the integration-level
//! capabilities a plugin reports. Per-model feature capabilities (vision,
//! function calling, streaming, …) are owned by Model Registry
//! (`ModelCapabilities`) and validated by the core before dispatch — they are
//! deliberately not duplicated here.

use gts::GtsTypeId;
use serde::{Deserialize, Serialize};

/// Per-call context the core Gateway passes to every provider-plugin method.
///
/// Carries what translation and transport need without the plugin reaching into
/// Model Registry itself. The core builds it from the resolved model
/// (`ModelInfoV1`) and the request being served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderCallCtx {
    /// Provider's own model identifier, sent on the wire
    /// (`ModelInfoV1.provider_model_id`).
    pub provider_model_id: String,

    /// Provider identity — the Model Registry model-info `gts_type`
    /// (e.g. `gts.cf.genai.model.info.v1~cf.genai._.openai.v1~`), the
    /// authoritative provider routing key. Equals the resolving plugin's
    /// declared `provider_type`; a plugin may use it to confirm it serves this
    /// provider.
    pub provider_type: GtsTypeId,

    /// Provider-specific settings payload (`ModelInfoV1.provider_settings`),
    /// carried as an opaque GTS value discriminated by `provider_type` — the
    /// same type-erased carrier Model Registry uses (`serde_json::Value`
    /// implements `gts::GtsSchema`). The plugin narrows it to its typed settings
    /// (e.g. `OpenAiSettingsV1`) via [`ProviderCallCtx::typed_settings`] and
    /// reads its own connection routing from there — including the `OAGW` alias
    /// when the provider uses one (some, e.g. local models, do not). `OAGW`
    /// injects credentials and applies circuit breaking; the plugin never reads
    /// or stores provider credentials.
    pub provider_settings: serde_json::Value,

    /// Gateway request correlation id (the response `id`), propagated for
    /// tracing across usage, error, and audit events.
    pub request_id: String,
}

impl ProviderCallCtx {
    /// Narrow [`Self::provider_settings`] to a concrete provider-settings type
    /// `Q`, validating it against `provider_type`. Mirrors Model Registry's
    /// `ModelV1::try_into_typed`.
    ///
    /// # Errors
    ///
    /// - [`gts::NarrowError::SchemaId`] when `provider_type` does not match
    ///   `Q`'s GTS type id (the plugin was handed a payload for another provider).
    /// - [`gts::NarrowError::Deserialize`] when the payload can't be
    ///   deserialized into `Q`.
    pub fn typed_settings<Q>(&self) -> Result<Q, gts::NarrowError>
    where
        Q: gts::GtsSchema,
        for<'de> Q: gts::GtsDeserialize<'de>,
    {
        gts::try_narrow::<Q>(self.provider_type.as_ref(), self.provider_settings.clone())
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
