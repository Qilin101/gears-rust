//! Entity ↔ SDK type mappers.
//!
//! Converts between `SeaORM` entities (`provider::Model`, `model::Model`) and
//! their SDK counterparts (`ProviderV1`, `ModelV1`), and provides
//! [`ActiveModel`] builders for create/update operations that project
//! denormalized filterable columns from the JSONB `info` column.
//!
//! # Denormalized column projection
//!
//! Every `OData`-filterable field promoted from `info` is kept in sync by these
//! builders. The `info` JSONB remains the authoritative source of truth; the
//! denormalized columns are read-only shadows for `OData` query performance.
//!
//! # Immutability enforcement
//!
//! The builders reject attempts to modify immutable identity fields:
//! `canonical_id`, `provider_slug`, `info.provider_model_id`, `info.gts_type`.
//! These are enforced at the mapper level so the repository never sees them.
//!
//! # `#[non_exhaustive]` handling
//!
//! SDK types `ProviderV1`, `ModelV1`, `ModelInfoV1`, and their inner types are
//! `#[non_exhaustive]` — they cannot be constructed via struct literal from
//! outside the SDK crate. This mapper uses serde JSON roundtripping:
//! `serde_json::to_value` (or manual JSON value construction) → deserialization
//! to the target type.

use model_registry_sdk::models::{
    ApprovalStatus, CreateModelRequestV1, CreateProviderRequestV1, LifecycleStatus, ModelInfoV1,
    ModelV1, ProviderStatus, ProviderV1, SupportedApi, UpdateModelRequestV1,
    UpdateProviderRequestV1,
};
use sea_orm::Set;
use serde_json::json;
use uuid::Uuid;

use super::entity;

// ═══════════════════════════════════════════════════════════════════════════════
// Provider conversions
// ═══════════════════════════════════════════════════════════════════════════════

/// Convert a `provider::Model` entity to `ProviderV1` using serde roundtrip.
///
/// Constructs a JSON value with the correct serde field names (lowercase for
/// enums via `rename_all = "lowercase"`) and deserializes into `ProviderV1`,
/// which is `#[non_exhaustive]` and cannot be built via struct literal.
///
/// # Panics
/// Only if the entity fields are somehow unserializable (programming error —
/// all field types are controlled by this crate).
#[must_use]
#[allow(clippy::expect_used)]
pub fn provider_entity_to_v1(e: &entity::provider::Model) -> ProviderV1 {
    let value = json!({
        "id": e.id,
        "slug": e.slug,
        "name": e.name,
        "gts_type": e.gts_type,
        "status": e.status,
        "managed": e.managed,
        "metadata": e.metadata,
        "discovery_enabled": e.discovery_enabled,
        "discovery_interval_seconds": e.discovery_interval_seconds,
        "created_at": e.created_at,
        "updated_at": e.updated_at,
    });
    // SAFETY: the JSON value is constructed inline from trusted entity fields
    // whose types are known to roundtrip. A panic here is a programming error.
    serde_json::from_value(value).expect("ProviderV1 roundtrip")
}

/// Build a `provider::ActiveModel` from a create request.
#[must_use]
pub fn provider_create_active_model(
    tenant_id: Uuid,
    req: &CreateProviderRequestV1,
) -> entity::provider::ActiveModel {
    entity::provider::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        slug: Set(req.slug().to_owned()),
        name: Set(req.name().to_owned()),
        gts_type: Set(req.gts_type().to_string()),
        status: Set(provider_status_str(ProviderStatus::Active)),
        managed: Set(req.managed()),
        metadata: Set(req.metadata().cloned()),
        discovery_enabled: Set(req.discovery_enabled()),
        discovery_interval_seconds: Set(req
            .discovery_interval_seconds()
            .map(|v| i32::try_from(v).unwrap_or(i32::MAX))),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    }
}

/// Build a `provider::ActiveModel` from an update request (PATCH semantics).
///
/// Only fields present (non-`None`) in `req` are applied. The slug is
/// immutable and silently ignored if present in the request.
#[must_use]
pub fn provider_update_active_model(
    existing: &entity::provider::Model,
    req: &UpdateProviderRequestV1,
) -> entity::provider::ActiveModel {
    let mut active: entity::provider::ActiveModel = existing.clone().into();

    if let Some(name) = &req.name {
        active.name = Set(name.clone());
    }
    if let Some(status) = &req.status {
        active.status = Set(provider_status_str(*status));
    }
    if let Some(managed) = req.managed {
        active.managed = Set(managed);
    }
    if let Some(metadata) = &req.metadata {
        active.metadata = Set(metadata.clone());
    }
    if let Some(discovery_enabled) = req.discovery_enabled {
        active.discovery_enabled = Set(discovery_enabled);
    }
    if let Some(interval) = req.discovery_interval_seconds {
        active.discovery_interval_seconds =
            Set(interval.map(|v| i32::try_from(v).unwrap_or(i32::MAX)));
    }

    active.updated_at = Set(chrono::Utc::now());
    active
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model conversions
// ═══════════════════════════════════════════════════════════════════════════════

/// Convert a `model::Model` entity to `ModelV1<serde_json::Value>`.
///
/// Deserializes the `info` JSONB column into `ModelInfoV1`. Falls back to a
/// minimal reconstruction from denormalized columns if the JSONB is missing
/// or corrupt (graceful degradation).
#[must_use]
#[allow(clippy::expect_used)]
pub fn model_entity_to_v1(e: &entity::model::Model) -> ModelV1 {
    let lifecycle_status = e.lifecycle_status.clone();
    let approval_status = e.approval_status.clone();

    let info = e
        .info
        .as_ref()
        .and_then(|v| serde_json::from_value::<ModelInfoV1>(v.clone()).ok())
        .unwrap_or_else(|| build_minimal_info(e));

    let value = json!({
        "id": e.id,
        "canonical_id": e.canonical_id,
        "lifecycle_status": lifecycle_status,
        "approval_status": approval_status,
        "info": info,
    });
    // SAFETY: same reasoning as provider_entity_to_v1; the JSON is constructed
    // from trusted entity fields whose serde representations are known.
    serde_json::from_value(value).expect("ModelV1 roundtrip")
}

/// Build a `model::ActiveModel` from a create request.
///
/// Serializes `info` to JSONB, extracts `provider_settings`, projects the
/// denormalized filterable columns, and derives `canonical_id` as
/// `{provider_slug}::{info.provider_model_id}`.
#[must_use]
pub fn model_create_active_model(
    tenant_id: Uuid,
    provider_id: Uuid,
    req: &CreateModelRequestV1,
    initial_approval_status: ApprovalStatus,
) -> entity::model::ActiveModel {
    let canonical_id = format!("{}::{}", req.provider_slug, req.info.provider_model_id);
    // SAFETY: req.info was just deserialized from valid JSON; re-serialization
    // cannot fail for well-formed domain types (no non-string map keys, no
    // I/O). Use expect to fail-fast rather than silently storing null.
    #[allow(clippy::expect_used)]
    let info_json = serde_json::to_value(&req.info)
        .expect("CreateModelRequestV1.info re-serialization cannot fail");

    // SAFETY: same reasoning — provider_settings is a JSON-compatible type.
    #[allow(clippy::expect_used)]
    let provider_settings_json = serde_json::to_value(&req.info.provider_settings)
        .expect("CreateModelRequestV1.info.provider_settings re-serialization cannot fail");

    entity::model::ActiveModel {
        id: Set(Uuid::new_v4()),
        provider_id: Set(provider_id),
        tenant_id: Set(tenant_id),
        canonical_id: Set(canonical_id),
        lifecycle_status: Set(lifecycle_status_str(req.lifecycle_status)),
        deprecated_at: Set(None),
        info: Set(Some(info_json)),
        provider_settings: Set(if provider_settings_json.is_null() {
            None
        } else {
            Some(provider_settings_json)
        }),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        // Denormalized columns
        gts_type: Set(Some(req.info.gts_type.to_string())),
        vendor: Set(req.info.vendor.clone()),
        family: Set(req.info.family.clone()),
        managed: Set(req.info.managed),
        architecture: Set(req.info.architecture.clone()),
        format: Set(req.info.format.clone()),
        provider_model_id: Set(Some(req.info.provider_model_id.clone())),
        supported_api: Set(supported_api_set_to_str(&req.info.supported_api)),
        approval_status: Set(approval_status_str(initial_approval_status)),
        cap_vision: Set(req.info.capabilities.vision.enabled),
        cap_function_calling: Set(req.info.capabilities.function_calling),
        cap_streaming: Set(req.info.capabilities.streaming),
        cap_reasoning_effort: Set(req.info.capabilities.reasoning.effort),
    }
}

/// Build a `model::ActiveModel` for a PATCH update.
///
/// Reads the existing entity and the update request, applies only the
/// `Some(...)` fields, re-serializes `info` JSONB if any info field changed,
/// and re-projects the denormalized filterable columns.
///
/// Immutable fields (`canonical_id`, `provider_slug`, `info.provider_model_id`,
/// `info.gts_type`) are silently ignored if present in the request.
#[allow(clippy::cognitive_complexity)]
#[must_use]
pub fn model_update_active_model(
    existing: &entity::model::Model,
    req: &UpdateModelRequestV1,
) -> entity::model::ActiveModel {
    let mut active: entity::model::ActiveModel = existing.clone().into();

    // — Lifecycle status —
    if let Some(lifecycle) = &req.lifecycle_status {
        active.lifecycle_status = Set(lifecycle_status_str(*lifecycle));
    }

    // — Approval status —
    if let Some(status) = &req.approval_status {
        active.approval_status = Set(approval_status_str(*status));
    }

    // — Info-based fields: deserialize existing info, apply patches, re-serialize —
    let mut info: Option<ModelInfoV1> = existing
        .info
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let info_changed = info
        .as_mut()
        .is_some_and(|info_inner| apply_info_patches(info_inner, req));

    if info_changed {
        // SAFETY: info was just deserialized from JSON and is re-serializable.
        #[allow(clippy::expect_used)]
        let fresh = info.expect("info is Some after apply_info_patches");
        // SAFETY: info was just deserialized from stored JSON; re-serialization
        // cannot fail for well-formed domain types. Fail-fast rather than
        // silently storing null.
        #[allow(clippy::expect_used)]
        let info_json = serde_json::to_value(&fresh)
            .expect("ModelInfoV1 re-serialization cannot fail");
        active.info = Set(Some(info_json));

        // Re-project denormalized columns
        active.gts_type = Set(Some(fresh.gts_type.to_string()));
        active.vendor = Set(fresh.vendor.clone());
        active.family = Set(fresh.family.clone());
        active.managed = Set(fresh.managed);
        active.architecture = Set(fresh.architecture.clone());
        active.format = Set(fresh.format.clone());
        active.provider_model_id = Set(Some(fresh.provider_model_id.clone()));
        active.supported_api = Set(supported_api_set_to_str(&fresh.supported_api));
        active.cap_vision = Set(fresh.capabilities.vision.enabled);
        active.cap_function_calling = Set(fresh.capabilities.function_calling);
        active.cap_streaming = Set(fresh.capabilities.streaming);
        active.cap_reasoning_effort = Set(fresh.capabilities.reasoning.effort);

        // Re-extract provider_settings
        // SAFETY: provider_settings was just deserialized from stored JSON;
        // re-serialization cannot fail for JSON-compatible types.
        #[allow(clippy::expect_used)]
        let ps_json = serde_json::to_value(&fresh.provider_settings)
            .expect("provider_settings re-serialization cannot fail");
        active.provider_settings = Set(if ps_json.is_null() {
            None
        } else {
            Some(ps_json)
        });
    }

    active.updated_at = Set(chrono::Utc::now());
    active
}

/// Apply `UpdateModelRequestV1` patches to a `ModelInfoV1` in-place.
///
/// Returns `true` if any field was changed.
#[allow(clippy::cognitive_complexity)]
fn apply_info_patches(info: &mut ModelInfoV1, req: &UpdateModelRequestV1) -> bool {
    let mut changed = false;

    macro_rules! patch_opt {
        ($field:ident) => {
            if let Some(ref v) = req.$field {
                info.$field.clone_from(v);
                changed = true;
            }
        };
    }
    macro_rules! patch_optopt {
        ($field:ident) => {
            if let Some(Some(ref v)) = req.$field {
                info.$field = Some(v.clone());
                changed = true;
            } else if let Some(None) = req.$field {
                info.$field = None;
                changed = true;
            }
        };
    }
    macro_rules! patch_val {
        ($field:ident) => {
            if let Some(ref v) = req.$field {
                info.$field = v.clone();
                changed = true;
            }
        };
    }

    patch_opt!(display_name);
    patch_optopt!(description);
    patch_optopt!(family);
    patch_optopt!(vendor);
    patch_val!(managed);
    patch_optopt!(architecture);
    patch_optopt!(size_bytes);
    patch_optopt!(format);
    patch_optopt!(region);
    patch_optopt!(hosted_by);
    patch_optopt!(reasoning_level);
    patch_optopt!(version);
    patch_optopt!(sort_order);
    patch_optopt!(icon);
    patch_optopt!(multiplier_display);
    patch_val!(performance);
    patch_val!(capabilities);
    patch_val!(disabled_capabilities);
    patch_val!(context_window);
    patch_val!(default_parameters);
    patch_val!(allow_parameter_override);
    patch_val!(allow_extra_params);
    patch_val!(provider_settings);

    changed
}

// ═══════════════════════════════════════════════════════════════════════════════
// Helper utilities — enum ↔ string
// ═══════════════════════════════════════════════════════════════════════════════

/// Convert a `ProviderStatus` to its lowercase storage string.
#[must_use]
#[allow(clippy::match_same_arms)]
fn provider_status_str(status: ProviderStatus) -> String {
    match status {
        ProviderStatus::Active => "active".to_owned(),
        ProviderStatus::Disabled => "disabled".to_owned(),
        _ => "active".to_owned(),
    }
}

/// Convert a `LifecycleStatus` to its lowercase storage string.
#[must_use]
#[allow(clippy::match_same_arms)]
fn lifecycle_status_str(status: LifecycleStatus) -> String {
    match status {
        LifecycleStatus::Production => "production".to_owned(),
        LifecycleStatus::Preview => "preview".to_owned(),
        LifecycleStatus::Experimental => "experimental".to_owned(),
        LifecycleStatus::Deprecated => "deprecated".to_owned(),
        LifecycleStatus::Sunset => "sunset".to_owned(),
        _ => "production".to_owned(),
    }
}

/// Convert an `ApprovalStatus` to its lowercase storage string.
#[must_use]
#[allow(clippy::match_same_arms)]
fn approval_status_str(status: ApprovalStatus) -> String {
    match status {
        ApprovalStatus::Pending => "pending".to_owned(),
        ApprovalStatus::Approved => "approved".to_owned(),
        ApprovalStatus::Rejected => "rejected".to_owned(),
        ApprovalStatus::Revoked => "revoked".to_owned(),
        _ => "pending".to_owned(),
    }
}

/// Serialize a `HashSet<SupportedApi>` to a comma-separated string for the
/// denormalized `supported_api` column.
fn supported_api_set_to_str(apis: &std::collections::HashSet<SupportedApi>) -> Option<String> {
    if apis.is_empty() {
        return None;
    }
    let mut items: Vec<String> = apis.iter().copied().map(supported_api_str).collect();
    items.sort_unstable();
    Some(items.join(","))
}

/// Convert a `SupportedApi` variant to its lowercase string representation.
#[must_use]
fn supported_api_str(api: SupportedApi) -> String {
    match api {
        SupportedApi::Completion => "completion".to_owned(),
        SupportedApi::Embedding => "embedding".to_owned(),
        SupportedApi::Batch => "batch".to_owned(),
        _ => "unknown".to_owned(),
    }
}

/// Parse a comma-separated `supported_api` string back into a `HashSet`.
#[allow(dead_code)]
fn parse_supported_api_set(s: Option<&str>) -> std::collections::HashSet<SupportedApi> {
    let mut apis = std::collections::HashSet::new();
    if let Some(s) = s {
        for part in s.split(',') {
            let part = part.trim();
            if !part.is_empty()
                && let Some(api) = supported_api_from_str(part)
            {
                apis.insert(api);
            }
        }
    }
    apis
}

#[allow(dead_code)]
fn supported_api_from_str(s: &str) -> Option<SupportedApi> {
    match s {
        "completion" => Some(SupportedApi::Completion),
        "embedding" => Some(SupportedApi::Embedding),
        "batch" => Some(SupportedApi::Batch),
        _ => None,
    }
}

/// Convert a denormalized comma-separated `supported_api` string to a JSON
/// array of lowercase strings (the format `ModelInfoV1.supported_api` expects).
fn supported_api_denorm_to_json_array(s: Option<&str>) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    if let Some(s) = s {
        for part in s.split(',') {
            let part = part.trim();
            if !part.is_empty() {
                items.push(part.to_owned());
            }
        }
    }
    items
}

/// Build a minimal `ModelInfoV1` from the denormalized columns when the `info`
/// JSONB is missing or corrupt.
fn build_minimal_info(e: &entity::model::Model) -> ModelInfoV1 {
    // Since ModelInfoV1 and its inner types are #[non_exhaustive], construct via
    // JSON roundtrip.
    let gts_type_str = e.gts_type.as_deref().unwrap_or("gts.cf.genai.model.info.v1~");
    let display_name = format!("model-{}", e.canonical_id);

    let value = json!({
        "gts_type": gts_type_str,
        "display_name": display_name,
        "description": null,
        "family": e.family,
        "vendor": e.vendor,
        "managed": e.managed,
        "architecture": e.architecture,
        "size_bytes": null,
        "format": e.format,
        "region": null,
        "hosted_by": null,
        "last_release_at": null,
        "reasoning_level": null,
        "version": null,
        "sort_order": null,
        "icon": null,
        "multiplier_display": null,
        "performance": {
            "response_latency_ms": null,
            "tokens_per_second": null
        },
        "additional_info": {},
        "supported_api": supported_api_denorm_to_json_array(e.supported_api.as_deref()),
        "provider_model_id": e.provider_model_id.as_deref().unwrap_or(""),
        "capabilities": {
            "vision": { "enabled": e.cap_vision, "supported_mime_types": [] },
            "reasoning": { "effort": e.cap_reasoning_effort, "toggle": false, "resume": false, "budget": false },
            "function_calling": e.cap_function_calling,
            "response_schema": false,
            "streaming": e.cap_streaming,
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
            "max_input_tokens": 0,
            "max_output_tokens": null,
            "output_vector_size": null
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
        "allow_parameter_override": false,
        "allow_extra_params": [],
        "provider_settings": null
    });
    // SAFETY: the JSON template above is constructed from entity fields with
    // known types. A failure here indicates a programming error or a corrupt
    // denormalized column (e.g. a new SupportedApi variant was added but the
    // column wasn't updated). Degrade gracefully rather than panicking.
    serde_json::from_value(value).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            "ModelInfoV1 roundtrip from denormalized columns failed, using fallback"
        );
        // Minimal fallback: only identity fields. The inner fallback is a
        // hardcoded static JSON that should always deserialize — if it
        // somehow doesn't, the all-null static JSON is the last resort.
        serde_json::from_value(json!({
            "gts_type": gts_type_str,
            "display_name": display_name,
        }))
        .unwrap_or_else(|_| {
            serde_json::from_value(json!({
                "gts_type": "gts.cf.genai.model.info.v1~",
                "display_name": "model-(unknown)",
            }))
            .unwrap_or_else(|_| {
                // Unreachable: the hardcoded JSON contains only primitive
                // types that always deserialize into the required fields.
                tracing::error!("all ModelInfoV1 fallback attempts failed - this is a programming bug");
                // Last resort: all-null value will produce a struct with
                // gts_type="" and display_name="" via serde defaults.
                serde_json::from_value(serde_json::Value::Object(serde_json::Map::default()))
                    .unwrap_or_else(|_| {
                        // If even serde defaults fail, ModelInfoV1's schema
                        // has changed incompatibly — but serde should always
                        // produce a valid value from an empty object since
                        // the struct uses Option<T> for non-required fields.
                        // This panic is a last resort for a programming error.
                        panic!("ModelInfoV1 is no longer constructible from an empty JSON object")
                    })
            })
        })
    })
}

#[cfg(test)]
#[path = "mapper_test.rs"]
mod mapper_test;
