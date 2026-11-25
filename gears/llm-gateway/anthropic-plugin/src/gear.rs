// Created: 2026-11-25 by Constructor Tech
//! `ToolKit` gear registration for the Anthropic provider plugin.
//!
//! The runtime publishes the plugin instance built from the configuration
//! (`entities`) and calls `init` only once it is admitted; `init` binds the
//! scoped `LlmGatewayProviderPluginClientV1` client to the admitted instance.
//! The provider settings type (`AnthropicProviderSettingsV1`) is a static GTS
//! declaration seeded by Types Registry.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use gts::{GtsId, GtsSchema, GtsTypeId};
use llm_gateway_sdk::{LlmGatewayProviderPluginClientV1, LlmGatewayProviderPluginSpecV1};
use oagw_sdk::ServiceGatewayClientV1;
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;

use crate::client::AnthropicPlugin;
use crate::config::AnthropicPluginConfig;
use crate::request::ANTHROPIC_PREFIX;
use crate::settings::AnthropicProviderSettingsV1;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.builtin.anthropic.plugin.v1";

/// Anthropic provider plugin gear.
#[toolkit::gear(
    name = "llm-gateway-anthropic-plugin",
    deps = [oagw],
    gts_publication(entities = Self::plugin_instance),
)]
#[derive(Default)]
pub struct AnthropicPluginGear {
    plugin: OnceLock<Arc<dyn LlmGatewayProviderPluginClientV1>>,
}

impl AnthropicPluginGear {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg: AnthropicPluginConfig = inputs.config_or_default()?;
        Ok(vec![registration(cfg.vendor, cfg.priority)?])
    }
}

#[async_trait]
impl Gear for AnthropicPluginGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: AnthropicPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );
        let gateway = ctx
            .client_hub()
            .get::<dyn ServiceGatewayClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get OAGW gateway: {e}"))?;
        let plugin: Arc<dyn LlmGatewayProviderPluginClientV1> =
            Arc::new(AnthropicPlugin::new(gateway));
        self.plugin
            .set(Arc::clone(&plugin))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        bind_plugin_instances::<LlmGatewayProviderPluginSpecV1, _>(
            &ctx.client_hub(),
            ctx.gts_publication_manifest()?.documents(),
            &plugin,
        )
    }
}

/// The plugin instance for `vendor` and `priority`.
fn registration(vendor: String, priority: i16) -> anyhow::Result<(GtsId, serde_json::Value)> {
    let id = LlmGatewayProviderPluginSpecV1::gts_make_instance_id(INSTANCE_SEGMENT);
    let instance = PluginV1::<LlmGatewayProviderPluginSpecV1> {
        id: id.clone(),
        vendor,
        priority,
        properties: LlmGatewayProviderPluginSpecV1 {
            provider_type: GtsTypeId::new(AnthropicProviderSettingsV1::TYPE_ID),
            tool_namespace: ANTHROPIC_PREFIX.trim_end_matches(':').to_owned(),
        },
    };
    Ok((
        GtsId::try_new(id.as_ref())?,
        serde_json::to_value(&instance)?,
    ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
