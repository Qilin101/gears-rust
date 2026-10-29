//! GTS schema definition for LLM Gateway provider plugins.
//!
//! The LLM Gateway (main gear) registers this schema in the types-registry
//! during `init()`. Each provider plugin registers an instance of it (plus a
//! scoped `ClientHub` client) so the gateway can discover and resolve the
//! plugin at runtime.
//!
//! Selection keys on **provider identity**: the instance's `provider_type`
//! carries the Model Registry provider type the plugin serves, a GTS type
//! derived from `gts.cf.genai.model.provider.v1~` (e.g.
//! `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~`). The gateway
//! resolves the model, reads `ModelInfoV1.gts_type`, and picks the plugin
//! whose `provider_type` matches (ties broken on `PluginV1.priority`). The
//! plugin also publishes that provider type's schema, which narrows
//! `provider_settings`; Model Registry validates model settings against it.

use gts::GtsTypeId;
use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

/// GTS type definition for LLM Gateway provider plugin instances.
///
/// # Instance ID format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~cf.llmgw.provider.plugin.v1~<vendor>.<package>.<name>.plugin.v1
/// ```
///
/// # Example
///
/// ```ignore
/// let instance_id = LlmGatewayProviderPluginSpecV1::gts_make_instance_id(
///     "cf.builtin.openai.plugin.v1",
/// );
///
/// let instance = PluginV1::<LlmGatewayProviderPluginSpecV1> {
///     id: instance_id.clone(),
///     vendor: "constructorfabric".to_owned(),
///     priority: 100,
///     properties: LlmGatewayProviderPluginSpecV1 {
///         provider_type: GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~"),
///     },
/// };
///
/// registry.register(vec![serde_json::to_value(&instance)?]).await?;
/// ```
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.llmgw.provider.plugin.v1~"),
    description = "LLM Gateway provider plugin specification",
    properties = "provider_type,tool_namespace",
)]
pub struct LlmGatewayProviderPluginSpecV1 {
    /// Model Registry provider type this plugin serves: a GTS type derived
    /// from `gts.cf.genai.model.provider.v1~` (e.g.
    /// `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~`). Matched
    /// against `ModelInfoV1.gts_type` during plugin resolution.
    pub provider_type: GtsTypeId,
    /// Prefix of the provider-specific tools this plugin accepts (e.g.
    /// `openai` for `openai:shell`).
    pub tool_namespace: String,
}
