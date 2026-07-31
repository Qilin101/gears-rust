use std::collections::{HashMap, HashSet};

use chrono::Utc;
use gts::GtsSchema;
use model_registry_sdk::models::{
    ApprovalStatus, ContextWindow, CreateModelRequestV1, CreateProviderRequestV1,
    DefaultInferenceParametersV1, DisabledCapabilities, DisabledMediaCapability,
    DisabledReasoningCapability, DisabledWebSearchCapability, LifecycleStatus, MediaCapability,
    ModelCapabilities, ModelInfoV1, ModelPerformance, ModelV1, OpenAiSettingsV1, ProviderStatus,
    ReasoningCapability, SupportedApi, UpdateModelRequestV1, UpdateProviderRequestV1,
    WebSearchCapability,
};
use serde_json::json;
use uuid::Uuid;

use super::super::entity;

use super::{
    model_create_active_model, model_entity_to_v1, model_update_active_model,
    provider_create_active_model, provider_entity_to_v1, provider_update_active_model,
};

// ---------------------------------------------------------------------------
// Test helpers — construct domain types via struct literals.
// (SDK entity/info structs are not #[non_exhaustive], so direct construction
// is supported and avoids the JSON serialize→deserialize round-trip.)
// ---------------------------------------------------------------------------

fn test_tenant_id() -> Uuid {
    Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
}

fn test_provider_id() -> Uuid {
    Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
}

fn test_model_id() -> Uuid {
    Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()
}

/// Build a `ModelInfoV1` via direct struct literal construction.
fn make_info(gts_leaf: &str, provider_settings: &serde_json::Value) -> ModelInfoV1 {
    let gts_type = format!("gts.cf.genai.model.info.v1~{gts_leaf}");
    ModelInfoV1 {
        gts_type: gts::GtsTypeId::new(&gts_type),
        display_name: "GPT-4o".to_owned(),
        description: Some("OpenAI's flagship model".to_owned()),
        family: Some("gpt-4".to_owned()),
        vendor: Some("OpenAI".to_owned()),
        managed: false,
        architecture: Some("transformer".to_owned()),
        size_bytes: None,
        format: Some("api-only".to_owned()),
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: Some("1.0".to_owned()),
        sort_order: Some(10),
        icon: None,
        multiplier_display: None,
        performance: ModelPerformance {
            response_latency_ms: Some(500),
            tokens_per_second: Some(100),
        },
        additional_info: HashMap::new(),
        supported_api: HashSet::from([SupportedApi::Completion]),
        provider_model_id: "gpt-4o".to_owned(),
        capabilities: ModelCapabilities {
            vision: MediaCapability {
                enabled: true,
                supported_mime_types: vec!["image/png".to_owned()],
            },
            reasoning: ReasoningCapability {
                effort: true,
                toggle: false,
                resume: false,
                budget: false,
            },
            function_calling: true,
            response_schema: true,
            streaming: true,
            file_input: MediaCapability::default(),
            image_generation: MediaCapability::default(),
            audio_input: MediaCapability::default(),
            audio_output: MediaCapability::default(),
            code_interpreter: false,
            web_search: WebSearchCapability {
                enabled: false,
                allowed_domains: false,
                excluded_domains: false,
            },
        },
        disabled_capabilities: DisabledCapabilities {
            vision: DisabledMediaCapability::default(),
            reasoning: DisabledReasoningCapability::default(),
            function_calling: false,
            response_schema: false,
            streaming: false,
            file_input: DisabledMediaCapability::default(),
            image_generation: DisabledMediaCapability::default(),
            audio_input: DisabledMediaCapability::default(),
            audio_output: DisabledMediaCapability::default(),
            code_interpreter: false,
            web_search: DisabledWebSearchCapability::default(),
        },
        context_window: ContextWindow {
            max_input_tokens: 128_000,
            max_output_tokens: Some(16_384),
            output_vector_size: None,
        },
        default_parameters: DefaultInferenceParametersV1::default(),
        allow_parameter_override: true,
        allow_extra_params: vec!["custom_param".to_owned()],
        provider_settings: provider_settings.clone(),
    }
}

fn openai_settings() -> serde_json::Value {
    json!({
        "oagw_alias": "openai-prod",
        "endpoint_kind": "chat_completions",
        "temperature": 0.7,
        "max_tokens": 4096,
    })
}

fn make_provider_entity(
    id: Uuid,
    tenant_id: Uuid,
    slug: &str,
    status: &str,
) -> entity::provider::Model {
    entity::provider::Model {
        id,
        tenant_id,
        slug: slug.to_owned(),
        name: format!("Provider {slug}"),
        gts_type: "gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~".to_owned(),
        status: status.to_owned(),
        managed: false,
        metadata: Some(json!({"region": "us-east"})),
        discovery_enabled: false,
        discovery_interval_seconds: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn make_model_entity(
    id: Uuid,
    provider_id: Uuid,
    tenant_id: Uuid,
    canonical_id: &str,
    _info: Option<serde_json::Value>,
) -> entity::model::Model {
    // The `_info` argument is intentionally ignored: there is no `info`
    // column, so per-test customizations must be applied to the returned
    // entity via direct field assignment (e.g. `entity.vendor = Some(...)`).
    // The fixture populates the promoted scalar columns, the JSONB
    // sub-objects, and the OData-filterable columns so any test that builds an
    // entity through this helper gets a fully-shaped `models` row.
    let _ = canonical_id;
    entity::model::Model {
        id,
        provider_id,
        tenant_id,
        canonical_id: canonical_id.to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: Some(openai_settings()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        // Promoted scalar columns
        display_name: "GPT-4o".to_owned(),
        description: Some("OpenAI's flagship model".to_owned()),
        size_bytes: None,
        region: Some("us-east-1".to_owned()),
        hosted_by: Some("OpenAI".to_owned()),
        last_release_at: None,
        reasoning_level: Some("high".to_owned()),
        version: Some("1.0".to_owned()),
        sort_order: Some(10),
        icon: None,
        multiplier_display: Some("1x".to_owned()),
        perf_response_latency_ms: Some(500),
        perf_tokens_per_second: Some(100),
        ctx_max_input_tokens: 128_000,
        ctx_max_output_tokens: Some(16_384),
        ctx_output_vector_size: None,
        allow_parameter_override: true,
        // JSONB sub-object columns
        capabilities_full: Some(json!({
            "vision": { "supported_mime_types": ["image/png"] },
            "reasoning": { "toggle": false, "resume": false, "budget": false },
            "response_schema": true,
            "file_input": { "enabled": false, "supported_mime_types": [] },
            "image_generation": { "enabled": false, "supported_mime_types": [] },
            "audio_input": { "enabled": false, "supported_mime_types": [] },
            "audio_output": { "enabled": false, "supported_mime_types": [] },
            "code_interpreter": false,
            "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
        })),
        default_parameters: Some(json!({
            "temperature": null, "top_p": null, "max_output_tokens": null,
            "max_tool_calls": null, "presence_penalty": null,
            "frequency_penalty": null, "top_logprobs": null,
            "truncation": null, "service_tier": null,
            "parallel_tool_calls": null, "text": null, "reasoning": null,
            "tool_choice": null, "store": null
        })),
        additional_info: Some(json!({})),
        disabled_capabilities_full: Some(json!({
            "vision": { "disabled": false, "disabled_mime_types": [] },
            "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
            "function_calling": false,
            "response_schema": false,
            "streaming": false,
            "file_input": { "disabled": false, "disabled_mime_types": [] },
            "image_generation": { "disabled": false, "disabled_mime_types": [] },
            "audio_input": { "disabled": false, "disabled_mime_types": [] },
            "audio_output": { "disabled": false, "disabled_mime_types": [] },
            "code_interpreter": false,
            "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
        })),
        allow_extra_params: Some(json!(["custom_param"])),
        // OData-filterable columns
        gts_type: Some("gts.cf.genai.model.info.v1~cf.genai._.openai.v1~".to_owned()),
        vendor: Some("OpenAI".to_owned()),
        family: Some("gpt-4".to_owned()),
        managed: false,
        architecture: Some("transformer".to_owned()),
        format: Some("api-only".to_owned()),
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: Some("completion".to_owned()),
        approval_status: "approved".to_owned(),
        cap_vision: true,
        cap_function_calling: true,
        cap_streaming: true,
        cap_reasoning_effort: true,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Provider entity → SDK roundtrip
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn provider_entity_to_v1_active() {
    let entity = make_provider_entity(test_provider_id(), test_tenant_id(), "openai", "active");
    let v1 = provider_entity_to_v1(&entity);

    assert_eq!(v1.id, test_provider_id());
    assert_eq!(v1.slug, "openai");
    assert_eq!(v1.status, ProviderStatus::Active);
    assert_eq!(
        v1.gts_type.as_ref(),
        "gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~"
    );
    assert!(v1.metadata.is_some());
}

#[test]
fn provider_entity_to_v1_disabled() {
    let entity = make_provider_entity(test_provider_id(), test_tenant_id(), "old", "disabled");
    let v1 = provider_entity_to_v1(&entity);

    assert_eq!(v1.status, ProviderStatus::Disabled);
}

#[test]
fn provider_entity_to_v1_no_metadata() {
    let mut entity =
        make_provider_entity(test_provider_id(), test_tenant_id(), "no-meta", "active");
    entity.metadata = None;
    let v1 = provider_entity_to_v1(&entity);

    assert!(v1.metadata.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Provider create ActiveModel
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn provider_create_sets_fields() {
    let gts = gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
    let req = CreateProviderRequestV1::builder("openai", "OpenAI", gts)
        .managed(true)
        .metadata(json!({"k": "v"}))
        .discovery_enabled(true)
        .discovery_interval_seconds(3600)
        .build();

    let am = provider_create_active_model(test_tenant_id(), &req);

    assert_eq!(am.tenant_id.unwrap(), test_tenant_id());
    assert_eq!(am.slug.unwrap(), "openai");
    assert_eq!(am.name.unwrap(), "OpenAI");
    assert_eq!(am.status.unwrap(), "active");
    assert!(am.managed.unwrap());
    assert!(am.discovery_enabled.unwrap());
    assert_eq!(am.discovery_interval_seconds.unwrap(), Some(3600));
}

#[test]
fn provider_create_defaults() {
    let gts = gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.custom.v1~");
    let req = CreateProviderRequestV1::builder("custom", "Custom", gts).build();

    let am = provider_create_active_model(test_tenant_id(), &req);

    assert_eq!(am.status.unwrap(), "active");
    assert!(!am.managed.unwrap());
    assert!(am.metadata.unwrap().is_none());
    assert!(!am.discovery_enabled.unwrap());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Provider update ActiveModel
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn provider_update_only_some_fields() {
    let entity = make_provider_entity(test_provider_id(), test_tenant_id(), "openai", "active");
    let req = UpdateProviderRequestV1 {
        name: Some("Updated".into()),
        status: Some(ProviderStatus::Disabled),
        managed: None,
        metadata: None,
        discovery_enabled: None,
        discovery_interval_seconds: None,
    };

    let am = provider_update_active_model(&entity, &req);

    assert_eq!(am.name.unwrap(), "Updated");
    assert_eq!(am.status.unwrap(), "disabled");
    // Unchanged fields preserved
    assert!(!am.managed.unwrap());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model entity → SDK roundtrip (OpenAI)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_openai() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    let model: ModelV1 = model_entity_to_v1(&entity);

    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.lifecycle_status, LifecycleStatus::Production);
    assert_eq!(model.approval_status, ApprovalStatus::Approved);
    assert_eq!(model.info.display_name, "GPT-4o");
    assert_eq!(model.info.provider_model_id, "gpt-4o");
    assert_eq!(model.info.vendor.as_deref(), Some("OpenAI"));
    assert!(model.info.capabilities.vision.enabled);
    assert!(model.info.capabilities.function_calling);
    assert!(model.info.capabilities.reasoning.effort);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model entity → SDK roundtrip (Anthropic)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_anthropic() {
    let anthropic_settings = json!({
        "oagw_alias": "anthropic-prod",
        "anthropic_version": "2023-06-01",
        "max_tokens": 8192,
    });
    let mut info = make_info("cf.genai._.anthropic.v1~", &anthropic_settings);
    // Patch fields via direct struct field assignment.
    info.vendor = Some("Anthropic".to_owned());
    info.family = Some("claude".to_owned());
    info.provider_model_id = "claude-sonnet-4-20250514".to_owned();

    let info_json = serde_json::to_value(&info).expect("serialize");

    // Scalar columns are authoritative. Override the entity's
    // vendor/family/provider_model_id/gts_type scalars so they match the
    // anthropic info payload.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "anthropic::claude-sonnet-4-20250514",
        Some(info_json),
    );
    entity.vendor = Some("Anthropic".to_owned());
    entity.family = Some("claude".to_owned());
    entity.provider_model_id = Some("claude-sonnet-4-20250514".to_owned());
    entity.gts_type = Some("gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~".to_owned());

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.canonical_id, "anthropic::claude-sonnet-4-20250514");
    assert_eq!(model.info.vendor.as_deref(), Some("Anthropic"));
    assert_eq!(model.info.family.as_deref(), Some("claude"));
    assert_eq!(model.info.provider_model_id, "claude-sonnet-4-20250514");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model entity → SDK (unknown provider, raw JSON)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_unknown_provider() {
    let raw_settings = json!({"custom_endpoint": "https://custom.example.com"});
    let mut info = make_info("cf.genai._.custom.v1~", &raw_settings);
    // Patch fields via direct struct field assignment.
    info.vendor = Some("Custom".to_owned());
    info.provider_model_id = "custom-model".to_owned();

    let info_json = serde_json::to_value(&info).expect("serialize");
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "custom::custom-model",
        Some(info_json),
    );
    entity.provider_settings = Some(raw_settings);
    entity.vendor = Some("Custom".to_owned());
    entity.provider_model_id = Some("custom-model".to_owned());
    entity.gts_type = Some("gts.cf.genai.model.info.v1~cf.genai._.custom.v1~".to_owned());

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.canonical_id, "custom::custom-model");
    assert_eq!(
        model.info.provider_settings.get("custom_endpoint"),
        Some(&json!("https://custom.example.com"))
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model fallback when info JSONB is missing
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_fallback_when_info_null() {
    // `display_name` and `ctx_max_input_tokens` are required scalar columns
    // (with DB defaults), so the read path reconstructs a full `ModelInfoV1`
    // from the promoted columns — no fallback needed. Confirms that even with
    // default placeholder values the entity reconstructs without panic.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );
    entity.display_name = String::new();
    entity.ctx_max_input_tokens = 0;
    entity.gts_type = None;
    entity.provider_model_id = None;

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.info.display_name, "model-openai::gpt-4o");
    assert_eq!(model.info.vendor.as_deref(), Some("OpenAI"));
    assert!(model.info.capabilities.vision.enabled);
    assert!(model.info.capabilities.function_calling);
}

// ═══════════════════════════════════════════════════════════════════════════════
// `build_minimal_info` fallback path
// Triggered when required discriminator columns (`gts_type`, `provider_model_id`)
// are missing or corrupt. Verifies graceful degradation with DB defaults in
// place: `display_name` and `ctx_max_input_tokens` always populated (DB-level
// NOT NULL DEFAULTs); only the filterable columns are nullable.
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn build_minimal_info_triggered_when_gts_type_missing() {
    // Missing `gts_type` (the discriminator for `provider_settings` polymorphism)
    // triggers the fallback path. The fallback must use a default `gts_type`
    // string and reconstruct a valid `ModelInfoV1`.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );
    entity.gts_type = None;
    // All other columns have valid values from `make_model_entity`.

    let model = model_entity_to_v1(&entity);

    // `build_minimal_info` uses the default gts_type prefix as a fallback.
    assert_eq!(
        model.info.gts_type.to_string(),
        "gts.cf.genai.model.info.v1~"
    );
    // `display_name` was populated by the entity fixture — keep it (don't
    // synthesize a placeholder when the column has a real value).
    assert_eq!(model.info.display_name, "GPT-4o");
    // Required scalar columns are projected.
    assert_eq!(model.info.context_window.max_input_tokens, 128_000);
    assert!(model.info.allow_parameter_override);
}

#[test]
fn build_minimal_info_triggered_when_provider_model_id_missing() {
    // Missing `provider_model_id` triggers the fallback path. The fallback
    // must use an empty `provider_model_id` string and still reconstruct a
    // valid `ModelInfoV1`.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );
    entity.provider_model_id = None;
    // Keep gts_type populated.

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.canonical_id, "openai::gpt-4o");
    // `display_name` was populated by the entity fixture.
    assert_eq!(model.info.display_name, "GPT-4o");
    // `provider_model_id` falls back to empty string in the JSON reconstruction.
    assert_eq!(model.info.provider_model_id, "");
}

#[test]
fn build_minimal_info_uses_synthesized_display_name_when_empty() {
    // When `display_name` is the empty string (DB DEFAULT ''), the fallback
    // synthesizes a placeholder from `canonical_id`. This should rarely
    // happen in practice (the application layer always sets a real name) but
    // the fallback must not panic.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );
    entity.display_name = String::new();
    entity.gts_type = None; // trigger fallback

    let model = model_entity_to_v1(&entity);

    // Synthesized from canonical_id.
    assert_eq!(model.info.display_name, "model-openai::gpt-4o");
}

#[test]
fn build_minimal_info_preserves_zero_ctx_max_input_tokens() {
    // When `ctx_max_input_tokens` is 0 (DB DEFAULT 0), the fallback must
    // emit it as 0 on the wire — NOT silently substitute a non-zero value.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );
    entity.ctx_max_input_tokens = 0;
    entity.gts_type = None; // trigger fallback

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.info.context_window.max_input_tokens, 0);
    assert_eq!(model.info.context_window.max_output_tokens, Some(16_384));
}

#[test]
fn build_minimal_info_preserves_scalar_capability_columns() {
    // The scalar capability booleans live in dedicated columns and are NOT
    // computed from JSONB — they must be projected as-is even in the fallback
    // path.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );
    // Flip the booleans to distinct values so each is independently verifiable.
    entity.cap_vision = false;
    entity.cap_function_calling = true;
    entity.cap_streaming = false;
    entity.cap_reasoning_effort = true;
    entity.gts_type = None; // trigger fallback

    let model = model_entity_to_v1(&entity);

    assert!(!model.info.capabilities.vision.enabled);
    assert!(model.info.capabilities.function_calling);
    assert!(!model.info.capabilities.streaming);
    assert!(model.info.capabilities.reasoning.effort);
}

#[test]
fn build_minimal_info_projects_all_promoted_scalar_columns() {
    // Comprehensive test: when the fallback triggers, every promoted scalar
    // column should still be projected onto the wire.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );
    entity.description = Some("Some description".to_owned());
    entity.region = Some("eu-west-1".to_owned());
    entity.hosted_by = Some("Azure".to_owned());
    entity.reasoning_level = Some("medium".to_owned());
    entity.version = Some("2.0".to_owned());
    entity.sort_order = Some(42);
    entity.icon = Some("https://example.com/icon.png".to_owned());
    entity.multiplier_display = Some("2x".to_owned());
    entity.perf_response_latency_ms = Some(250);
    entity.perf_tokens_per_second = Some(200);
    entity.ctx_max_input_tokens = 200_000;
    entity.ctx_max_output_tokens = Some(8_192);
    entity.ctx_output_vector_size = Some(1536);
    entity.allow_parameter_override = false;
    entity.gts_type = None; // trigger fallback

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.info.description.as_deref(), Some("Some description"));
    assert_eq!(model.info.region.as_deref(), Some("eu-west-1"));
    assert_eq!(model.info.hosted_by.as_deref(), Some("Azure"));
    assert_eq!(model.info.reasoning_level.as_deref(), Some("medium"));
    assert_eq!(model.info.version.as_deref(), Some("2.0"));
    assert_eq!(model.info.sort_order, Some(42));
    assert_eq!(
        model.info.icon.as_deref(),
        Some("https://example.com/icon.png")
    );
    assert_eq!(model.info.multiplier_display.as_deref(), Some("2x"));
    assert_eq!(model.info.performance.response_latency_ms, Some(250));
    assert_eq!(model.info.performance.tokens_per_second, Some(200));
    assert_eq!(model.info.context_window.max_input_tokens, 200_000);
    assert_eq!(model.info.context_window.max_output_tokens, Some(8_192));
    assert_eq!(model.info.context_window.output_vector_size, Some(1536));
    assert!(!model.info.allow_parameter_override);
}

#[test]
fn build_minimal_info_does_not_trigger_when_required_fields_present() {
    // Sanity check: when `gts_type` and `provider_model_id` are both present,
    // the regular read path (not the fallback) is used. The regular path
    // projects the values from `display_name` (a real value) and uses the
    // rich JSONB columns for capabilities.
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );
    // All scalar columns populated by `make_model_entity`; both discriminators
    // are present (no need to set None here).

    let model = model_entity_to_v1(&entity);

    // Regular path: full capabilities come from the JSONB-rich read path.
    assert_eq!(
        model.info.gts_type.to_string(),
        "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~"
    );
    assert_eq!(model.info.display_name, "GPT-4o");
    // Vision mime types come from `capabilities_full` JSONB (full read path).
    assert_eq!(
        model.info.capabilities.vision.supported_mime_types,
        vec!["image/png".to_owned()]
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// OData-filterable columns after create
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_create_projects_filterable_columns() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let req = CreateModelRequestV1 {
        provider_slug: "openai".into(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: Some(ApprovalStatus::Approved),
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Approved,
    );

    assert_eq!(am.canonical_id.unwrap(), "openai::gpt-4o");
    assert_eq!(am.lifecycle_status.unwrap(), "production");
    assert_eq!(am.approval_status.unwrap(), "approved");
    assert_eq!(
        am.gts_type.unwrap(),
        Some(OpenAiSettingsV1::TYPE_ID.to_owned())
    );
    assert_eq!(am.vendor.unwrap(), Some("OpenAI".to_owned()));
    assert_eq!(am.family.unwrap(), Some("gpt-4".to_owned()));
    assert!(!am.managed.unwrap());
    assert_eq!(am.provider_model_id.unwrap(), Some("gpt-4o".to_owned()));
    assert_eq!(am.supported_api.unwrap(), Some("completion".to_owned()));
    assert!(am.cap_vision.unwrap());
    assert!(am.cap_function_calling.unwrap());
    assert!(am.cap_streaming.unwrap());
    assert!(am.cap_reasoning_effort.unwrap());
    // Spot-check one of the JSONB sub-object columns.
    assert!(am.capabilities_full.unwrap().is_some());
}

#[test]
fn model_create_default_pending_approval() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let req = CreateModelRequestV1 {
        provider_slug: "openai".into(),
        lifecycle_status: LifecycleStatus::Preview,
        approval_status: None,
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Pending,
    );

    assert_eq!(am.approval_status.unwrap(), "pending");
    assert_eq!(am.lifecycle_status.unwrap(), "preview");
}

// ═══════════════════════════════════════════════════════════════════════════════
// OData-filterable columns re-projected after PATCH
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_update_reprojects_filterable_columns() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    let req = UpdateModelRequestV1 {
        vendor: Some(Some("OpenAI-Updated".into())),
        family: Some(Some("gpt-4.1".into())),
        managed: Some(true),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req);

    assert_eq!(am.vendor.unwrap(), Some("OpenAI-Updated".into()));
    assert_eq!(am.family.unwrap(), Some("gpt-4.1".into()));
    assert!(am.managed.unwrap());
    // Unchanged
    assert_eq!(am.provider_model_id.unwrap(), Some("gpt-4o".into()));
    assert!(am.cap_function_calling.unwrap());
    assert!(am.cap_streaming.unwrap());
}

#[test]
fn model_update_no_changes_preserves_columns() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    let req = UpdateModelRequestV1::default();
    let am = model_update_active_model(&entity, &req);

    assert_eq!(am.vendor.unwrap(), Some("OpenAI".into()));
    assert_eq!(am.family.unwrap(), Some("gpt-4".into()));
    assert!(!am.managed.unwrap());
    assert!(am.cap_vision.unwrap());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Immutability / malformed JSONB
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_handles_malformed_jsonb() {
    // When the polymorphic `provider_settings` JSONB is missing, the read path
    // returns null on the wire.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );
    entity.provider_settings = None;

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.info.display_name, "GPT-4o");
    assert!(
        model.info.provider_settings.is_null(),
        "missing provider_settings JSONB must surface as null on the wire"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Approval and lifecycle changes via update
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_update_applies_approval_status() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    let req = UpdateModelRequestV1 {
        approval_status: Some(ApprovalStatus::Rejected),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req);

    // approval_status is patched in place by the mapper — assert the new value
    // is applied.
    assert_eq!(am.approval_status.unwrap(), "rejected");
}

#[test]
fn model_update_lifecycle_status() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    let req = UpdateModelRequestV1 {
        lifecycle_status: Some(LifecycleStatus::Deprecated),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req);

    assert_eq!(am.lifecycle_status.unwrap(), "deprecated");
}

// ═══════════════════════════════════════════════════════════════════════════════
// provider_settings roundtrip
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_preserves_provider_settings() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    let model = model_entity_to_v1(&entity);

    let ps = &model.info.provider_settings;
    assert_eq!(ps.get("oagw_alias"), Some(&json!("openai-prod")));
    assert_eq!(ps.get("temperature"), Some(&json!(0.7)));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Read-path round-trip with every promoted column populated
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
#[allow(clippy::cognitive_complexity)]
fn model_entity_to_v1_round_trips_all_promoted_columns() {
    // Build an entity with every promoted column populated, then read it back
    // and verify every field lands on the SDK `ModelInfoV1` correctly.
    let now = Utc::now();
    let entity = entity::model::Model {
        id: test_model_id(),
        provider_id: test_provider_id(),
        tenant_id: test_tenant_id(),
        canonical_id: "openai::gpt-4o".to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: Some(json!({"oagw_alias": "openai-prod", "temperature": 0.7})),
        created_at: now,
        updated_at: now,
        // Promoted scalar columns
        display_name: "GPT-4o".to_owned(),
        description: Some("OpenAI's flagship model".to_owned()),
        size_bytes: Some(0),
        region: Some("us-east-1".to_owned()),
        hosted_by: Some("OpenAI".to_owned()),
        last_release_at: None,
        reasoning_level: Some("high".to_owned()),
        version: Some("1.0".to_owned()),
        sort_order: Some(10),
        icon: Some("https://example.com/gpt-4o.png".to_owned()),
        multiplier_display: Some("1x".to_owned()),
        perf_response_latency_ms: Some(500),
        perf_tokens_per_second: Some(100),
        ctx_max_input_tokens: 128_000,
        ctx_max_output_tokens: Some(16_384),
        ctx_output_vector_size: None,
        allow_parameter_override: true,
        // JSONB sub-object columns
        capabilities_full: Some(json!({
            "vision": { "supported_mime_types": ["image/png", "image/jpeg"] },
            "reasoning": { "toggle": false, "resume": false, "budget": false },
            "response_schema": true,
            "file_input": { "enabled": false, "supported_mime_types": [] },
            "image_generation": { "enabled": false, "supported_mime_types": [] },
            "audio_input": { "enabled": false, "supported_mime_types": [] },
            "audio_output": { "enabled": false, "supported_mime_types": [] },
            "code_interpreter": false,
            "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
        })),
        default_parameters: Some(json!({
            "temperature": 0.7,
            "top_p": null,
            "max_output_tokens": null
        })),
        additional_info: Some(json!({"region": "us-east", "internal_owner": "team-a"})),
        disabled_capabilities_full: Some(json!({
            "vision": { "disabled": false, "disabled_mime_types": [] },
            "function_calling": false,
            "streaming": false
        })),
        allow_extra_params: Some(json!(["custom_param", "trace_id"])),
        // OData-filterable columns
        gts_type: Some("gts.cf.genai.model.info.v1~cf.genai._.openai.v1~".to_owned()),
        vendor: Some("OpenAI".to_owned()),
        family: Some("gpt-4".to_owned()),
        managed: false,
        architecture: Some("transformer".to_owned()),
        format: Some("api-only".to_owned()),
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: Some("completion".to_owned()),
        approval_status: "approved".to_owned(),
        cap_vision: true,
        cap_function_calling: true,
        cap_streaming: true,
        cap_reasoning_effort: true,
    };

    let model = model_entity_to_v1(&entity);

    // Scalar field round-trip
    assert_eq!(model.info.display_name, "GPT-4o");
    assert_eq!(
        model.info.description.as_deref(),
        Some("OpenAI's flagship model")
    );
    assert_eq!(model.info.region.as_deref(), Some("us-east-1"));
    assert_eq!(model.info.hosted_by.as_deref(), Some("OpenAI"));
    assert_eq!(model.info.reasoning_level.as_deref(), Some("high"));
    assert_eq!(model.info.version.as_deref(), Some("1.0"));
    assert_eq!(model.info.sort_order, Some(10));
    assert_eq!(
        model.info.icon.as_deref(),
        Some("https://example.com/gpt-4o.png")
    );
    assert_eq!(model.info.multiplier_display.as_deref(), Some("1x"));
    assert_eq!(model.info.performance.response_latency_ms, Some(500));
    assert_eq!(model.info.performance.tokens_per_second, Some(100));
    assert_eq!(model.info.context_window.max_input_tokens, 128_000);
    assert_eq!(model.info.context_window.max_output_tokens, Some(16_384));
    assert!(model.info.allow_parameter_override);

    // JSONB sub-object round-trip
    assert!(
        model.info.capabilities.vision.enabled,
        "scalar `cap_vision` must override JSONB"
    );
    assert_eq!(
        model.info.capabilities.vision.supported_mime_types,
        vec!["image/png".to_owned(), "image/jpeg".to_owned()],
        "vision mime types must come from JSONB"
    );
    assert_eq!(model.info.default_parameters.temperature, Some(0.7));
    assert_eq!(
        model.info.additional_info.get("internal_owner"),
        Some(&json!("team-a"))
    );
    assert_eq!(
        model.info.allow_extra_params,
        vec!["custom_param", "trace_id"]
    );

    // Provider settings unchanged
    assert_eq!(
        model.info.provider_settings.get("oagw_alias"),
        Some(&json!("openai-prod"))
    );

    // Wire-level fields
    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.approval_status, ApprovalStatus::Approved);
}

#[test]
fn model_entity_to_v1_default_db_values_reconstruct_without_panic() {
    // Simulate a row that was inserted via raw SQL bypassing the application
    // layer — only the migration defaults are populated. The read path must
    // still produce a valid `ModelInfoV1` without panicking (graceful
    // degradation via `build_minimal_info`).
    let now = Utc::now();
    let entity = entity::model::Model {
        id: test_model_id(),
        provider_id: test_provider_id(),
        tenant_id: test_tenant_id(),
        canonical_id: "openai::gpt-4o".to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: None,
        created_at: now,
        updated_at: now,
        // DB DEFAULT values
        display_name: String::new(), // DEFAULT ''
        description: None,
        size_bytes: None,
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: None,
        sort_order: None,
        icon: None,
        multiplier_display: None,
        perf_response_latency_ms: None,
        perf_tokens_per_second: None,
        ctx_max_input_tokens: 0, // DEFAULT 0
        ctx_max_output_tokens: None,
        ctx_output_vector_size: None,
        allow_parameter_override: false, // DEFAULT 0
        capabilities_full: None,
        default_parameters: None,
        additional_info: None,
        disabled_capabilities_full: None,
        allow_extra_params: None,
        gts_type: None, // discriminator missing — trigger fallback
        vendor: Some("OpenAI".to_owned()),
        family: None,
        managed: false,
        architecture: None,
        format: None,
        provider_model_id: None, // also missing — triggers fallback
        supported_api: None,
        approval_status: "pending".to_owned(),
        cap_vision: false,
        cap_function_calling: false,
        cap_streaming: false,
        cap_reasoning_effort: false,
    };

    let model = model_entity_to_v1(&entity);

    // `build_minimal_info` should kick in because `gts_type` is None.
    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.info.display_name, "model-openai::gpt-4o");
    assert_eq!(model.info.vendor.as_deref(), Some("OpenAI"));
}

#[test]
fn model_entity_to_v1_handles_null_jsonb_sub_objects() {
    // Verify each JSONB sub-object column can be NULL without
    // breaking the read path (defaulting to empty / null shapes).
    let now = Utc::now();
    let entity = entity::model::Model {
        id: test_model_id(),
        provider_id: test_provider_id(),
        tenant_id: test_tenant_id(),
        canonical_id: "openai::gpt-4o".to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: None,
        created_at: now,
        updated_at: now,
        display_name: "GPT-4o".to_owned(),
        description: None,
        size_bytes: None,
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: None,
        sort_order: None,
        icon: None,
        multiplier_display: None,
        perf_response_latency_ms: None,
        perf_tokens_per_second: None,
        ctx_max_input_tokens: 8192,
        ctx_max_output_tokens: None,
        ctx_output_vector_size: None,
        allow_parameter_override: false,
        // ALL JSONB sub-objects NULL
        capabilities_full: None,
        default_parameters: None,
        additional_info: None,
        disabled_capabilities_full: None,
        allow_extra_params: None,
        gts_type: Some("gts.cf.genai.model.info.v1~cf.genai._.openai.v1~".to_owned()),
        vendor: Some("OpenAI".to_owned()),
        family: None,
        managed: false,
        architecture: None,
        format: None,
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: Some("completion".to_owned()),
        approval_status: "approved".to_owned(),
        cap_vision: true,
        cap_function_calling: true,
        cap_streaming: true,
        cap_reasoning_effort: false,
    };

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.info.display_name, "GPT-4o");
    assert_eq!(model.info.context_window.max_input_tokens, 8192);
    // Defaults are reasonable — additional_info is empty HashMap,
    // allow_extra_params is empty Vec, capabilities come from scalar bools.
    assert!(model.info.capabilities.vision.enabled);
    assert!(model.info.capabilities.function_calling);
    assert!(model.info.capabilities.streaming);
    assert!(!model.info.capabilities.reasoning.effort);
    assert!(model.info.allow_extra_params.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════════
// build_capabilities merge logic
// (scalar bools override JSONB content; JSONB-only fields preserved)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn build_capabilities_scalar_vision_overrides_jsonb() {
    // JSONB says vision.enabled = true, scalar cap_vision = false → scalar wins.
    let now = Utc::now();
    let entity = entity::model::Model {
        id: test_model_id(),
        provider_id: test_provider_id(),
        tenant_id: test_tenant_id(),
        canonical_id: "openai::gpt-4o".to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: None,
        created_at: now,
        updated_at: now,
        display_name: "GPT-4o".to_owned(),
        description: None,
        size_bytes: None,
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: None,
        sort_order: None,
        icon: None,
        multiplier_display: None,
        perf_response_latency_ms: None,
        perf_tokens_per_second: None,
        ctx_max_input_tokens: 8192,
        ctx_max_output_tokens: None,
        ctx_output_vector_size: None,
        allow_parameter_override: false,
        capabilities_full: Some(json!({
            "vision": { "enabled": true, "supported_mime_types": ["image/png"] },
        })),
        default_parameters: None,
        additional_info: None,
        disabled_capabilities_full: None,
        allow_extra_params: None,
        gts_type: Some("gts.cf.genai.model.info.v1~".to_owned()),
        vendor: None,
        family: None,
        managed: false,
        architecture: None,
        format: None,
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: None,
        approval_status: "pending".to_owned(),
        cap_vision: false, // scalar overrides JSONB
        cap_function_calling: false,
        cap_streaming: false,
        cap_reasoning_effort: false,
    };
    let model = model_entity_to_v1(&entity);
    assert!(
        !model.info.capabilities.vision.enabled,
        "scalar cap_vision must override JSONB vision.enabled"
    );
    // But mime types come from JSONB
    assert_eq!(
        model.info.capabilities.vision.supported_mime_types,
        vec!["image/png".to_owned()]
    );
}

#[test]
fn build_capabilities_scalar_function_calling_overrides_jsonb() {
    let now = Utc::now();
    let entity = entity::model::Model {
        id: test_model_id(),
        provider_id: test_provider_id(),
        tenant_id: test_tenant_id(),
        canonical_id: "openai::gpt-4o".to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: None,
        created_at: now,
        updated_at: now,
        display_name: "GPT-4o".to_owned(),
        description: None,
        size_bytes: None,
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: None,
        sort_order: None,
        icon: None,
        multiplier_display: None,
        perf_response_latency_ms: None,
        perf_tokens_per_second: None,
        ctx_max_input_tokens: 8192,
        ctx_max_output_tokens: None,
        ctx_output_vector_size: None,
        allow_parameter_override: false,
        capabilities_full: Some(json!({
            "function_calling": true, // JSONB says true
        })),
        default_parameters: None,
        additional_info: None,
        disabled_capabilities_full: None,
        allow_extra_params: None,
        gts_type: Some("gts.cf.genai.model.info.v1~".to_owned()),
        vendor: None,
        family: None,
        managed: false,
        architecture: None,
        format: None,
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: None,
        approval_status: "pending".to_owned(),
        cap_vision: false,
        cap_function_calling: false, // scalar overrides JSONB
        cap_streaming: false,
        cap_reasoning_effort: false,
    };
    let model = model_entity_to_v1(&entity);
    assert!(
        !model.info.capabilities.function_calling,
        "scalar cap_function_calling must override JSONB"
    );
}

#[test]
fn build_capabilities_scalar_streaming_overrides_jsonb() {
    let now = Utc::now();
    let entity = entity::model::Model {
        id: test_model_id(),
        provider_id: test_provider_id(),
        tenant_id: test_tenant_id(),
        canonical_id: "openai::gpt-4o".to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: None,
        created_at: now,
        updated_at: now,
        display_name: "GPT-4o".to_owned(),
        description: None,
        size_bytes: None,
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: None,
        sort_order: None,
        icon: None,
        multiplier_display: None,
        perf_response_latency_ms: None,
        perf_tokens_per_second: None,
        ctx_max_input_tokens: 8192,
        ctx_max_output_tokens: None,
        ctx_output_vector_size: None,
        allow_parameter_override: false,
        capabilities_full: Some(json!({
            "streaming": true, // JSONB says true
        })),
        default_parameters: None,
        additional_info: None,
        disabled_capabilities_full: None,
        allow_extra_params: None,
        gts_type: Some("gts.cf.genai.model.info.v1~".to_owned()),
        vendor: None,
        family: None,
        managed: false,
        architecture: None,
        format: None,
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: None,
        approval_status: "pending".to_owned(),
        cap_vision: false,
        cap_function_calling: false,
        cap_streaming: false, // scalar overrides JSONB
        cap_reasoning_effort: false,
    };
    let model = model_entity_to_v1(&entity);
    assert!(
        !model.info.capabilities.streaming,
        "scalar cap_streaming must override JSONB"
    );
}

#[test]
fn build_capabilities_scalar_reasoning_effort_overrides_jsonb() {
    let now = Utc::now();
    let entity = entity::model::Model {
        id: test_model_id(),
        provider_id: test_provider_id(),
        tenant_id: test_tenant_id(),
        canonical_id: "openai::gpt-4o".to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: None,
        created_at: now,
        updated_at: now,
        display_name: "GPT-4o".to_owned(),
        description: None,
        size_bytes: None,
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: None,
        sort_order: None,
        icon: None,
        multiplier_display: None,
        perf_response_latency_ms: None,
        perf_tokens_per_second: None,
        ctx_max_input_tokens: 8192,
        ctx_max_output_tokens: None,
        ctx_output_vector_size: None,
        allow_parameter_override: false,
        capabilities_full: Some(json!({
            "reasoning": { "effort": true, "toggle": true, "resume": false, "budget": true }
        })),
        default_parameters: None,
        additional_info: None,
        disabled_capabilities_full: None,
        allow_extra_params: None,
        gts_type: Some("gts.cf.genai.model.info.v1~".to_owned()),
        vendor: None,
        family: None,
        managed: false,
        architecture: None,
        format: None,
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: None,
        approval_status: "pending".to_owned(),
        cap_vision: false,
        cap_function_calling: false,
        cap_streaming: false,
        cap_reasoning_effort: false, // scalar overrides JSONB (true → false)
    };
    let model = model_entity_to_v1(&entity);
    assert!(
        !model.info.capabilities.reasoning.effort,
        "scalar cap_reasoning_effort must override JSONB"
    );
    // But toggle/resume/budget come from JSONB (not scalar columns)
    assert!(
        model.info.capabilities.reasoning.toggle,
        "reasoning.toggle must come from JSONB"
    );
    assert!(
        model.info.capabilities.reasoning.budget,
        "reasoning.budget must come from JSONB"
    );
}

#[test]
fn build_capabilities_preserves_jsonb_only_fields() {
    // Verify the non-promoted JSONB capability fields survive the merge.
    let now = Utc::now();
    let entity = entity::model::Model {
        id: test_model_id(),
        provider_id: test_provider_id(),
        tenant_id: test_tenant_id(),
        canonical_id: "openai::gpt-4o".to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        provider_settings: None,
        created_at: now,
        updated_at: now,
        display_name: "GPT-4o".to_owned(),
        description: None,
        size_bytes: None,
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: None,
        sort_order: None,
        icon: None,
        multiplier_display: None,
        perf_response_latency_ms: None,
        perf_tokens_per_second: None,
        ctx_max_input_tokens: 8192,
        ctx_max_output_tokens: None,
        ctx_output_vector_size: None,
        allow_parameter_override: false,
        capabilities_full: Some(json!({
            "response_schema": true,
            "file_input": { "enabled": true, "supported_mime_types": ["application/pdf"] },
            "image_generation": { "enabled": false, "supported_mime_types": [] },
            "audio_input": { "enabled": true, "supported_mime_types": ["audio/mp3"] },
            "audio_output": { "enabled": false, "supported_mime_types": [] },
            "code_interpreter": true,
            "web_search": { "enabled": true, "allowed_domains": false, "excluded_domains": true }
        })),
        default_parameters: None,
        additional_info: None,
        disabled_capabilities_full: None,
        allow_extra_params: None,
        gts_type: Some("gts.cf.genai.model.info.v1~".to_owned()),
        vendor: None,
        family: None,
        managed: false,
        architecture: None,
        format: None,
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: None,
        approval_status: "pending".to_owned(),
        cap_vision: false,
        cap_function_calling: false,
        cap_streaming: false,
        cap_reasoning_effort: false,
    };
    let model = model_entity_to_v1(&entity);

    assert!(model.info.capabilities.response_schema);
    assert!(model.info.capabilities.file_input.enabled);
    assert_eq!(
        model.info.capabilities.file_input.supported_mime_types,
        vec!["application/pdf".to_owned()]
    );
    assert!(model.info.capabilities.audio_input.enabled);
    assert_eq!(
        model.info.capabilities.audio_input.supported_mime_types,
        vec!["audio/mp3".to_owned()]
    );
    assert!(model.info.capabilities.code_interpreter);
    assert!(model.info.capabilities.web_search.enabled);
    assert!(!model.info.capabilities.web_search.allowed_domains);
    assert!(model.info.capabilities.web_search.excluded_domains);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Write-path tests for `model_create_active_model`
// Verifies every promoted column (scalar and JSONB sub-object) is set
// correctly from a fully-populated `ModelInfoV1`.
// ═══════════════════════════════════════════════════════════════════════════════

/// Build a fully-populated `CreateModelRequestV1` for write-path tests.
fn make_create_request(gts_leaf: &str) -> CreateModelRequestV1 {
    let info = make_info(gts_leaf, &openai_settings());
    CreateModelRequestV1 {
        provider_slug: "openai".into(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: Some(ApprovalStatus::Approved),
        info,
    }
}

#[test]
fn model_create_active_model_sets_all_scalar_columns() {
    // Build a request with every scalar field populated, then assert that the
    // corresponding ActiveModel column values match the input.
    let mut info = make_info("cf.genai._.openai.v1~", &openai_settings());
    // Patch in distinct values for every promoted scalar column so the
    // assertions can confirm each one was projected.
    info.description = Some("A fully-populated test model".to_owned());
    info.size_bytes = Some(1_073_741_824); // 1 GiB
    info.region = Some("eu-west-1".to_owned());
    info.hosted_by = Some("Azure".to_owned());
    info.reasoning_level = Some("medium".to_owned());
    info.version = Some("2.5.0".to_owned());
    info.sort_order = Some(42);
    info.icon = Some("https://example.com/icon.png".to_owned());
    info.multiplier_display = Some("2.5x".to_owned());
    info.performance = ModelPerformance {
        response_latency_ms: Some(250),
        tokens_per_second: Some(200),
    };
    info.context_window = ContextWindow {
        max_input_tokens: 200_000,
        max_output_tokens: Some(32_768),
        output_vector_size: Some(1536),
    };
    info.allow_parameter_override = false;

    let req = CreateModelRequestV1 {
        provider_slug: "openai".into(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: Some(ApprovalStatus::Approved),
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Approved,
    );

    // Promoted scalar columns — assert each one matches the input.
    assert_eq!(am.display_name.unwrap(), "GPT-4o");
    assert_eq!(
        am.description.unwrap(),
        Some("A fully-populated test model".to_owned())
    );
    assert_eq!(am.size_bytes.unwrap(), Some(1_073_741_824_i64));
    assert_eq!(am.region.unwrap(), Some("eu-west-1".to_owned()));
    assert_eq!(am.hosted_by.unwrap(), Some("Azure".to_owned()));
    assert!(am.last_release_at.unwrap().is_none());
    assert_eq!(am.reasoning_level.unwrap(), Some("medium".to_owned()));
    assert_eq!(am.version.unwrap(), Some("2.5.0".to_owned()));
    assert_eq!(am.sort_order.unwrap(), Some(42));
    assert_eq!(
        am.icon.unwrap(),
        Some("https://example.com/icon.png".to_owned())
    );
    assert_eq!(am.multiplier_display.unwrap(), Some("2.5x".to_owned()));
    assert_eq!(am.perf_response_latency_ms.unwrap(), Some(250));
    assert_eq!(am.perf_tokens_per_second.unwrap(), Some(200));
    assert_eq!(am.ctx_max_input_tokens.unwrap(), 200_000);
    assert_eq!(am.ctx_max_output_tokens.unwrap(), Some(32_768));
    assert_eq!(am.ctx_output_vector_size.unwrap(), Some(1536));
    assert!(!am.allow_parameter_override.unwrap());
}

#[test]
fn model_create_active_model_extracts_provider_settings() {
    let settings = openai_settings();
    let req = make_create_request("cf.genai._.openai.v1~");

    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Approved,
    );

    let ps = am
        .provider_settings
        .unwrap()
        .expect("provider_settings set");
    assert_eq!(ps.get("oagw_alias"), Some(&json!("openai-prod")));
    assert_eq!(ps.get("temperature"), Some(&json!(0.7)));
    // Verify it matches the input settings shape.
    assert_eq!(ps.get("endpoint_kind"), settings.get("endpoint_kind"));
}

#[test]
fn model_create_active_model_handles_null_provider_settings() {
    // When `provider_settings` is `null` in the input, the column is stored as None.
    let info = make_info("cf.genai._.openai.v1~", &serde_json::Value::Null);
    let req = CreateModelRequestV1 {
        provider_slug: "openai".into(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Pending,
    );

    assert!(am.provider_settings.unwrap().is_none());
}

#[test]
fn model_create_active_model_capabilities_strips_promoted_booleans() {
    // The promoted booleans (vision.enabled, reasoning.effort, function_calling,
    // streaming) must be extracted from `capabilities_full` and stored as scalar
    // columns. The remaining capability content rides in the JSONB sub-object.
    let req = make_create_request("cf.genai._.openai.v1~");
    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Approved,
    );

    // Scalar columns get the promoted booleans.
    assert!(am.cap_vision.unwrap());
    assert!(am.cap_function_calling.unwrap());
    assert!(am.cap_streaming.unwrap());
    assert!(am.cap_reasoning_effort.unwrap());

    // JSONB `capabilities_full` must NOT contain the promoted booleans
    // (they are authoritative in the columns).
    let cap_full = am
        .capabilities_full
        .unwrap()
        .expect("capabilities_full set");
    let cap_obj = cap_full
        .as_object()
        .expect("capabilities_full is an object");

    // `function_calling` and `streaming` are removed at top level.
    assert!(
        !cap_obj.contains_key("function_calling"),
        "function_calling must be stripped from capabilities_full JSONB"
    );
    assert!(
        !cap_obj.contains_key("streaming"),
        "streaming must be stripped from capabilities_full JSONB"
    );

    // `vision.enabled` and `reasoning.effort` are removed from their nested objects.
    let vision = cap_obj
        .get("vision")
        .and_then(|v| v.as_object())
        .expect("vision present");
    assert!(
        !vision.contains_key("enabled"),
        "vision.enabled must be stripped from capabilities_full JSONB"
    );
    // vision.supported_mime_types preserved (it's not promoted).
    assert!(vision.contains_key("supported_mime_types"));

    let reasoning = cap_obj
        .get("reasoning")
        .and_then(|v| v.as_object())
        .expect("reasoning present");
    assert!(
        !reasoning.contains_key("effort"),
        "reasoning.effort must be stripped from capabilities_full JSONB"
    );
    // reasoning.toggle/resume/budget preserved.
    assert!(reasoning.contains_key("toggle"));
    assert!(reasoning.contains_key("resume"));
    assert!(reasoning.contains_key("budget"));

    // Non-promoted capability fields are preserved in JSONB.
    assert!(cap_obj.contains_key("response_schema"));
    assert!(cap_obj.contains_key("file_input"));
    assert!(cap_obj.contains_key("image_generation"));
    assert!(cap_obj.contains_key("audio_input"));
    assert!(cap_obj.contains_key("audio_output"));
    assert!(cap_obj.contains_key("code_interpreter"));
    assert!(cap_obj.contains_key("web_search"));
}

#[test]
fn model_create_active_model_stores_disabled_capabilities_fully() {
    // `disabled_capabilities_full` is NOT promoted — the entire DisabledCapabilities
    // structure rides in this single JSONB column.
    let req = make_create_request("cf.genai._.openai.v1~");
    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Approved,
    );

    let disabled_full = am
        .disabled_capabilities_full
        .unwrap()
        .expect("disabled_capabilities_full set");
    let obj = disabled_full.as_object().expect("object");
    assert!(obj.contains_key("vision"));
    assert!(obj.contains_key("reasoning"));
    assert!(obj.contains_key("function_calling"));
    assert!(obj.contains_key("response_schema"));
    assert!(obj.contains_key("streaming"));
    assert!(obj.contains_key("file_input"));
    assert!(obj.contains_key("audio_input"));
    assert!(obj.contains_key("audio_output"));
    assert!(obj.contains_key("code_interpreter"));
    assert!(obj.contains_key("web_search"));
}

#[test]
fn model_create_active_model_additional_info_round_trip() {
    // `additional_info` is a HashMap<String, Value> that rides in its own JSONB
    // sub-object column. Verify it survives the write path intact.
    let mut info = make_info("cf.genai._.openai.v1~", &openai_settings());
    info.additional_info = HashMap::from([
        (
            "internal_owner".to_owned(),
            serde_json::Value::String("team-a".to_owned()),
        ),
        (
            "billing_code".to_owned(),
            serde_json::Value::String("AI-12345".to_owned()),
        ),
        ("experiment_flag".to_owned(), serde_json::Value::Bool(true)),
        ("priority".to_owned(), serde_json::Value::Number(7.into())),
    ]);

    let req = CreateModelRequestV1 {
        provider_slug: "openai".into(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Pending,
    );

    let ai = am.additional_info.unwrap().expect("additional_info set");
    assert_eq!(ai.get("internal_owner"), Some(&json!("team-a")));
    assert_eq!(ai.get("billing_code"), Some(&json!("AI-12345")));
    assert_eq!(ai.get("experiment_flag"), Some(&json!(true)));
    assert_eq!(ai.get("priority"), Some(&json!(7)));
}

#[test]
fn model_create_active_model_default_parameters_round_trip() {
    // DefaultInferenceParametersV1 fields use `skip_serializing_if =
    // Option::is_none`, so `serde_json::to_value(...)` only emits the populated
    // fields. Verify that the JSONB sub-object column carries those populated
    // values intact.
    let mut info = make_info("cf.genai._.openai.v1~", &openai_settings());
    // Patch in distinct values for the populated default_parameters fields.
    info.default_parameters = DefaultInferenceParametersV1 {
        temperature: Some(0.5),
        top_p: Some(0.9),
        max_output_tokens: Some(2048),
        ..DefaultInferenceParametersV1::default()
    };

    let req = CreateModelRequestV1 {
        provider_slug: "openai".into(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    };
    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Pending,
    );

    let dp = am
        .default_parameters
        .unwrap()
        .expect("default_parameters set");
    let obj = dp.as_object().expect("object");
    // Populated fields survive the round-trip.
    assert_eq!(obj.get("temperature"), Some(&json!(0.5)));
    assert_eq!(obj.get("top_p"), Some(&json!(0.9)));
    assert_eq!(obj.get("max_output_tokens"), Some(&json!(2048)));
}

#[test]
fn model_create_active_model_allow_extra_params_round_trip() {
    let req = make_create_request("cf.genai._.openai.v1~");
    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Approved,
    );

    let aep = am
        .allow_extra_params
        .unwrap()
        .expect("allow_extra_params set");
    let arr = aep.as_array().expect("array");
    assert_eq!(arr, &vec![json!("custom_param")]);
}

#[test]
fn model_create_active_model_sets_canonical_id_format() {
    let req = make_create_request("cf.genai._.openai.v1~");
    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Approved,
    );

    // canonical_id = {provider_slug}::{provider_model_id}
    assert_eq!(am.canonical_id.unwrap(), "openai::gpt-4o");
}

#[test]
fn model_create_active_model_sets_supported_api_csv() {
    // `supported_api` is serialized as a comma-separated string. Verify it is
    // sorted (deterministic) and lowercase.
    let req = make_create_request("cf.genai._.openai.v1~");
    let am = model_create_active_model(
        test_tenant_id(),
        test_provider_id(),
        &req,
        ApprovalStatus::Approved,
    );

    let sa = am.supported_api.unwrap().expect("supported_api set");
    assert_eq!(sa, "completion");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Write-path tests for `model_update_active_model`
// Verifies PATCH semantics: a single field change re-projects every column.
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
#[allow(clippy::cognitive_complexity)]
fn model_update_patches_single_field_reprojects_all_columns() {
    // Start from a fully-populated entity, PATCH a single scalar field
    // (description), and confirm every promoted column is re-projected
    // (preserved where unchanged, updated where patched).
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    let req = UpdateModelRequestV1 {
        description: Some(Some("Updated description".to_owned())),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req);

    // Patched field — description was None, now Some("Updated description").
    assert_eq!(
        am.description.unwrap(),
        Some("Updated description".to_owned())
    );

    // All other promoted scalar columns preserved (re-projected from
    // the reconstructed ModelInfoV1).
    assert_eq!(am.display_name.unwrap(), "GPT-4o");
    assert!(am.size_bytes.unwrap().is_none());
    assert_eq!(am.region.unwrap(), Some("us-east-1".to_owned()));
    assert_eq!(am.hosted_by.unwrap(), Some("OpenAI".to_owned()));
    assert_eq!(am.reasoning_level.unwrap(), Some("high".to_owned()));
    assert_eq!(am.version.unwrap(), Some("1.0".to_owned()));
    assert_eq!(am.sort_order.unwrap(), Some(10));
    assert!(am.icon.unwrap().is_none());
    assert_eq!(am.multiplier_display.unwrap(), Some("1x".to_owned()));
    assert_eq!(am.perf_response_latency_ms.unwrap(), Some(500));
    assert_eq!(am.perf_tokens_per_second.unwrap(), Some(100));
    assert_eq!(am.ctx_max_input_tokens.unwrap(), 128_000);
    assert_eq!(am.ctx_max_output_tokens.unwrap(), Some(16_384));
    assert!(am.ctx_output_vector_size.unwrap().is_none());
    assert!(am.allow_parameter_override.unwrap());

    // JSONB sub-object columns preserved.
    assert!(am.capabilities_full.unwrap().is_some());
    assert!(am.default_parameters.unwrap().is_some());
    assert!(am.additional_info.unwrap().is_some());
    assert!(am.disabled_capabilities_full.unwrap().is_some());
    assert!(am.allow_extra_params.unwrap().is_some());

    // OData scalar capability booleans re-projected.
    assert!(am.cap_vision.unwrap());
    assert!(am.cap_function_calling.unwrap());
    assert!(am.cap_streaming.unwrap());
    assert!(am.cap_reasoning_effort.unwrap());
}

#[test]
fn model_update_patches_capability_reprojects_columns_and_jsonb() {
    // PATCH on `capabilities` (e.g. vision.enabled flips false→true) must
    // re-project BOTH the scalar capability columns AND `capabilities_full`
    // JSONB (which must still strip the promoted booleans).
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );
    // Force entity to start with vision=false so the patch has visible effect.
    entity.cap_vision = false;
    entity.cap_function_calling = false;
    entity.cap_streaming = false;
    entity.cap_reasoning_effort = false;

    // Build a PATCH that flips the capability bools.
    let req = UpdateModelRequestV1 {
        capabilities: Some(
            serde_json::from_value(json!({
                "vision": { "enabled": true, "supported_mime_types": ["image/jpeg"] },
                "reasoning": { "effort": true, "toggle": false, "resume": false, "budget": false },
                "function_calling": true,
                "response_schema": true,
                "streaming": true,
                "file_input": { "enabled": false, "supported_mime_types": [] },
                "image_generation": { "enabled": false, "supported_mime_types": [] },
                "audio_input": { "enabled": false, "supported_mime_types": [] },
                "audio_output": { "enabled": false, "supported_mime_types": [] },
                "code_interpreter": false,
                "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
            }))
            .unwrap(),
        ),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req);

    // Scalar booleans flipped.
    assert!(am.cap_vision.unwrap());
    assert!(am.cap_function_calling.unwrap());
    assert!(am.cap_streaming.unwrap());
    assert!(am.cap_reasoning_effort.unwrap());

    // JSONB `capabilities_full` re-projected with the promoted booleans stripped.
    let cap_full = am
        .capabilities_full
        .unwrap()
        .expect("capabilities_full set");
    let cap_obj = cap_full.as_object().expect("object");
    assert!(!cap_obj.contains_key("function_calling"));
    assert!(!cap_obj.contains_key("streaming"));
    let vision = cap_obj
        .get("vision")
        .and_then(|v| v.as_object())
        .expect("vision present");
    assert!(!vision.contains_key("enabled"));
    let reasoning = cap_obj
        .get("reasoning")
        .and_then(|v| v.as_object())
        .expect("reasoning present");
    assert!(!reasoning.contains_key("effort"));
}

#[test]
fn model_update_patches_provider_settings_reprojects_column() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    // PATCH provider_settings to a new value.
    let new_settings = json!({
        "oagw_alias": "openai-staging",
        "endpoint_kind": "responses",
    });
    let req = UpdateModelRequestV1 {
        provider_settings: Some(serde_json::from_value(new_settings).unwrap()),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req);

    let ps = am
        .provider_settings
        .unwrap()
        .expect("provider_settings set");
    assert_eq!(ps.get("oagw_alias"), Some(&json!("openai-staging")));
    assert_eq!(ps.get("endpoint_kind"), Some(&json!("responses")));
}

#[test]
fn model_update_no_patches_leaves_columns_unchanged() {
    // When the request has no patches AND no lifecycle change, no columns
    // should be touched (the ActiveModel is built from the existing entity
    // and only `updated_at` is NOT bumped).
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        Some(info_json),
    );

    let req = UpdateModelRequestV1::default();
    let am = model_update_active_model(&entity, &req);

    // All scalar columns match the input.
    assert_eq!(am.display_name.unwrap(), "GPT-4o");
    assert_eq!(
        am.description.unwrap(),
        Some("OpenAI's flagship model".to_owned())
    );
    assert_eq!(am.region.unwrap(), Some("us-east-1".to_owned()));
    assert_eq!(am.ctx_max_input_tokens.unwrap(), 128_000);
    assert!(am.allow_parameter_override.unwrap());

    // OData-filterable columns preserved.
    assert_eq!(am.vendor.unwrap(), Some("OpenAI".to_owned()));
    assert_eq!(am.family.unwrap(), Some("gpt-4".to_owned()));
    assert!(!am.managed.unwrap());
    assert_eq!(am.provider_model_id.unwrap(), Some("gpt-4o".to_owned()));
    assert!(am.cap_vision.unwrap());
    assert!(am.cap_function_calling.unwrap());
    assert!(am.cap_streaming.unwrap());
    assert!(am.cap_reasoning_effort.unwrap());
}
