use chrono::Utc;
use gts::GtsSchema;
use model_registry_sdk::models::{
    ApprovalStatus, CreateModelRequestV1, CreateProviderRequestV1, LifecycleStatus, ModelInfoV1,
    ModelV1, OpenAiSettingsV1, ProviderStatus, UpdateModelRequestV1, UpdateProviderRequestV1,
};
use serde_json::json;
use uuid::Uuid;

use super::super::entity;

use super::{
    model_create_active_model, model_entity_to_v1, model_update_active_model,
    provider_create_active_model, provider_entity_to_v1, provider_update_active_model,
};

// ---------------------------------------------------------------------------
// Test helpers — construct domain types via JSON roundtrip
// (all SDK types are #[non_exhaustive] and cannot use struct literal syntax)
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

/// Build a `ModelInfoV1` from a JSON template.
fn make_info(gts_leaf: &str, provider_settings: &serde_json::Value) -> ModelInfoV1 {
    let gts_type = format!("gts.cf.genai.model.info.v1~{gts_leaf}");
    let value = json!({
        "gts_type": gts_type,
        "display_name": "GPT-4o",
        "description": "OpenAI's flagship model",
        "family": "gpt-4",
        "vendor": "OpenAI",
        "managed": false,
        "architecture": "transformer",
        "format": "api-only",
        "version": "1.0",
        "sort_order": 10,
        "performance": {
            "response_latency_ms": 500,
            "tokens_per_second": 100
        },
        "additional_info": {},
        "supported_api": ["completion"],
        "provider_model_id": "gpt-4o",
        "capabilities": {
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
        },
        "disabled_capabilities": {
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
        },
        "context_window": {
            "max_input_tokens": 128_000,
            "max_output_tokens": 16_384
        },
        "default_parameters": {
            "temperature": null,
            "top_p": null,
            "max_output_tokens": null,
            "max_tool_calls": null,
            "presence_penalty": null,
            "frequency_penalty": null,
            "top_logprobs": null,
            "truncation": null,
            "service_tier": null,
            "parallel_tool_calls": null,
            "text": null,
            "reasoning": null,
            "tool_choice": null,
            "store": null
        },
        "allow_parameter_override": true,
        "allow_extra_params": ["custom_param"],
        "provider_settings": provider_settings
    });
    serde_json::from_value(value).expect("ModelInfoV1 from test JSON")
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
    info: Option<serde_json::Value>,
) -> entity::model::Model {
    entity::model::Model {
        id,
        provider_id,
        tenant_id,
        canonical_id: canonical_id.to_owned(),
        lifecycle_status: "production".to_owned(),
        deprecated_at: None,
        info,
        provider_settings: Some(openai_settings()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        // Denormalized columns
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
    // Patch fields via JSON roundtrip
    if let Ok(mut v) = serde_json::to_value(&info) {
        v["vendor"] = json!("Anthropic");
        v["family"] = json!("claude");
        v["provider_model_id"] = json!("claude-sonnet-4-20250514");
        info = serde_json::from_value(v).unwrap();
    }

    let info_json = serde_json::to_value(&info).expect("serialize");

    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "anthropic::claude-sonnet-4-20250514",
        Some(info_json),
    );

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
    if let Ok(mut v) = serde_json::to_value(&info) {
        v["vendor"] = json!("Custom");
        v["provider_model_id"] = json!("custom-model");
        info = serde_json::from_value(v).unwrap();
    }

    let info_json = serde_json::to_value(&info).expect("serialize");
    let entity = entity::model::Model {
        provider_settings: Some(raw_settings),
        ..make_model_entity(
            test_model_id(),
            test_provider_id(),
            test_tenant_id(),
            "custom::custom-model",
            Some(info_json),
        )
    };

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
    let entity = make_model_entity(
        test_model_id(),
        test_provider_id(),
        test_tenant_id(),
        "openai::gpt-4o",
        None,
    );

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.info.display_name, "model-openai::gpt-4o");
    assert_eq!(model.info.vendor.as_deref(), Some("OpenAI"));
    assert!(model.info.capabilities.vision.enabled);
    assert!(model.info.capabilities.function_calling);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Denormalized columns after create
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_create_denormalized_match_info() {
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
    assert!(am.info.unwrap().is_some());
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
// Denormalized columns re-projected after PATCH
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_update_reprojects_denormalized() {
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
    let entity = entity::model::Model {
        info: Some(json!({"this is": "not valid", "gts_type": 12345})),
        ..make_model_entity(
            test_model_id(),
            test_provider_id(),
            test_tenant_id(),
            "openai::gpt-4o",
            None,
        )
    };

    let model = model_entity_to_v1(&entity);

    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.info.display_name, "model-openai::gpt-4o");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Approval and lifecycle changes via update
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_update_approval_status() {
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
