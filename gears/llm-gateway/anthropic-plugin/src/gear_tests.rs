// Created: 2026-11-25 by Constructor Tech
//! The publication boundary: the runtime publishes the configured instance
//! before `init`; `init` binds the scoped client to the admitted manifest.

use std::sync::Arc;

use llm_gateway_sdk::LlmGatewayProviderPluginClientV1;
use oagw_sdk::ServiceGatewayClientV1;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::config::ConfigProvider;
use toolkit::context::GearCtx;
use toolkit::gts_publication::GtsPublicationCapability;
use uuid::Uuid;

use super::AnthropicPluginGear;
use crate::client::client_tests::FakeGateway;

const INSTANCE_ID: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.llmgw.provider.plugin.v1~cf.builtin.anthropic.plugin.v1";

struct Config(serde_json::Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

fn config() -> Config {
    Config(json!({
        AnthropicPluginGear::MODULE_NAME: { "config": { "vendor": "acme", "priority": 7 } }
    }))
}

fn ctx(hub: &Arc<ClientHub>) -> GearCtx {
    GearCtx::new(
        AnthropicPluginGear::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(config()),
        Arc::clone(hub),
        CancellationToken::new(),
    )
}

fn hub_with_gateway() -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    let gateway: Arc<dyn ServiceGatewayClientV1> =
        FakeGateway::new(|| unreachable!("init makes no provider call"));
    hub.register::<dyn ServiceGatewayClientV1>(gateway);
    hub
}

fn bound(hub: &ClientHub) -> bool {
    hub.try_get_scoped::<dyn LlmGatewayProviderPluginClientV1>(&ClientScope::gts_id(INSTANCE_ID))
        .is_some()
}

/// The manifest is the configured instance, serving the Anthropic provider
/// type and the `anthropic` tool namespace, without `init`.
#[test]
fn the_manifest_is_the_configured_instance() {
    let gear = AnthropicPluginGear::default();
    let manifest = gear.descriptor().build_gts_manifest(&config()).unwrap();

    let documents: Vec<_> = manifest
        .documents()
        .iter()
        .map(|(id, document)| (id.as_ref().to_owned(), document.clone()))
        .collect();
    assert_eq!(
        documents,
        [(
            INSTANCE_ID.to_owned(),
            json!({
                "id": INSTANCE_ID,
                "vendor": "acme",
                "priority": 7,
                "properties": {
                    "provider_type": "gts.cf.genai.model.provider.v1~cf.genai._.anthropic.v1~",
                    "tool_namespace": "anthropic"
                }
            })
        )]
    );
    assert_eq!(manifest.publisher_name(), "llm-gateway-anthropic-plugin");
    assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn init_binds_the_admitted_instance() {
    let hub = hub_with_gateway();
    let gear = AnthropicPluginGear::default();
    let manifest = Arc::new(gear.descriptor().build_gts_manifest(&config()).unwrap());

    gear.init(&ctx(&hub).with_gts_publication_manifest_for_test(manifest))
        .await
        .expect("init with an admitted manifest");

    assert!(bound(&hub));
}

/// Without an admitted manifest — outside the runtime — `init` fails and
/// binds nothing.
#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing() {
    let hub = hub_with_gateway();

    let err = AnthropicPluginGear::default()
        .init(&ctx(&hub))
        .await
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("admitted GTS manifest"),
        "{err:#}"
    );
    assert!(!bound(&hub));
}

#[tokio::test]
async fn init_without_oagw_binds_nothing() {
    let hub = Arc::new(ClientHub::new());
    let gear = AnthropicPluginGear::default();
    let manifest = Arc::new(gear.descriptor().build_gts_manifest(&config()).unwrap());

    let err = gear
        .init(&ctx(&hub).with_gts_publication_manifest_for_test(manifest))
        .await
        .unwrap_err();

    assert!(format!("{err:#}").contains("OAGW"), "{err:#}");
    assert!(!bound(&hub));
}
