//! End-to-end demo of the LLM Gateway SDK traits against the Model Registry
//! contract.
//!
//! Wires three mock SDK implementations together and drives the contract:
//!
//! ```text
//! 0. plugin publishes its provider type (derived from gts.cf.genai.model.provider.v1~)
//!    → the registry validates each model's provider_settings against it
//!
//! main → LlmGatewayClientV1::create_response  (openai::gpt-4o)
//!          → ModelRegistryClientV1::resolve_model
//!          → plugin selected by info.gts_type, settings narrowed to the plugin type
//!          → ResponseResource (success)
//!
//! main → create_response (openai::gpt-4o-mini) → registry: MODEL_NOT_APPROVED → model_not_found
//! main → create_response (anthropic::claude-sonnet-4) → provider_not_supported
//!
//! main → ModelRegistryClientV1::list_available_models
//!          → QueryBuilder<ModelSchema> + typed FieldRefs, then the same as $filter text
//! ```
//!
//! Run with: `cargo run -p cf-gears-llm-gateway-demo`
//!
//! Pass `--provider-type-schema` to print the JSON schema of the provider base
//! type and of the demo plugin's derived provider type, then exit.

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
    MODEL_API_COMPLETION, MODEL_CANONICAL_ID, MODEL_GTS_TYPE, MODEL_STREAMING, ModelFilterField,
    ModelSchema, QueryBuilder, SortDir,
};
use model_registry_sdk::{ModelRegistryClientV1, ModelV1, ProviderTypeV1};
use toolkit_odata::filter::FilterField as _;
use toolkit_odata::pagination::short_filter_hash;
use toolkit_odata::{ODataQuery, parse_filter_string};
use toolkit_security::SecurityContext;

use gateway::DemoGateway;
use mock_plugin::{MockOpenAiPlugin, OpenAiProviderSettingsV1};
use mock_registry::{ANTHROPIC_PROVIDER_TYPE, MockModelRegistry, OPENAI_PROVIDER_TYPE};

type DemoResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The derived provider type's schema with the base type inlined, the shape
/// Types Registry hands back as the effective schema. The derived schema
/// composes as `allOf: [$ref base, overlay]`; the registry cannot resolve a
/// `gts://` reference by itself, so the base is substituted here.
fn effective_provider_type_schema() -> serde_json::Value {
    let base = <ProviderTypeV1>::gts_schema();
    let mut derived = OpenAiProviderSettingsV1::gts_schema();
    if let Some(branches) = derived.get_mut("allOf").and_then(|v| v.as_array_mut()) {
        for branch in branches.iter_mut() {
            if branch.get("$ref").is_some() {
                *branch = base.clone();
            }
        }
    }
    derived
}

/// Print the JSON schema of the provider base type and of the demo plugin's
/// derived provider type, then exit.
fn print_provider_type_schema() -> DemoResult {
    println!("GTS type id: {}", <ProviderTypeV1>::TYPE_ID);
    println!(
        "{}",
        serde_json::to_string_pretty(&<ProviderTypeV1>::gts_schema())?
    );
    println!("\nGTS type id: {}", OpenAiProviderSettingsV1::TYPE_ID);
    println!(
        "{}",
        serde_json::to_string_pretty(&OpenAiProviderSettingsV1::gts_schema())?
    );
    Ok(())
}

/// What Model Registry does on every model write: validate `provider_settings`
/// against the `properties.provider_settings` subschemas of the provider
/// type's effective schema.
fn validate_settings_like_the_registry(models: &[&ModelV1]) -> DemoResult {
    println!("\n─── Step 0: provider type schema check (what Model Registry does on write) ───");
    let effective = effective_provider_type_schema();
    let mut subschemas = Vec::new();
    for branch in effective["allOf"].as_array().into_iter().flatten() {
        if let Some(sub) = branch.pointer("/properties/provider_settings") {
            subschemas.push(sub.clone());
        }
    }
    // Nested types are referenced through the root `definitions`, which the
    // registry carries along with the subschemas.
    let mut combined = serde_json::json!({ "allOf": subschemas });
    if let Some(defs) = effective.get("definitions") {
        combined["definitions"] = defs.clone();
    }
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .build(&combined)?;
    println!(
        "→ {} narrows provider_settings with {} subschema(s)",
        OpenAiProviderSettingsV1::TYPE_ID,
        combined["allOf"].as_array().map_or(0, Vec::len)
    );
    for model in models {
        if model.info.gts_type.as_ref() != OpenAiProviderSettingsV1::TYPE_ID {
            println!("←   {}: other provider type, skipped", model.canonical_id);
            continue;
        }
        let errors: Vec<String> = validator
            .iter_errors(&model.info.provider_settings)
            .map(|e| format!("{}: {e}", e.instance_path()))
            .collect();
        if errors.is_empty() {
            println!("←   {}: provider_settings valid", model.canonical_id);
        } else {
            println!("←   {}: SCHEMA_MISMATCH {errors:?}", model.canonical_id);
        }
    }
    let bad = serde_json::json!({ "cost": { "input_per_1k_micro": "free" } });
    let errors: Vec<String> = validator
        .iter_errors(&bad)
        .map(|e| format!("provider_settings{}: {e}", e.instance_path()))
        .collect();
    println!("←   a settings document without oagw_alias and a string cost: {errors:?}");
    Ok(())
}

#[tokio::main]
async fn main() -> DemoResult {
    if std::env::args().any(|a| a == "--provider-type-schema") {
        return print_provider_type_schema();
    }

    // --- Wire the mock dependencies as SDK trait objects. ---
    let mock_registry = MockModelRegistry::new()?;
    let fixtures: Vec<&ModelV1> = mock_registry.models().collect();
    validate_settings_like_the_registry(&fixtures)?;
    drop(fixtures);
    let registry: Arc<dyn ModelRegistryClientV1> = Arc::new(mock_registry);

    let plugin: Arc<dyn LlmGatewayProviderPluginClientV1> = Arc::new(MockOpenAiPlugin);
    let capabilities = plugin.capabilities();
    println!(
        "\n→ registered plugin for {} (streaming={}, media={:?}, web_search={})",
        OPENAI_PROVIDER_TYPE,
        capabilities.streaming_transport,
        capabilities.media_input,
        capabilities.web_search,
    );
    assert_eq!(
        OpenAiProviderSettingsV1::TYPE_ID,
        OPENAI_PROVIDER_TYPE,
        "the plugin's settings type is its provider type"
    );

    let mut plugins: HashMap<GtsTypeId, Arc<dyn LlmGatewayProviderPluginClientV1>> = HashMap::new();
    plugins.insert(GtsTypeId::new(OPENAI_PROVIDER_TYPE), plugin);

    let gateway = DemoGateway::new(Arc::clone(&registry), plugins);
    let ctx = SecurityContext::anonymous();

    // --- 1. OpenAI model, plugin exists → success. ---
    println!("\n─── Request 1: openai::gpt-4o (approved, plugin exists) ───");
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
            "← usage    : in={} out={} total={} cost={} credits",
            usage.input_tokens, usage.output_tokens, usage.total_tokens, usage.cost
        );
    }
    if let Some(details) = &response.provider_details {
        println!(
            "← provider : stop_reason={:?} request_id={:?}",
            details.stop_reason, details.provider_request_id
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

    // --- 2. Revoked model → the registry's approval gate, reported as
    //        model_not_found like any model unavailable for the tenant. ---
    println!("\n─── Request 2: openai::gpt-4o-mini (revoked) ───");
    let request = CreateResponseBody {
        model: "openai::gpt-4o-mini".to_owned(),
        input: Some(ResponseInput::Text("Still there?".to_owned())),
        ..Default::default()
    };
    println!("→ create_response(model = {})", request.model);
    match gateway.create_response(&ctx, request).await {
        Err(e) => println!("← error     : {e}"),
        Ok(_) => println!("← UNEXPECTED: a revoked model resolved"),
    }

    // --- 3. Anthropic model, no plugin → provider_not_supported. ---
    println!("\n─── Request 3: anthropic::claude-sonnet-4 (no plugin) ───");
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

    // --- 4. Unknown model → model not found. ---
    println!("\n─── Request 4: openai::nope (unknown) ───");
    match gateway
        .create_response(
            &ctx,
            CreateResponseBody {
                model: "openai::nope".to_owned(),
                ..Default::default()
            },
        )
        .await
    {
        Err(e) => println!("← error     : {e}"),
        Ok(_) => println!("← UNEXPECTED: an unknown model resolved"),
    }

    // --- 5/6. List the available models through the OData surface. ---
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
) -> DemoResult {
    println!("\n─── Request 5: list_available_models via the typed QueryBuilder ───");

    // The question a gateway asks: which available models can I route to the
    // OpenAI plugin, streaming, over the completion API?
    let query = QueryBuilder::<ModelSchema>::new()
        .filter(
            MODEL_GTS_TYPE
                .eq(OPENAI_PROVIDER_TYPE)
                .and(MODEL_STREAMING.eq(true))
                .and(MODEL_API_COMPLETION.eq(true)),
        )
        .order_by(MODEL_CANONICAL_ID, SortDir::Asc)
        .page_size(10)
        .build();
    println!(
        "→ built    : filter_hash={:?}, limit={:?}",
        query.filter_hash, query.limit
    );

    let page = registry.list_available_models(ctx, &query).await?;
    println!(
        "← matched  : {} of 3 fixtures (the revoked one is never listed; limit={})",
        page.items.len(),
        page.page_info.limit
    );
    for model in &page.items {
        println!(
            "←   {} — approval={}, streaming={}, vision={}, sort_order={}",
            model.canonical_id,
            model.approval.state.as_str(),
            model.info.capabilities.streaming,
            model.info.capabilities.vision.enabled,
            model.info.sort_order,
        );
    }

    Ok(())
}

/// Same listing expressed as raw `$filter` text, the shape that arrives over
/// HTTP.
async fn list_models_with_filter_text(
    registry: &dyn ModelRegistryClientV1,
    ctx: &SecurityContext,
) -> DemoResult {
    println!("\n─── Request 6: the same listing as raw $filter text ───");

    let filter = format!(
        "{gts_type} eq '{OPENAI_PROVIDER_TYPE}' and {streaming} eq true",
        gts_type = ModelFilterField::GtsType.name(),
        streaming = ModelFilterField::Streaming.name(),
    );
    println!("→ $filter  : {filter}");

    let parsed = parse_filter_string(&filter)?;
    let mut query = ODataQuery::default().with_limit(10);
    if let Some(hash) = short_filter_hash(Some(parsed.as_expr())) {
        query = query.with_filter_hash(hash);
    }
    let query = query.with_filter(parsed.into_expr());

    let page = registry.list_available_models(ctx, &query).await?;
    println!("← matched  : {} of 3 fixtures", page.items.len());

    // Field names outside the allowlist never reach the backend: the SDK enum
    // is the same gate the real repository applies before touching a column.
    for name in [
        "vision",
        "info.capabilities.vision",
        "cost",
        "approval_state",
    ] {
        println!(
            "→ filterable(`{name}`) : {}",
            ModelFilterField::from_name(name).is_some()
        );
    }

    Ok(())
}
