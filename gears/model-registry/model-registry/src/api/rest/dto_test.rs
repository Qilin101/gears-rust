//! Tests for REST DTO (de)serialization.
//!
//! Response DTOs derive only `Serialize` (not `Deserialize`);
//! request DTOs derive only `Deserialize` (not `Serialize`).
//! Tests verify the appropriate direction per DTO role:
//!
//! - Response DTOs: construct in-memory → serialize to JSON
//! - Request DTOs:  construct JSON → deserialize to struct
//! - List DTOs:     construct in-memory → serialize to JSON

use serde_json::json;
use uuid::Uuid;

use super::dto::*;

// ---------------------------------------------------------------------------
// ProviderDto — response (Serialize only)
// ---------------------------------------------------------------------------

#[test]
fn provider_dto_serializes() {
    let id = Uuid::nil();
    let dto = ProviderDto {
        id,
        slug: "openai".into(),
        name: "OpenAI".into(),
        gts_type: "gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~".into(),
        status: "active".into(),
        managed: true,
        metadata: Some(json!({"region": "us-east-1"})),
        discovery_enabled: true,
        discovery_interval_seconds: Some(3600),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-06-15T12:30:00Z".into(),
    };

    let json = serde_json::to_value(&dto).expect("serialize ProviderDto");
    assert_eq!(json["id"], json!(id.to_string()));
    assert_eq!(json["slug"], "openai");
    assert_eq!(json["name"], "OpenAI");
    assert_eq!(
        json["gts_type"],
        "gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~"
    );
    assert_eq!(json["status"], "active");
    assert_eq!(json["managed"], true);
    assert_eq!(json["metadata"], json!({"region": "us-east-1"}));
    assert_eq!(json["discovery_enabled"], true);
    assert_eq!(json["discovery_interval_seconds"], 3600);
    assert_eq!(json["created_at"], "2026-01-01T00:00:00Z");
    assert_eq!(json["updated_at"], "2026-06-15T12:30:00Z");
}

#[test]
fn provider_dto_omits_absent_fields() {
    let dto = ProviderDto {
        id: Uuid::nil(),
        slug: "ollama".into(),
        name: "Ollama".into(),
        gts_type: "gts.cf.genai.models.provider.v1~cf.genai._.generic.v1~".into(),
        status: "active".into(),
        managed: false,
        metadata: None,
        discovery_enabled: false,
        discovery_interval_seconds: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    };

    let json = serde_json::to_value(&dto).expect("serialize");
    assert!(json.get("metadata").is_none());
    assert!(json.get("discovery_interval_seconds").is_none());
}

// ---------------------------------------------------------------------------
// CreateProviderRequestDto — request (Deserialize only)
// ---------------------------------------------------------------------------

#[test]
fn create_provider_request_deserializes() {
    let json = json!({
        "slug": "anthropic",
        "name": "Anthropic",
        "gts_type": "gts.cf.genai.models.provider.v1~cf.genai._.anthropic.v1~",
        "managed": true,
        "metadata": {"region": "eu-west-1"},
        "discovery_enabled": true,
        "discovery_interval_seconds": 1800,
    });

    let dto: CreateProviderRequestDto = serde_json::from_value(json).expect("deserialize");
    assert_eq!(dto.slug, "anthropic");
    assert_eq!(dto.name, "Anthropic");
    assert_eq!(
        dto.gts_type,
        "gts.cf.genai.models.provider.v1~cf.genai._.anthropic.v1~"
    );
    assert!(dto.managed);
    assert_eq!(dto.metadata, Some(json!({"region": "eu-west-1"})));
    assert!(dto.discovery_enabled);
    assert_eq!(dto.discovery_interval_seconds, Some(1800));
}

#[test]
fn create_provider_request_defaults() {
    let json = json!({
        "slug": "test",
        "name": "Test",
        "gts_type": "gts.cf.genai.models.provider.v1~cf.genai._.generic.v1~",
    });

    let dto: CreateProviderRequestDto = serde_json::from_value(json).expect("deserialize");
    assert!(!dto.managed);
    assert!(dto.metadata.is_none());
    assert!(!dto.discovery_enabled);
    assert!(dto.discovery_interval_seconds.is_none());
    assert_eq!(dto.slug, "test");
    assert_eq!(dto.name, "Test");
}

// ---------------------------------------------------------------------------
// UpdateProviderRequestDto — request (Deserialize only)
// ---------------------------------------------------------------------------

#[test]
fn update_provider_request_deserializes() {
    let json = json!({
        "name": "Updated",
        "status": "active",
        "managed": true,
        "metadata": {"k": "v"},
        "discovery_enabled": false,
        "discovery_interval_seconds": null,
    });

    let dto: UpdateProviderRequestDto = serde_json::from_value(json).expect("deserialize");
    assert_eq!(dto.name, Some("Updated".into()));
    assert_eq!(dto.status, Some("active".into()));
    assert_eq!(dto.managed, Some(true));
    assert_eq!(dto.metadata, Some(Some(json!({"k": "v"}))));
    assert_eq!(dto.discovery_enabled, Some(false));
    assert_eq!(dto.discovery_interval_seconds, Some(None));
}

#[test]
fn update_provider_request_omitted_fields_default_to_none() {
    let json = json!({});

    let dto: UpdateProviderRequestDto = serde_json::from_value(json).expect("deserialize");
    assert!(dto.name.is_none());
    assert!(dto.status.is_none());
    assert!(dto.managed.is_none());
    assert!(dto.metadata.is_none());
    assert!(dto.discovery_enabled.is_none());
    assert!(dto.discovery_interval_seconds.is_none());
}

// ---------------------------------------------------------------------------
// ModelDto — response (Serialize only)
// ---------------------------------------------------------------------------

#[test]
fn model_dto_serializes() {
    let info = json!({
        "display_name": "GPT-4o",
        "provider_model_id": "gpt-4o",
    });

    let dto = ModelDto {
        id: Uuid::nil(),
        canonical_id: "openai::gpt-4o".into(),
        lifecycle_status: "production".into(),
        approval_status: "approved".into(),
        info: info.clone(),
    };

    let json = serde_json::to_value(&dto).expect("serialize");
    assert_eq!(json["id"], json!(Uuid::nil().to_string()));
    assert_eq!(json["canonical_id"], "openai::gpt-4o");
    assert_eq!(json["lifecycle_status"], "production");
    assert_eq!(json["approval_status"], "approved");
    assert_eq!(json["info"], info);
}

// ---------------------------------------------------------------------------
// CreateModelRequestDto — request (Deserialize only)
// ---------------------------------------------------------------------------

#[test]
fn create_model_request_deserializes() {
    let info = json!({
        "display_name": "Claude 4.5 Sonnet",
        "provider_model_id": "claude-sonnet-4-5",
    });

    let json = json!({
        "provider_slug": "anthropic",
        "lifecycle_status": "production",
        "approval_status": "approved",
        "info": info,
    });

    let dto: CreateModelRequestDto = serde_json::from_value(json).expect("deserialize");
    assert_eq!(dto.provider_slug, "anthropic");
    assert_eq!(dto.lifecycle_status, "production");
    assert_eq!(dto.approval_status, Some("approved".into()));
}

#[test]
fn create_model_request_defaults_approval_status() {
    let json = json!({
        "provider_slug": "openai",
        "lifecycle_status": "experimental",
        "info": {"key": "value"},
    });

    let dto: CreateModelRequestDto = serde_json::from_value(json).expect("deserialize");
    assert!(dto.approval_status.is_none());
    assert_eq!(dto.provider_slug, "openai");
    assert_eq!(dto.lifecycle_status, "experimental");
    assert_eq!(dto.info, json!({"key": "value"}));
}

// ---------------------------------------------------------------------------
// UpdateModelRequestDto — request (Deserialize only)
// ---------------------------------------------------------------------------

#[test]
fn update_model_request_deserializes() {
    let json = json!({
        "approval_status": "approved",
        "lifecycle_status": "sunset",
        "display_name": "New Name",
        "description": "Updated desc",
        "family": null,
        "vendor": "Acme",
        "managed": true,
        "architecture": "llama",
        "size_bytes": null,
        "format": "gguf",
        "region": null,
        "hosted_by": "self",
        "reasoning_level": null,
        "version": "2.0",
        "sort_order": null,
        "icon": null,
        "multiplier_display": "3x",
        "performance": {"tokens_per_second": 45},
        "capabilities": {"vision": true},
        "disabled_capabilities": {"code_interpreter": true},
        "context_window": {"max_input_tokens": 128_000},
        "default_parameters": {"temperature": 0.7},
        "allow_parameter_override": true,
        "allow_extra_params": ["top_p", "frequency_penalty"],
        "provider_settings": {"oagw_alias": "prod"}
    });

    let dto: UpdateModelRequestDto = serde_json::from_value(json).expect("deserialize");
    assert_eq!(dto.approval_status, Some("approved".into()));
    assert_eq!(dto.lifecycle_status, Some("sunset".into()));
    assert_eq!(dto.display_name, Some("New Name".into()));
    assert_eq!(dto.description, Some(Some("Updated desc".into())));
    assert_eq!(dto.family, Some(None));
    assert_eq!(dto.vendor, Some(Some("Acme".into())));
    assert_eq!(dto.managed, Some(true));
    assert_eq!(dto.architecture, Some(Some("llama".into())));
    assert_eq!(dto.size_bytes, Some(None));
    assert_eq!(dto.format, Some(Some("gguf".into())));
    assert_eq!(dto.region, Some(None));
    assert_eq!(dto.hosted_by, Some(Some("self".into())));
    assert_eq!(dto.version, Some(Some("2.0".into())));
    assert_eq!(dto.multiplier_display, Some(Some("3x".into())));
    assert_eq!(dto.allow_parameter_override, Some(true));
    assert_eq!(
        dto.allow_extra_params,
        Some(vec!["top_p".into(), "frequency_penalty".into()])
    );
}

#[test]
#[allow(clippy::cognitive_complexity)]
fn update_model_request_omitted_fields_default_to_none() {
    let json = json!({});

    let dto: UpdateModelRequestDto = serde_json::from_value(json).expect("deserialize");
    assert!(dto.approval_status.is_none());
    assert!(dto.lifecycle_status.is_none());
    assert!(dto.display_name.is_none());
    assert!(dto.description.is_none());
    assert!(dto.family.is_none());
    assert!(dto.vendor.is_none());
    assert!(dto.managed.is_none());
    assert!(dto.architecture.is_none());
    assert!(dto.size_bytes.is_none());
    assert!(dto.format.is_none());
    assert!(dto.region.is_none());
    assert!(dto.hosted_by.is_none());
    assert!(dto.reasoning_level.is_none());
    assert!(dto.version.is_none());
    assert!(dto.sort_order.is_none());
    assert!(dto.icon.is_none());
    assert!(dto.multiplier_display.is_none());
    assert!(dto.performance.is_none());
    assert!(dto.capabilities.is_none());
    assert!(dto.disabled_capabilities.is_none());
    assert!(dto.context_window.is_none());
    assert!(dto.default_parameters.is_none());
    assert!(dto.allow_parameter_override.is_none());
    assert!(dto.allow_extra_params.is_none());
    assert!(dto.provider_settings.is_none());
}

// ---------------------------------------------------------------------------
// List DTOs — response (Serialize only)
// ---------------------------------------------------------------------------

#[test]
fn provider_list_dto_serializes() {
    let dto = ProviderListDto {
        items: vec![ProviderDto {
            id: Uuid::nil(),
            slug: "openai".into(),
            name: "OpenAI".into(),
            gts_type: "gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~".into(),
            status: "active".into(),
            managed: true,
            metadata: None,
            discovery_enabled: false,
            discovery_interval_seconds: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }],
        page_info: PageInfoDto {
            next_cursor: Some("cursor-abc".into()),
            prev_cursor: None,
            limit: 20,
        },
    };

    let json = serde_json::to_value(&dto).expect("serialize");
    assert!(json["items"].is_array());
    assert_eq!(json["items"][0]["slug"], "openai");
    assert_eq!(json["page_info"]["next_cursor"], "cursor-abc");
    assert!(json["page_info"].get("prev_cursor").is_none());
    assert_eq!(json["page_info"]["limit"], 20);
}

#[test]
fn model_list_dto_serializes() {
    let dto = ModelListDto {
        items: vec![ModelDto {
            id: Uuid::nil(),
            canonical_id: "openai::gpt-4o".into(),
            lifecycle_status: "production".into(),
            approval_status: "approved".into(),
            info: json!({"key": "val"}),
        }],
        page_info: PageInfoDto {
            next_cursor: None,
            prev_cursor: Some("cursor-xyz".into()),
            limit: 10,
        },
    };

    let json = serde_json::to_value(&dto).expect("serialize");
    assert!(json["items"].is_array());
    assert_eq!(json["items"][0]["canonical_id"], "openai::gpt-4o");
    assert!(json["page_info"].get("next_cursor").is_none());
    assert_eq!(json["page_info"]["prev_cursor"], "cursor-xyz");
    assert_eq!(json["page_info"]["limit"], 10);
}

// ---------------------------------------------------------------------------
// PageInfoDto — response (Serialize only)
// ---------------------------------------------------------------------------

#[test]
fn page_info_serializes_with_all_fields() {
    let dto = PageInfoDto {
        next_cursor: Some("next".into()),
        prev_cursor: Some("prev".into()),
        limit: 50,
    };

    let json = serde_json::to_value(&dto).expect("serialize");
    assert_eq!(json["next_cursor"], "next");
    assert_eq!(json["prev_cursor"], "prev");
    assert_eq!(json["limit"], 50);
}

#[test]
fn page_info_omits_absent_cursors() {
    let dto = PageInfoDto {
        next_cursor: None,
        prev_cursor: None,
        limit: 10,
    };

    let json = serde_json::to_value(&dto).expect("serialize");
    assert!(json.get("next_cursor").is_none());
    assert!(json.get("prev_cursor").is_none());
    assert_eq!(json["limit"], 10);
}
