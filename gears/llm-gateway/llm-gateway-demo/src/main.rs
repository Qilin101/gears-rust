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
//!
//! main → ModelRegistryClientV1::list_tenant_models
//!          → QueryBuilder<ModelSchema> + typed FieldRefs (no $filter text)
//!          → Page<ModelV1> (filter evaluated in the mock)
//!
//! main → ModelRegistryClientV1::list_tenant_models
//!          → the same listing as raw $filter text, for comparison
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
use model_registry_sdk::odata::{
    MODEL_CANONICAL_ID, MODEL_GTS_TYPE, MODEL_LIFECYCLE_STATUS, MODEL_STREAMING, ModelFilterField,
    ModelSchema, QueryBuilder, SortDir,
};
use model_registry_sdk::{LifecycleStatus, ModelInfoV1, ModelRegistryClientV1, OpenAiSettingsV1};
use toolkit_odata::filter::FilterField as _;
use toolkit_odata::pagination::short_filter_hash;
use toolkit_odata::{ODataQuery, parse_filter_string};
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

    let gateway = DemoGateway::new(Arc::clone(&registry), plugins);
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

    // --- 3/4. List models through the OData surface the SDK publishes. ---
    list_models_with_builder(registry.as_ref(), &ctx).await?;
    list_models_with_filter_text(registry.as_ref(), &ctx).await?;

    Ok(())
}

/// List models with a query built from the SDK's typed field references.
///
/// `QueryBuilder` assembles the filter AST directly: no `$filter` text, so no
/// quoting or escaping of the values, and `build()` computes the `filter_hash`
/// that cursor pagination validates.
async fn list_models_with_builder(
    registry: &dyn ModelRegistryClientV1,
    ctx: &SecurityContext,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n─── Request 3: list_tenant_models via the typed QueryBuilder ───");

    // The question a gateway actually asks: which production models can I route
    // to the OpenAI plugin, streaming? `MODEL_LIFECYCLE_STATUS.eq(..)` takes the
    // SDK enum, so a misspelled status is a compile error.
    let query = QueryBuilder::<ModelSchema>::new()
        .filter(
            MODEL_GTS_TYPE
                .eq(OPENAI_PROVIDER_TYPE)
                .and(MODEL_STREAMING.eq(true))
                .and(MODEL_LIFECYCLE_STATUS.eq(LifecycleStatus::Production)),
        )
        .order_by(MODEL_CANONICAL_ID, SortDir::Asc)
        .page_size(10)
        .build();
    println!(
        "→ built    : filter_hash={:?}, limit={:?}",
        query.filter_hash, query.limit
    );

    let page = registry.list_tenant_models(ctx, &query).await?;
    println!(
        "← matched  : {} of 2 fixtures (limit={})",
        page.items.len(),
        page.page_info.limit
    );
    for model in &page.items {
        println!(
            "←   {} — streaming={}, vision={}",
            model.canonical_id,
            model.info.capabilities.streaming,
            model.info.capabilities.vision.enabled,
        );
    }

    Ok(())
}

/// Same listing expressed as raw `$filter` text — the shape that arrives over
/// HTTP, and the fallback when a filter is assembled at runtime.
///
/// Field names still come from [`ModelFilterField`] rather than string
/// literals, but the values are interpolated into the query text, so this form
/// has to worry about quoting in a way the builder does not.
async fn list_models_with_filter_text(
    registry: &dyn ModelRegistryClientV1,
    ctx: &SecurityContext,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n─── Request 4: the same listing as raw $filter text ───");

    let filter = format!(
        "{gts_type} eq '{OPENAI_PROVIDER_TYPE}' and {streaming} eq true",
        gts_type = ModelFilterField::GtsType.name(),
        streaming = ModelFilterField::Streaming.name(),
    );
    println!("→ $filter  : {filter}");

    let parsed = parse_filter_string(&filter)?;
    let mut query = ODataQuery::default().with_limit(10);
    // The builder does this step for you.
    if let Some(hash) = short_filter_hash(Some(parsed.as_expr())) {
        query = query.with_filter_hash(hash);
    }
    let query = query.with_filter(parsed.into_expr());

    let page = registry.list_tenant_models(ctx, &query).await?;
    println!("← matched  : {} of 2 fixtures", page.items.len());

    // Field names outside the allowlist never reach the backend: the SDK enum
    // is the same gate the real repository applies before touching a column.
    for name in ["vision", "info.capabilities.vision", "cost"] {
        println!(
            "→ filterable(`{name}`) : {}",
            ModelFilterField::from_name(name).is_some()
        );
    }

    Ok(())
}
