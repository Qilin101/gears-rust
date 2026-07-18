//! End-to-end demo of the LLM Gateway SDK traits.
//!
//! Wires three mock SDK implementations together and drives two requests:
//!
//! ```text
//! main → LlmGatewayClientV1::create_response  (openai::gpt-4o)
//!          → ModelRegistryClientV1::get_tenant_model
//!          → LlmGatewayProviderPluginClientV1::create_response
//!          → ResponseResource (success)
//!
//! main → LlmGatewayClientV1::create_response  (anthropic::claude-sonnet-4)
//!          → ModelRegistryClientV1::get_tenant_model
//!          → no provider plugin registered → LlmGatewayError (expected)
//! ```
//!
//! Run with: `cargo run -p cf-gears-llm-gateway-demo`
//!
//! Pass `--openai-settings-schema` to instead print the JSON schema for the
//! `OpenAiSettingsV1` provider-settings GTS type and exit.

// Demo binary: `{:?}` is the natural way to print SDK enums/values here,
// and unicode arrows (`→`, `←`) make terminal output readable.
#![allow(clippy::use_debug, clippy::non_ascii_literal)]

mod gateway;
mod mock_plugin;
mod mock_registry;

use std::collections::HashMap;
use std::sync::Arc;

use gts::{GtsSchema, GtsTypeId};
use llm_gateway_sdk::models::content::OutputContentPart;
use llm_gateway_sdk::models::core::ResponseInput;
use llm_gateway_sdk::models::items::OutputItem;
use llm_gateway_sdk::{CreateResponseBody, LlmGatewayClientV1, LlmGatewayProviderPluginClientV1};
use model_registry_sdk::{ModelInfoV1, ModelRegistryClientV1, OpenAiSettingsV1};
use toolkit_security::SecurityContext;

use gateway::DemoGateway;
use mock_plugin::MockOpenAiPlugin;
use mock_registry::{ANTHROPIC_PROVIDER_TYPE, MockModelRegistry, OPENAI_PROVIDER_TYPE};

/// Print the JSON schema for the `OpenAiSettingsV1` provider-settings GTS type
/// and the base `ModelInfoV1` envelope it derives from.
fn print_openai_settings_schema() -> Result<(), Box<dyn std::error::Error>> {
    println!("GTS type id: {}", <ModelInfoV1>::TYPE_ID);
    println!(
        "{}",
        serde_json::to_string_pretty(&<ModelInfoV1>::gts_schema())?
    );

    println!("\nGTS type id: {}", OpenAiSettingsV1::TYPE_ID);
    println!(
        "{}",
        serde_json::to_string_pretty(&OpenAiSettingsV1::gts_schema())?
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--openai-settings-schema") {
        return print_openai_settings_schema();
    }

    // --- Wire the mock dependencies as SDK trait objects. ---
    let registry: Arc<dyn ModelRegistryClientV1> = Arc::new(MockModelRegistry::new()?);

    let plugin: Arc<dyn LlmGatewayProviderPluginClientV1> = Arc::new(MockOpenAiPlugin);
    println!(
        "→ registered plugin for {} (streaming={item}, media={md:?})",
        OPENAI_PROVIDER_TYPE,
        item = plugin.capabilities().streaming_transport,
        md = plugin.capabilities().media_input,
    );

    let mut plugins: HashMap<GtsTypeId, Arc<dyn LlmGatewayProviderPluginClientV1>> = HashMap::new();
    plugins.insert(GtsTypeId::new(OPENAI_PROVIDER_TYPE), plugin);

    let gateway = DemoGateway::new(registry, plugins);
    let ctx = SecurityContext::anonymous();

    // --- 1. OpenAI model -- existing plugin → success. ---
    println!("\n─── Request 1: openai::gpt-4o (plugin exists) ───");
    let request = CreateResponseBody {
        model: "openai::gpt-4o".to_owned(),
        input: Some(ResponseInput::Text("Hello, gateway!".to_owned())),
        ..Default::default()
    };
    println!("→ create_response(model = {})", request.model);
    let response = gateway.create_response(&ctx, request).await?;
    println!("← status   : {:?}", response.status);
    println!("← model    : {}", response.model);
    if let Some(usage) = &response.usage {
        println!(
            "← usage    : in={} out={} total={}",
            usage.input_tokens, usage.output_tokens, usage.total_tokens
        );
    }
    for item in &response.output {
        if let OutputItem::Message(msg) = item {
            for part in &msg.content {
                if let OutputContentPart::OutputText(text) = part {
                    println!("← output   : {}", text.text);
                }
            }
        }
    }

    // --- 2. Anthropic model -- no plugin → clean error. ---
    println!("\n─── Request 2: anthropic::claude-sonnet-4 (no plugin) ───");
    let request = CreateResponseBody {
        model: "anthropic::claude-sonnet-4".to_owned(),
        input: Some(ResponseInput::Text("Translate this".to_owned())),
        ..Default::default()
    };
    println!("→ create_response(model = {})", request.model);
    match gateway.create_response(&ctx, request).await {
        Err(e) => println!("← error     : {e}"),
        Ok(_) => println!(
            "← UNEXPECTED: should have failed -- no plugin registered for {ANTHROPIC_PROVIDER_TYPE}"
        ),
    }

    Ok(())
}
