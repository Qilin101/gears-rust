//! Entity ↔ SDK type mappers.
//!
//! Converts between `SeaORM` entities (`provider::Model`, `model::Model`) and
//! their SDK counterparts (`ProviderV1`, `ModelV1`), and provides
//! [`ActiveModel`] builders for create/update operations that project the
//! promoted columns from `ModelInfoV1`.
//!
//! # Storage layout (post-2026-07-24)
//!
//! `ModelInfoV1` is now stored as:
//! - **17 scalar columns** — every field that promotes cleanly (one column per
//!   leaf field, including nested leaves such as `performance.response_latency_ms`).
//! - **5 JSONB sub-object columns** — `capabilities_full` (everything in
//!   `ModelCapabilities` minus the 4 `OData` booleans), `default_parameters`
//!   (`DefaultInferenceParametersV1`), `additional_info` (the
//!   `HashMap<String, Value>` escape hatch), `disabled_capabilities_full`
//!   (the `DisabledCapabilities` mirror), and `allow_extra_params` (a
//!   `Vec<String>` of caller-supplied parameter names).
//! - **`provider_settings`** — the polymorphic JSONB column (kept; discriminated
//!   by `gts_type`).
//!
//! The previous `info` JSONB column has been dropped entirely; scalar columns
//! are now the source of truth. The denormalized OData-filterable columns
//! (15 fields including 4 capability bools) are projections of the corresponding
//! `ModelInfoV1` fields maintained on every create/update.
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
use tracing;
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
    // Single timestamp for both created_at and updated_at so they match exactly.
    let now = chrono::Utc::now();
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
        discovery_interval_seconds: Set(req.discovery_interval_seconds().map(i64::from)),
        created_at: Set(now),
        updated_at: Set(now),
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
        active.discovery_interval_seconds = Set(interval.map(i64::from));
    }

    // Only bump `updated_at` when at least one field was actually set.
    let changed = req.name.is_some()
        || req.status.is_some()
        || req.managed.is_some()
        || req.metadata.is_some()
        || req.discovery_enabled.is_some()
        || req.discovery_interval_seconds.is_some();
    if changed {
        active.updated_at = Set(chrono::Utc::now());
    }
    active
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model conversions
// ═══════════════════════════════════════════════════════════════════════════════

/// Convert a `model::Model` entity to `ModelV1<serde_json::Value>`.
///
/// Builds a JSON value from the new promoted scalar columns + the four JSONB
/// sub-object columns + the polymorphic `provider_settings` JSONB, then
/// deserializes via `serde_json::from_value::<ModelInfoV1>(value)`. Falls back
/// to a minimal reconstruction from denormalized columns if any required field
/// is missing (graceful degradation — should rarely trigger now that the
/// migration enforces `NOT NULL DEFAULT`s on the required columns).
#[must_use]
#[allow(clippy::expect_used)]
pub fn model_entity_to_v1(e: &entity::model::Model) -> ModelV1 {
    let lifecycle_status = e.lifecycle_status.clone();
    let approval_status = e.approval_status.clone();

    let info = build_model_info_v1(e);

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

/// Build a `ModelInfoV1` JSON value by stitching the 17 promoted scalar
/// columns + 4 JSONB sub-object columns + the polymorphic `provider_settings`
/// JSONB together, then deserialize via JSON roundtrip.
///
/// Falls back to a minimal reconstruction when required columns are absent
/// (legacy rows, partial inserts, or migration in-flight).
#[must_use]
fn build_model_info_v1(e: &entity::model::Model) -> ModelInfoV1 {
    // If `gts_type` (the discriminator) or `provider_model_id` are missing, we
    // can't deserialize a meaningful `ModelInfoV1`. Trigger the graceful
    // fallback.
    if e.gts_type.is_none() || e.provider_model_id.is_none() {
        return build_minimal_info(e);
    }

    let capabilities = build_capabilities(e);

    let value = json!({
        "gts_type": e.gts_type.as_deref().unwrap_or(""),
        "display_name": e.display_name,
        "description": e.description,
        "family": e.family,
        "vendor": e.vendor,
        "managed": e.managed,
        "architecture": e.architecture,
        "size_bytes": e.size_bytes,
        "format": e.format,
        "region": e.region,
        "hosted_by": e.hosted_by,
        "last_release_at": e.last_release_at,
        "reasoning_level": e.reasoning_level,
        "version": e.version,
        "sort_order": e.sort_order,
        "icon": e.icon,
        "multiplier_display": e.multiplier_display,
        "performance": {
            "response_latency_ms": e.perf_response_latency_ms,
            "tokens_per_second": e.perf_tokens_per_second,
        },
        "additional_info": e.additional_info.clone().unwrap_or_else(|| json!({})),
        "supported_api": supported_api_denorm_to_json_array(e.supported_api.as_deref()),
        "provider_model_id": e.provider_model_id.as_deref().unwrap_or(""),
        "capabilities": capabilities,
        "disabled_capabilities": merge_disabled_capabilities(e.disabled_capabilities_full.as_ref()),
        "context_window": {
            "max_input_tokens": e.ctx_max_input_tokens,
            "max_output_tokens": e.ctx_max_output_tokens,
            "output_vector_size": e.ctx_output_vector_size,
        },
        "default_parameters": merge_default_parameters(e.default_parameters.as_ref()),
        "allow_parameter_override": e.allow_parameter_override,
        "allow_extra_params": e.allow_extra_params.clone().unwrap_or_else(|| json!([])),
        "provider_settings": e.provider_settings.clone().unwrap_or(serde_json::Value::Null),
    });
    // SAFETY: the JSON value above is constructed inline from trusted entity
    // fields whose serde representations are known. A panic here is a
    // programming error — e.g. a new field was added to `ModelInfoV1` but the
    // builder wasn't updated.
    serde_json::from_value(value).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            "ModelInfoV1 roundtrip from promoted columns failed, using fallback"
        );
        build_minimal_info(e)
    })
}

/// Build a `ModelCapabilities` JSON value by merging the 4 promoted scalar
/// capability booleans (`cap_vision`, `cap_function_calling`, `cap_streaming`,
/// `cap_reasoning_effort`) with the rest of the capability content from the
/// `capabilities_full` JSONB sub-object column.
///
/// **The 4 scalar columns are authoritative** — they override whatever may be
/// stored in `capabilities_full.vision.enabled` etc. The remaining fields
/// (mime types, `response_schema`, `file_input`, `image_generation`, `audio_*`,
/// `code_interpreter`, `web_search`, reasoning toggle/resume/budget) come from
/// the JSONB column. If the JSONB column is missing, sensible defaults are
/// filled in for all required fields.
#[must_use]
fn build_capabilities(e: &entity::model::Model) -> serde_json::Value {
    // Parse the capabilities_full JSONB content (the "rest" of capabilities).
    // If missing or malformed, start from an empty object so the scalar bools
    // fill in the rest.
    let mut caps_obj: serde_json::Map<String, serde_json::Value> = e
        .capabilities_full
        .as_ref()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    // Vision: scalar `cap_vision` overrides `vision.enabled`. Mime types come
    // from the JSONB column (or default to empty).
    let vision_mime_types = caps_obj
        .get("vision")
        .and_then(|v| v.get("supported_mime_types"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    caps_obj.insert(
        "vision".to_owned(),
        json!({
            "enabled": e.cap_vision,
            "supported_mime_types": vision_mime_types,
        }),
    );

    // Reasoning: scalar `cap_reasoning_effort` overrides `reasoning.effort`.
    // toggle/resume/budget come from the JSONB column (or default to false).
    let reasoning_obj = caps_obj
        .get("reasoning")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let mut reasoning = reasoning_obj;
    reasoning.insert("effort".to_owned(), json!(e.cap_reasoning_effort));
    reasoning.entry("toggle".to_owned()).or_insert(json!(false));
    reasoning.entry("resume".to_owned()).or_insert(json!(false));
    reasoning.entry("budget".to_owned()).or_insert(json!(false));
    caps_obj.insert("reasoning".to_owned(), serde_json::Value::Object(reasoning));

    // Scalar bools for function_calling and streaming override JSONB content.
    caps_obj.insert("function_calling".to_owned(), json!(e.cap_function_calling));
    caps_obj.insert("streaming".to_owned(), json!(e.cap_streaming));

    // Default-fill remaining fields that are required by `ModelCapabilities`.
    caps_obj
        .entry("response_schema".to_owned())
        .or_insert(json!(false));
    caps_obj
        .entry("file_input".to_owned())
        .or_insert(json!({ "enabled": false, "supported_mime_types": [] }));
    caps_obj
        .entry("image_generation".to_owned())
        .or_insert(json!({ "enabled": false, "supported_mime_types": [] }));
    caps_obj
        .entry("audio_input".to_owned())
        .or_insert(json!({ "enabled": false, "supported_mime_types": [] }));
    caps_obj
        .entry("audio_output".to_owned())
        .or_insert(json!({ "enabled": false, "supported_mime_types": [] }));
    caps_obj
        .entry("code_interpreter".to_owned())
        .or_insert(json!(false));
    caps_obj.entry("web_search".to_owned()).or_insert(json!({
        "enabled": false,
        "allowed_domains": false,
        "excluded_domains": false
    }));

    serde_json::Value::Object(caps_obj)
}

/// Return a `Default` `DisabledCapabilities` JSON object (all flags false,
/// all lists empty). Used when `disabled_capabilities_full` is NULL.
#[must_use]
fn default_disabled_capabilities_value() -> serde_json::Value {
    json!({
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
    })
}

/// Return a `Default` `DefaultInferenceParametersV1` JSON object (all fields
/// null). Used when `default_parameters` is NULL.
#[must_use]
fn default_inference_parameters_value() -> serde_json::Value {
    json!({
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
    })
}

/// Merge a stored `disabled_capabilities_full` JSONB value with the default
/// `DisabledCapabilities` shape. Ensures all required fields (`reasoning`,
/// `vision`, etc.) are present even if the stored JSON omits them.
#[must_use]
fn merge_disabled_capabilities(stored: Option<&serde_json::Value>) -> serde_json::Value {
    let defaults = default_disabled_capabilities_value();
    let defaults_obj = defaults.as_object().cloned().unwrap_or_default();
    let mut stored_obj = stored
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    // For nested objects (vision, reasoning, file_input, etc.), merge field
    // by field so missing fields get default values.
    let nested_keys = [
        "vision",
        "reasoning",
        "file_input",
        "image_generation",
        "audio_input",
        "audio_output",
        "web_search",
    ];
    for key in nested_keys {
        if let Some(default_nested) = defaults_obj.get(key).and_then(|v| v.as_object()) {
            let merged = match stored_obj.get(key).and_then(|v| v.as_object().cloned()) {
                Some(mut s) => {
                    for (k, v) in default_nested {
                        s.entry(k.clone()).or_insert(v.clone());
                    }
                    s
                }
                None => default_nested.clone(),
            };
            stored_obj.insert(key.to_owned(), serde_json::Value::Object(merged));
        }
    }
    // Scalar booleans default to false
    for key in [
        "function_calling",
        "response_schema",
        "streaming",
        "code_interpreter",
    ] {
        stored_obj.entry(key.to_owned()).or_insert(json!(false));
    }

    serde_json::Value::Object(stored_obj)
}

/// Merge a stored `default_parameters` JSONB value with the default
/// `DefaultInferenceParametersV1` shape. Ensures all optional fields are
/// present (defaulting to null) even if the stored JSON omits them.
#[must_use]
fn merge_default_parameters(stored: Option<&serde_json::Value>) -> serde_json::Value {
    let defaults = default_inference_parameters_value();
    let defaults_obj = defaults.as_object().cloned().unwrap_or_default();
    let mut stored_obj = stored
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    for (k, v) in defaults_obj {
        stored_obj.entry(k).or_insert(v);
    }

    serde_json::Value::Object(stored_obj)
}

/// Build a `model::ActiveModel` from a create request.
///
/// Projects every `ModelInfoV1` field into the corresponding promoted column
/// (17 scalar + 5 JSONB sub-objects + the polymorphic `provider_settings`),
/// derives `canonical_id` as `{provider_slug}::{info.provider_model_id}`.
#[allow(clippy::expect_used)]
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
    let provider_settings_json = serde_json::to_value(&req.info.provider_settings)
        .expect("CreateModelRequestV1.info.provider_settings re-serialization cannot fail");

    // Single timestamp for both created_at and updated_at so they match exactly.
    let now = chrono::Utc::now();

    let cap_full = build_capabilities_full_for_create(&req.info.capabilities);
    let disabled_full = serde_json::to_value(&req.info.disabled_capabilities)
        .expect("disabled_capabilities re-serialization cannot fail");
    let default_params = serde_json::to_value(&req.info.default_parameters)
        .expect("default_parameters re-serialization cannot fail");
    let additional_info = serde_json::to_value(&req.info.additional_info)
        .expect("additional_info re-serialization cannot fail");
    let allow_extra_params = serde_json::to_value(&req.info.allow_extra_params)
        .expect("allow_extra_params re-serialization cannot fail");

    entity::model::ActiveModel {
        id: Set(Uuid::new_v4()),
        provider_id: Set(provider_id),
        tenant_id: Set(tenant_id),
        canonical_id: Set(canonical_id),
        lifecycle_status: Set(lifecycle_status_str(req.lifecycle_status)),
        deprecated_at: Set(None),
        provider_settings: Set(if provider_settings_json.is_null() {
            None
        } else {
            Some(provider_settings_json)
        }),
        created_at: Set(now),
        updated_at: Set(now),
        // 17 promoted scalar columns
        display_name: Set(req.info.display_name.clone()),
        description: Set(req.info.description.clone()),
        size_bytes: Set(req
            .info
            .size_bytes
            .map(i64::try_from)
            .transpose()
            .ok()
            .flatten()),
        region: Set(req.info.region.clone()),
        hosted_by: Set(req.info.hosted_by.clone()),
        last_release_at: Set(req.info.last_release_at),
        reasoning_level: Set(req.info.reasoning_level.clone()),
        version: Set(req.info.version.clone()),
        sort_order: Set(req.info.sort_order.map(i64::from)),
        icon: Set(req.info.icon.clone()),
        multiplier_display: Set(req.info.multiplier_display.clone()),
        perf_response_latency_ms: Set(req.info.performance.response_latency_ms.map(i64::from)),
        perf_tokens_per_second: Set(req.info.performance.tokens_per_second.map(i64::from)),
        ctx_max_input_tokens: Set(i64::from(req.info.context_window.max_input_tokens)),
        ctx_max_output_tokens: Set(req.info.context_window.max_output_tokens.map(i64::from)),
        ctx_output_vector_size: Set(req.info.context_window.output_vector_size.map(i64::from)),
        allow_parameter_override: Set(req.info.allow_parameter_override),
        // 5 JSONB sub-object columns
        capabilities_full: Set(Some(cap_full)),
        default_parameters: Set(Some(default_params)),
        additional_info: Set(Some(additional_info)),
        disabled_capabilities_full: Set(Some(disabled_full)),
        allow_extra_params: Set(Some(allow_extra_params)),
        // Denormalized columns (15 existing OData filter surface)
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

/// Build the `capabilities_full` JSONB sub-object from a `ModelCapabilities`
/// value — strips the 4 scalar-OData booleans so the columns remain the
/// authoritative source for `vision.enabled`, `function_calling`,
/// `streaming`, and `reasoning.effort`.
#[must_use]
#[allow(clippy::expect_used)]
fn build_capabilities_full_for_create(
    caps: &model_registry_sdk::models::ModelCapabilities,
) -> serde_json::Value {
    // Serialize the full struct, then strip the 4 promoted fields so the
    // JSONB column matches the "rest of capabilities" contract.
    let mut value =
        serde_json::to_value(caps).expect("ModelCapabilities re-serialization cannot fail");
    if let Some(obj) = value.as_object_mut() {
        obj.remove("function_calling");
        obj.remove("streaming");
        if let Some(vision) = obj.get_mut("vision").and_then(|v| v.as_object_mut()) {
            vision.remove("enabled");
        }
        if let Some(reasoning) = obj.get_mut("reasoning").and_then(|v| v.as_object_mut()) {
            reasoning.remove("effort");
        }
    }
    value
}

/// Build a `model::ActiveModel` for a PATCH update.
///
/// Reads the existing entity, reconstructs `ModelInfoV1` from the new column
/// layout, applies only the `Some(...)` patches from the request, and
/// re-projects every promoted column.
///
/// `approval_status` is patched in place alongside the other fields when
/// `req.approval_status` is `Some(...)`.
///
/// Immutable fields (`canonical_id`, `provider_slug`, `info.provider_model_id`,
/// `info.gts_type`) are silently ignored if present in the request.
#[allow(clippy::cognitive_complexity)]
#[allow(clippy::expect_used)]
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
    if let Some(approval) = req.approval_status {
        active.approval_status = Set(approval_status_str(approval));
    }

    // Reconstruct ModelInfoV1 from the existing promoted columns, apply
    // patches, and re-project every column back. We use the same
    // `model_entity_to_v1` round-trip used by the read path so the patch
    // logic is symmetric.
    let read_back = model_entity_to_v1(existing);
    let mut fresh = read_back.info;
    let info_changed = apply_info_patches(&mut fresh, req);

    if info_changed {
        let info_inner = &fresh;

        // 17 promoted scalar columns
        active.display_name = Set(info_inner.display_name.clone());
        active.description = Set(info_inner.description.clone());
        active.size_bytes = Set(info_inner
            .size_bytes
            .map(i64::try_from)
            .transpose()
            .ok()
            .flatten());
        active.region = Set(info_inner.region.clone());
        active.hosted_by = Set(info_inner.hosted_by.clone());
        active.last_release_at = Set(info_inner.last_release_at);
        active.reasoning_level = Set(info_inner.reasoning_level.clone());
        active.version = Set(info_inner.version.clone());
        active.sort_order = Set(info_inner.sort_order.map(i64::from));
        active.icon = Set(info_inner.icon.clone());
        active.multiplier_display = Set(info_inner.multiplier_display.clone());
        active.perf_response_latency_ms =
            Set(info_inner.performance.response_latency_ms.map(i64::from));
        active.perf_tokens_per_second =
            Set(info_inner.performance.tokens_per_second.map(i64::from));
        active.ctx_max_input_tokens = Set(i64::from(info_inner.context_window.max_input_tokens));
        active.ctx_max_output_tokens =
            Set(info_inner.context_window.max_output_tokens.map(i64::from));
        active.ctx_output_vector_size =
            Set(info_inner.context_window.output_vector_size.map(i64::from));
        active.allow_parameter_override = Set(info_inner.allow_parameter_override);

        // 5 JSONB sub-object columns
        // `capabilities_full` strips the 4 promoted booleans so the columns
        // remain authoritative (consistent with `build_capabilities_full_for_create`
        // used on the create path).
        let cap_full = build_capabilities_full_for_create(&info_inner.capabilities);
        #[allow(clippy::expect_used)]
        let disabled_full = serde_json::to_value(&info_inner.disabled_capabilities)
            .expect("DisabledCapabilities re-serialization cannot fail");
        #[allow(clippy::expect_used)]
        let default_params = serde_json::to_value(&info_inner.default_parameters)
            .expect("DefaultInferenceParametersV1 re-serialization cannot fail");
        #[allow(clippy::expect_used)]
        let additional_info = serde_json::to_value(&info_inner.additional_info)
            .expect("additional_info re-serialization cannot fail");
        #[allow(clippy::expect_used)]
        let allow_extra_params = serde_json::to_value(&info_inner.allow_extra_params)
            .expect("allow_extra_params re-serialization cannot fail");
        active.capabilities_full = Set(Some(cap_full));
        active.default_parameters = Set(Some(default_params));
        active.additional_info = Set(Some(additional_info));
        active.disabled_capabilities_full = Set(Some(disabled_full));
        active.allow_extra_params = Set(Some(allow_extra_params));

        // Re-extract provider_settings
        #[allow(clippy::expect_used)]
        let ps_json = serde_json::to_value(&info_inner.provider_settings)
            .expect("provider_settings re-serialization cannot fail");
        active.provider_settings = Set(if ps_json.is_null() {
            None
        } else {
            Some(ps_json)
        });

        // Re-project denormalized columns
        active.gts_type = Set(Some(info_inner.gts_type.to_string()));
        active.vendor = Set(info_inner.vendor.clone());
        active.family = Set(info_inner.family.clone());
        active.managed = Set(info_inner.managed);
        active.architecture = Set(info_inner.architecture.clone());
        active.format = Set(info_inner.format.clone());
        active.provider_model_id = Set(Some(info_inner.provider_model_id.clone()));
        active.supported_api = Set(supported_api_set_to_str(&info_inner.supported_api));
        active.cap_vision = Set(info_inner.capabilities.vision.enabled);
        active.cap_function_calling = Set(info_inner.capabilities.function_calling);
        active.cap_streaming = Set(info_inner.capabilities.streaming);
        active.cap_reasoning_effort = Set(info_inner.capabilities.reasoning.effort);
    }

    // Only bump `updated_at` when at least one field was actually set.
    let changed = req.lifecycle_status.is_some() || req.approval_status.is_some() || info_changed;
    if changed {
        active.updated_at = Set(chrono::Utc::now());
    }
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
fn provider_status_str(status: ProviderStatus) -> String {
    match status {
        ProviderStatus::Active => "active".to_owned(),
        ProviderStatus::Disabled => "disabled".to_owned(),
        _ => {
            tracing::error!(
                ?status,
                "unknown ProviderStatus variant, defaulting to active"
            );
            "active".to_owned()
        }
    }
}

/// Convert a `LifecycleStatus` to its lowercase storage string.
#[must_use]
fn lifecycle_status_str(status: LifecycleStatus) -> String {
    match status {
        LifecycleStatus::Production => "production".to_owned(),
        LifecycleStatus::Preview => "preview".to_owned(),
        LifecycleStatus::Experimental => "experimental".to_owned(),
        LifecycleStatus::Deprecated => "deprecated".to_owned(),
        LifecycleStatus::Sunset => "sunset".to_owned(),
        _ => {
            tracing::error!(
                ?status,
                "unknown LifecycleStatus variant, defaulting to production"
            );
            "production".to_owned()
        }
    }
}

/// Convert an `ApprovalStatus` to its lowercase storage string.
#[must_use]
fn approval_status_str(status: ApprovalStatus) -> String {
    match status {
        ApprovalStatus::Pending => "pending".to_owned(),
        ApprovalStatus::Approved => "approved".to_owned(),
        ApprovalStatus::Rejected => "rejected".to_owned(),
        ApprovalStatus::Revoked => "revoked".to_owned(),
        _ => {
            tracing::error!(
                ?status,
                "unknown ApprovalStatus variant, defaulting to pending"
            );
            "pending".to_owned()
        }
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
        _ => {
            tracing::error!(?api, "unknown SupportedApi variant, defaulting to unknown");
            "unknown".to_owned()
        }
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

/// Build a minimal `ModelInfoV1` from the denormalized columns when the
/// required discriminator fields (`gts_type`, `provider_model_id`) are
/// missing or corrupt on the row.
///
/// This is the **graceful-degradation path** — with the post-2026-07-24
/// schema, every `ModelInfoV1` field lives in either a scalar column or a
/// small JSONB sub-object, so the regular read path in
/// [`build_model_info_v1`] is authoritative. This fallback is only invoked
/// when `gts_type` / `provider_model_id` are NULL (legacy rows, raw SQL
/// inserts that bypassed the application layer, or in-flight migration).
///
/// Required scalar columns (`display_name`, `ctx_max_input_tokens`,
/// `allow_parameter_override`) have `NOT NULL DEFAULT`s at the DB level, so
/// they are always populated. Only the denormalized filterable columns
/// (`gts_type`, `provider_model_id`) remain nullable.
#[must_use]
fn build_minimal_info(e: &entity::model::Model) -> ModelInfoV1 {
    // Since ModelInfoV1 and its inner types are #[non_exhaustive], construct via
    // JSON roundtrip.
    let gts_type_str = e
        .gts_type
        .as_deref()
        .unwrap_or("gts.cf.genai.model.info.v1~");
    let display_name = if e.display_name.is_empty() {
        format!("model-{}", e.canonical_id)
    } else {
        e.display_name.clone()
    };

    let value = json!({
        "gts_type": gts_type_str,
        "display_name": display_name,
        "description": e.description,
        "family": e.family,
        "vendor": e.vendor,
        "managed": e.managed,
        "architecture": e.architecture,
        "size_bytes": null,
        "format": e.format,
        "region": e.region,
        "hosted_by": e.hosted_by,
        "last_release_at": e.last_release_at,
        "reasoning_level": e.reasoning_level,
        "version": e.version,
        "sort_order": e.sort_order,
        "icon": e.icon,
        "multiplier_display": e.multiplier_display,
        "performance": {
            "response_latency_ms": e.perf_response_latency_ms,
            "tokens_per_second": e.perf_tokens_per_second
        },
        "additional_info": e.additional_info.clone().unwrap_or_else(|| json!({})),
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
            "max_input_tokens": e.ctx_max_input_tokens,
            "max_output_tokens": e.ctx_max_output_tokens,
            "output_vector_size": e.ctx_output_vector_size
        },
        "default_parameters": merge_default_parameters(e.default_parameters.as_ref()),
        "allow_parameter_override": e.allow_parameter_override,
        "allow_extra_params": e.allow_extra_params.clone().unwrap_or_else(|| json!([])),
        "provider_settings": e.provider_settings.clone().unwrap_or(serde_json::Value::Null),
    });
    // SAFETY: the JSON template above is constructed from entity fields with
    // known types. A failure here indicates a programming error (e.g. a new
    // field was added to `ModelInfoV1` but the fallback wasn't updated).
    // Use a single defensive fallback to the bare identity fields rather
    // than a chain of nested unwraps — DB defaults guarantee the required
    // columns are populated.
    serde_json::from_value(value).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            canonical_id = %e.canonical_id,
            "ModelInfoV1 roundtrip from denormalized columns failed, using minimal fallback"
        );
        // Bare-bones fallback: only the two identity fields. Construct via a
        // separate JSON value to give `serde_json::from_value` another chance
        // to produce a valid `ModelInfoV1`. The remaining fields use their
        // serde defaults (empty strings / null / empty Vec / etc.).
        serde_json::from_value(json!({
            "gts_type": gts_type_str,
            "display_name": display_name,
        }))
        .unwrap_or_else(|fallback_err| {
            // Last resort — if even the bare identity fields fail to
            // deserialize, the schema is fundamentally incompatible. This
            // should never happen with the SDK types; panic so we notice.
            tracing::error!(
                error = %fallback_err,
                "all ModelInfoV1 fallback attempts failed - this is a programming bug"
            );
            panic!("ModelInfoV1 is no longer constructible from a minimal JSON object")
        })
    })
}

#[cfg(test)]
#[path = "mapper_test.rs"]
mod mapper_test;
