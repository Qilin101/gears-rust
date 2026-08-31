use std::collections::{HashMap, HashSet};

use chrono::Utc;
use gts::GtsSchema;
use model_registry_sdk::models::{
    ApprovalStatus, ContextWindow, CreateModelRequestV1, DefaultInferenceParametersV1,
    DisabledCapabilities, DisabledMediaCapability, DisabledReasoningCapability,
    DisabledWebSearchCapability, LifecycleStatus, MediaCapability, ModelCapabilities, ModelInfoV1,
    ModelPerformance, ModelV1, OpenAiSettingsV1, ProviderStatus, ProviderV1, ReasoningCapability,
    SupportedApi, UpdateModelRequestV1, WebSearchCapability,
};
use serde_json::json;
use uuid::Uuid;

use crate::domain::error::DomainError;

use super::super::entity;

use super::{model_create_active_model, model_entity_to_v1, model_update_active_model};

// ---------------------------------------------------------------------------
// Test helpers — construct domain types via struct literals.
// ---------------------------------------------------------------------------

fn test_tenant_id() -> Uuid {
    Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
}

fn test_provider_id() -> Uuid {
    Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
}

/// The resolved provider the write path is given. Only `id` and `slug` are read
/// by `model_create_active_model` — `slug` is the left half of `canonical_id`.
fn test_provider() -> ProviderV1 {
    ProviderV1 {
        id: test_provider_id(),
        tenant_id: test_tenant_id(),
        slug: "openai".into(),
        name: "OpenAI".into(),
        gts_type: gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~"),
        status: ProviderStatus::Active,
        managed: false,
        metadata: None,
        discovery_enabled: false,
        discovery_interval_seconds: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
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

/// Fully-shaped `models` row: promoted scalars, JSONB sub-objects (holding the
/// complete sub-object shapes the write path stores), and filterable columns.
/// Per-test customizations are applied to the returned entity via direct field
/// assignment (e.g. `entity.vendor = Some(...)`).
fn make_model_entity(
    id: Uuid,
    provider_id: Uuid,
    tenant_id: Uuid,
    canonical_id: &str,
) -> entity::model::Model {
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
            "vision": { "enabled": true, "supported_mime_types": ["image/png"] },
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
        })),
        default_parameters: Some(json!({})),
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
// Model entity → SDK roundtrip (OpenAI)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_openai() {
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    let model: ModelV1 = model_entity_to_v1(entity).expect("model maps");

    assert_eq!(model.id, test_model_id());
    assert_eq!(model.provider_id, test_provider_id());
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
    // Scalar columns are authoritative — set the anthropic identity columns.
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "anthropic::claude-sonnet-4-20250514",
    );
    entity.provider_settings = Some(json!({
        "oagw_alias": "anthropic-prod",
        "anthropic_version": "2023-06-01",
        "max_tokens": 8192,
    }));
    entity.vendor = Some("Anthropic".to_owned());
    entity.family = Some("claude".to_owned());
    entity.provider_model_id = Some("claude-sonnet-4-20250514".to_owned());
    entity.gts_type = Some("gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~".to_owned());

    let model = model_entity_to_v1(entity).expect("model maps");

    assert_eq!(model.canonical_id, "anthropic::claude-sonnet-4-20250514");
    assert_eq!(model.info.vendor.as_deref(), Some("Anthropic"));
    assert_eq!(model.info.family.as_deref(), Some("claude"));
    assert_eq!(model.info.provider_model_id, "claude-sonnet-4-20250514");
    assert_eq!(
        model.info.gts_type.as_ref(),
        "gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model entity → SDK (unknown provider, raw JSON)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_unknown_provider() {
    let raw_settings = json!({"custom_endpoint": "https://custom.example.com"});
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "custom::custom-model",
    );
    entity.provider_settings = Some(raw_settings);
    entity.vendor = Some("Custom".to_owned());
    entity.provider_model_id = Some("custom-model".to_owned());
    entity.gts_type = Some("gts.cf.genai.model.info.v1~cf.genai._.custom.v1~".to_owned());

    let model = model_entity_to_v1(entity).expect("model maps");

    assert_eq!(model.canonical_id, "custom::custom-model");
    assert_eq!(
        model.info.provider_settings.get("custom_endpoint"),
        Some(&json!("https://custom.example.com"))
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Out-of-domain enum columns are typed errors, not panics
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_rejects_corrupt_lifecycle_status() {
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );
    entity.lifecycle_status = "retired".to_owned();

    let err =
        model_entity_to_v1(entity).expect_err("out-of-domain lifecycle_status must be rejected");
    assert!(
        matches!(&err, DomainError::Internal { .. }),
        "expected Internal, got {err:?}"
    );
}

#[test]
fn model_entity_to_v1_rejects_corrupt_approval_status() {
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );
    entity.approval_status = "escalated".to_owned();

    let err =
        model_entity_to_v1(entity).expect_err("out-of-domain approval_status must be rejected");
    assert!(
        matches!(&err, DomainError::Internal { .. }),
        "expected Internal, got {err:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// OData-filterable columns after create
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_create_projects_filterable_columns() {
    let info = make_info("cf.genai._.openai.v1~", &openai_settings());
    let req = CreateModelRequestV1 {
        provider_id: test_provider_id(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: Some(ApprovalStatus::Approved),
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
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
        provider_id: test_provider_id(),
        lifecycle_status: LifecycleStatus::Preview,
        approval_status: None,
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
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
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    let req = UpdateModelRequestV1 {
        vendor: Some(Some("OpenAI-Updated".into())),
        family: Some(Some("gpt-4.1".into())),
        managed: Some(true),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req).expect("update maps");

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
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    let req = UpdateModelRequestV1::default();
    let am = model_update_active_model(&entity, &req).expect("update maps");

    assert_eq!(am.vendor.unwrap(), Some("OpenAI".into()));
    assert_eq!(am.family.unwrap(), Some("gpt-4".into()));
    assert!(!am.managed.unwrap());
    assert!(am.cap_vision.unwrap());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Approval and lifecycle changes via update
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_update_applies_approval_status() {
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    let req = UpdateModelRequestV1 {
        approval_status: Some(ApprovalStatus::Rejected),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req).expect("update maps");

    // approval_status is patched in place by the mapper — assert the new value
    // is applied.
    assert_eq!(am.approval_status.unwrap(), "rejected");
}

#[test]
fn model_update_lifecycle_status() {
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    let req = UpdateModelRequestV1 {
        lifecycle_status: Some(LifecycleStatus::Deprecated),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req).expect("update maps");

    assert_eq!(am.lifecycle_status.unwrap(), "deprecated");
}

// ═══════════════════════════════════════════════════════════════════════════════
// provider_settings roundtrip
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_entity_to_v1_preserves_provider_settings() {
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    let model = model_entity_to_v1(entity).expect("model maps");

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
        size_bytes: Some(1_073_741_824),
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
            "vision": { "enabled": false, "supported_mime_types": ["image/png", "image/jpeg"] },
            "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
            "function_calling": false,
            "response_schema": true,
            "streaming": false,
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
            "vision": { "disabled": true, "disabled_mime_types": ["image/gif"] },
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
        allow_extra_params: Some(json!(["custom_param", "trace_id"])),
        // OData-filterable columns
        gts_type: Some("gts.cf.genai.model.info.v1~cf.genai._.openai.v1~".to_owned()),
        vendor: Some("OpenAI".to_owned()),
        family: Some("gpt-4".to_owned()),
        managed: false,
        architecture: Some("transformer".to_owned()),
        format: Some("api-only".to_owned()),
        provider_model_id: Some("gpt-4o".to_owned()),
        supported_api: Some("batch,completion".to_owned()),
        approval_status: "approved".to_owned(),
        cap_vision: true,
        cap_function_calling: true,
        cap_streaming: true,
        cap_reasoning_effort: true,
    };

    let model = model_entity_to_v1(entity).expect("model maps");

    // Scalar field round-trip
    assert_eq!(model.info.display_name, "GPT-4o");
    assert_eq!(
        model.info.description.as_deref(),
        Some("OpenAI's flagship model")
    );
    assert_eq!(model.info.size_bytes, Some(1_073_741_824));
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
    assert_eq!(
        model.info.supported_api,
        HashSet::from([SupportedApi::Batch, SupportedApi::Completion])
    );

    // JSONB sub-object round-trip
    assert!(
        model.info.capabilities.vision.enabled,
        "scalar `cap_vision` must override JSONB"
    );
    assert!(model.info.capabilities.function_calling);
    assert!(model.info.capabilities.streaming);
    assert!(model.info.capabilities.reasoning.effort);
    assert_eq!(
        model.info.capabilities.vision.supported_mime_types,
        vec!["image/png".to_owned(), "image/jpeg".to_owned()],
        "vision mime types must come from JSONB"
    );
    assert!(
        model.info.capabilities.response_schema,
        "non-indexed capability fields must come from JSONB"
    );
    assert!(model.info.disabled_capabilities.vision.disabled);
    assert_eq!(
        model.info.disabled_capabilities.vision.disabled_mime_types,
        vec!["image/gif".to_owned()]
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
fn model_entity_to_v1_default_db_values_reconstruct_without_error() {
    // Simulate a row inserted via raw SQL bypassing the application layer —
    // only the migration defaults are populated. The read path must produce a
    // valid `ModelInfoV1` from the columns alone.
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
        gts_type: None, // nullable identity column
        vendor: Some("OpenAI".to_owned()),
        family: None,
        managed: false,
        architecture: None,
        format: None,
        provider_model_id: None, // nullable identity column
        supported_api: None,
        approval_status: "pending".to_owned(),
        cap_vision: false,
        cap_function_calling: false,
        cap_streaming: false,
        cap_reasoning_effort: false,
    };

    let model = model_entity_to_v1(entity).expect("model maps");

    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.info.vendor.as_deref(), Some("OpenAI"));
    // The two nullable identity columns read back as empty strings — no
    // synthesized placeholder, no fallback projection.
    assert_eq!(model.info.gts_type.as_ref(), "");
    assert_eq!(model.info.provider_model_id, "");
    assert_eq!(model.info.display_name, "");
    assert_eq!(model.info.context_window.max_input_tokens, 0);
}

#[test]
fn model_entity_to_v1_handles_null_jsonb_sub_objects() {
    // Verify each JSONB sub-object column can be NULL without breaking the
    // read path (defaulting to empty / null shapes).
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );
    entity.provider_settings = None;
    entity.capabilities_full = None;
    entity.default_parameters = None;
    entity.additional_info = None;
    entity.disabled_capabilities_full = None;
    entity.allow_extra_params = None;
    entity.cap_reasoning_effort = false;

    let model = model_entity_to_v1(entity).expect("model maps");

    assert_eq!(model.info.display_name, "GPT-4o");
    assert_eq!(model.info.context_window.max_input_tokens, 128_000);
    // Capabilities fall back to the scalar columns; the rest defaults.
    assert!(model.info.capabilities.vision.enabled);
    assert!(model.info.capabilities.function_calling);
    assert!(model.info.capabilities.streaming);
    assert!(!model.info.capabilities.reasoning.effort);
    assert!(!model.info.capabilities.response_schema);
    assert!(
        model
            .info
            .capabilities
            .vision
            .supported_mime_types
            .is_empty()
    );
    assert_eq!(
        model.info.disabled_capabilities,
        DisabledCapabilities::none()
    );
    assert_eq!(
        model.info.default_parameters,
        DefaultInferenceParametersV1::default()
    );
    assert!(model.info.additional_info.is_empty());
    assert!(model.info.allow_extra_params.is_empty());
    assert!(
        model.info.provider_settings.is_null(),
        "missing provider_settings JSONB must surface as null on the wire"
    );
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
        provider_id: test_provider_id(),
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
        provider_id: test_provider_id(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: Some(ApprovalStatus::Approved),
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
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
        &test_provider(),
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
        provider_id: test_provider_id(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
        &req,
        ApprovalStatus::Pending,
    );

    assert!(am.provider_settings.unwrap().is_none());
}

#[test]
fn model_create_active_model_stores_full_capabilities() {
    // `capabilities_full` stores the complete `ModelCapabilities`; the four
    // indexed flags are additionally shadowed into their scalar columns.
    let req = make_create_request("cf.genai._.openai.v1~");
    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
        &req,
        ApprovalStatus::Approved,
    );

    // Scalar columns carry the indexed flags.
    assert!(am.cap_vision.unwrap());
    assert!(am.cap_function_calling.unwrap());
    assert!(am.cap_streaming.unwrap());
    assert!(am.cap_reasoning_effort.unwrap());

    // JSONB carries the whole struct — it deserializes back to the input.
    let cap_full = am
        .capabilities_full
        .unwrap()
        .expect("capabilities_full set");
    let stored: ModelCapabilities =
        serde_json::from_value(cap_full).expect("capabilities_full holds a full ModelCapabilities");
    assert_eq!(stored, req.info.capabilities);
}

#[test]
fn model_create_active_model_stores_disabled_capabilities_fully() {
    // `disabled_capabilities_full` is NOT promoted — the entire DisabledCapabilities
    // structure rides in this single JSONB column.
    let req = make_create_request("cf.genai._.openai.v1~");
    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
        &req,
        ApprovalStatus::Approved,
    );

    let disabled_full = am
        .disabled_capabilities_full
        .unwrap()
        .expect("disabled_capabilities_full set");
    let stored: DisabledCapabilities = serde_json::from_value(disabled_full)
        .expect("disabled_capabilities_full holds a full DisabledCapabilities");
    assert_eq!(stored, req.info.disabled_capabilities);
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
        provider_id: test_provider_id(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    };

    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
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
        provider_id: test_provider_id(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    };
    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
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
        &test_provider(),
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
        &test_provider(),
        &req,
        ApprovalStatus::Approved,
    );

    // canonical_id = {provider.slug}::{provider_model_id}
    assert_eq!(am.canonical_id.unwrap(), "openai::gpt-4o");
}

#[test]
fn model_create_active_model_sets_supported_api_csv() {
    // `supported_api` is serialized as a comma-separated string. Verify it is
    // sorted (deterministic) and lowercase.
    let mut info = make_info("cf.genai._.openai.v1~", &openai_settings());
    info.supported_api = HashSet::from([SupportedApi::Completion, SupportedApi::Batch]);
    let req = CreateModelRequestV1 {
        provider_id: test_provider_id(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    };
    let am = model_create_active_model(
        test_tenant_id(),
        &test_provider(),
        &req,
        ApprovalStatus::Approved,
    );

    let sa = am.supported_api.unwrap().expect("supported_api set");
    assert_eq!(sa, "batch,completion");
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
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    let req = UpdateModelRequestV1 {
        description: Some(Some("Updated description".to_owned())),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req).expect("update maps");

    // Patched field.
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
    // JSONB (which stores the complete struct).
    let mut entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );
    // Force entity to start with the flags off so the patch has visible effect.
    entity.cap_vision = false;
    entity.cap_function_calling = false;
    entity.cap_streaming = false;
    entity.cap_reasoning_effort = false;

    let patched = ModelCapabilities {
        vision: MediaCapability {
            enabled: true,
            supported_mime_types: vec!["image/jpeg".to_owned()],
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
        ..ModelCapabilities::default()
    };
    let req = UpdateModelRequestV1 {
        capabilities: Some(patched.clone()),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req).expect("update maps");

    // Scalar booleans flipped.
    assert!(am.cap_vision.unwrap());
    assert!(am.cap_function_calling.unwrap());
    assert!(am.cap_streaming.unwrap());
    assert!(am.cap_reasoning_effort.unwrap());

    // JSONB `capabilities_full` re-projected with the complete struct.
    let cap_full = am
        .capabilities_full
        .unwrap()
        .expect("capabilities_full set");
    let stored: ModelCapabilities =
        serde_json::from_value(cap_full).expect("capabilities_full holds a full ModelCapabilities");
    assert_eq!(stored, patched);
}

#[test]
fn model_update_patches_provider_settings_reprojects_column() {
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    // PATCH provider_settings to a new value.
    let req = UpdateModelRequestV1 {
        provider_settings: Some(json!({
            "oagw_alias": "openai-staging",
            "endpoint_kind": "responses",
        })),
        ..UpdateModelRequestV1::default()
    };

    let am = model_update_active_model(&entity, &req).expect("update maps");

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
    // and `updated_at` is NOT bumped).
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
    );

    let req = UpdateModelRequestV1::default();
    let am = model_update_active_model(&entity, &req).expect("update maps");

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
