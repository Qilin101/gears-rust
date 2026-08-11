//! `model::Model` ↔ [`ModelV1`] mapper.
//!
//! Converts the `models` entity to its SDK counterpart and provides
//! [`ActiveModel`] builders for create/update operations that project the
//! promoted columns from `ModelInfoV1`. The provider-side equivalents live in
//! [`super::provider_mapper`].
//!
//! # Storage layout
//!
//! `ModelInfoV1` is stored as:
//! - **Scalar columns** — every field that promotes cleanly (one column per
//!   leaf field, including nested leaves such as `performance.response_latency_ms`).
//! - **JSONB sub-object columns** — `capabilities_full` (the complete
//!   `ModelCapabilities`), `default_parameters` (`DefaultInferenceParametersV1`),
//!   `additional_info` (the `HashMap<String, Value>` escape hatch),
//!   `disabled_capabilities_full` (the `DisabledCapabilities` mirror), and
//!   `allow_extra_params` (a `Vec<String>` of caller-supplied parameter names).
//! - **`provider_settings`** — the polymorphic JSONB column, discriminated by
//!   `gts_type`.
//!
//! The OData-filterable columns are projections of the corresponding
//! `ModelInfoV1` fields, maintained on every create/update. The four capability
//! flags (`cap_vision`, `cap_function_calling`, `cap_streaming`,
//! `cap_reasoning_effort`) are **authoritative on read** — they overwrite
//! whatever `capabilities_full` holds for the same fields.
//!
//! # Immutability enforcement
//!
//! The builders reject attempts to modify immutable identity fields:
//! `canonical_id`, `provider_slug`, `info.provider_model_id`, `info.gts_type`.
//! These are enforced at the mapper level so the repository never sees them.
//!
//! # Read path is fallible
//!
//! The SDK types are built with struct literals, so adding a field to
//! `ModelInfoV1` is a compile error here rather than a runtime failure. The two
//! ways a row can fail to lift into the SDK types — an out-of-domain enum string
//! and an out-of-range `ctx_max_input_tokens` — surface as
//! [`DomainError::Internal`] instead of a panic.

use std::collections::HashSet;

use model_registry_sdk::models::{
    ApprovalStatus, ContextWindow, CreateModelRequestV1, LifecycleStatus, ModelCapabilities,
    ModelInfoV1, ModelPerformance, ModelV1, SupportedApi, UpdateModelRequestV1,
};
use sea_orm::Set;
use uuid::Uuid;

use crate::domain::error::DomainError;

use super::entity;

// ═══════════════════════════════════════════════════════════════════════════════
// Column codecs
// ═══════════════════════════════════════════════════════════════════════════════

/// Decode a JSONB sub-object column into its typed form.
///
/// Takes the column by value so the decode moves strings out of the JSON tree
/// instead of copying them.
///
/// The write path always stores the complete sub-object, so a NULL column or a
/// value the type no longer accepts means the row predates the current shape;
/// both decode to `T::default()`.
fn from_json_column<T>(column: Option<serde_json::Value>) -> T
where
    T: Default + serde::de::DeserializeOwned,
{
    column
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Encode a sub-object into its JSONB column value.
///
/// The SDK types are plain data (no non-string map keys, no I/O), so
/// serialization cannot fail in practice; a failure stores JSON `null` rather
/// than panicking.
fn to_json_column<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// Decode the comma-separated `supported_api` column. Unknown members are
/// dropped — the column is a denormalized filter shadow, not the source of
/// truth for anything else.
fn supported_api_from_csv(csv: Option<&str>) -> HashSet<SupportedApi> {
    csv.map(|s| {
        s.split(',')
            .filter_map(|part| SupportedApi::from_wire(part.trim()))
            .collect()
    })
    .unwrap_or_default()
}

/// Encode a `HashSet<SupportedApi>` as the sorted comma-separated
/// `supported_api` column value. `None` for an empty set.
fn supported_api_to_csv(apis: &HashSet<SupportedApi>) -> Option<String> {
    if apis.is_empty() {
        return None;
    }
    let mut items: Vec<&str> = apis.iter().copied().map(SupportedApi::as_str).collect();
    items.sort_unstable();
    Some(items.join(","))
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model conversions — read path
// ═══════════════════════════════════════════════════════════════════════════════

/// Convert a `model::Model` entity to `ModelV1<serde_json::Value>`.
///
/// # Errors
/// [`DomainError::Internal`] when `lifecycle_status` / `approval_status` hold
/// values outside their enum domain, or when the row fails to lift into
/// [`ModelInfoV1`] (see [`model_entity_to_info_v1`]).
pub fn model_entity_to_v1(e: entity::model::Model) -> Result<ModelV1, DomainError> {
    let lifecycle_status = LifecycleStatus::from_wire(&e.lifecycle_status).ok_or_else(|| {
        DomainError::internal(format!(
            "models.lifecycle_status out-of-domain value `{}` on model {}",
            e.lifecycle_status, e.canonical_id
        ))
    })?;
    let approval_status = ApprovalStatus::from_wire(&e.approval_status).ok_or_else(|| {
        DomainError::internal(format!(
            "models.approval_status out-of-domain value `{}` on model {}",
            e.approval_status, e.canonical_id
        ))
    })?;

    Ok(ModelV1 {
        id: e.id,
        // Cloned rather than moved: `model_entity_to_info_v1` consumes `e` and
        // names `canonical_id` in its diagnostics.
        canonical_id: e.canonical_id.clone(),
        provider_id: e.provider_id,
        lifecycle_status,
        approval_status,
        info: model_entity_to_info_v1(e)?,
    })
}

/// Lift the promoted scalar columns + JSONB sub-object columns + the
/// polymorphic `provider_settings` blob into [`ModelInfoV1`].
///
/// The nullable identity columns (`gts_type`, `provider_model_id`) fall back to
/// the empty string; the three required scalars (`display_name`,
/// `ctx_max_input_tokens`, `allow_parameter_override`) are `NOT NULL DEFAULT`
/// at the DB level and are read verbatim.
///
/// # Errors
/// [`DomainError::Internal`] when `ctx_max_input_tokens` is outside the `u32`
/// range the SDK type carries.
fn model_entity_to_info_v1(e: entity::model::Model) -> Result<ModelInfoV1, DomainError> {
    // `capabilities_full` carries the complete `ModelCapabilities`; the four
    // indexed flags are then overwritten from their authoritative columns.
    let mut capabilities: ModelCapabilities = from_json_column(e.capabilities_full);
    capabilities.vision.enabled = e.cap_vision;
    capabilities.function_calling = e.cap_function_calling;
    capabilities.streaming = e.cap_streaming;
    capabilities.reasoning.effort = e.cap_reasoning_effort;

    let max_input_tokens = u32::try_from(e.ctx_max_input_tokens).map_err(|_| {
        DomainError::internal(format!(
            "models.ctx_max_input_tokens out-of-range value {} on model {}",
            e.ctx_max_input_tokens, e.canonical_id
        ))
    })?;

    Ok(ModelInfoV1 {
        gts_type: gts::GtsTypeId::new(e.gts_type.as_deref().unwrap_or_default()),
        display_name: e.display_name,
        description: e.description,
        family: e.family,
        vendor: e.vendor,
        managed: e.managed,
        architecture: e.architecture,
        size_bytes: e.size_bytes.and_then(|v| u64::try_from(v).ok()),
        format: e.format,
        region: e.region,
        hosted_by: e.hosted_by,
        last_release_at: e.last_release_at,
        reasoning_level: e.reasoning_level,
        version: e.version,
        sort_order: e.sort_order.and_then(|v| i32::try_from(v).ok()),
        icon: e.icon,
        multiplier_display: e.multiplier_display,
        performance: ModelPerformance {
            response_latency_ms: e
                .perf_response_latency_ms
                .and_then(|v| u32::try_from(v).ok()),
            tokens_per_second: e.perf_tokens_per_second.and_then(|v| u32::try_from(v).ok()),
        },
        additional_info: from_json_column(e.additional_info),
        supported_api: supported_api_from_csv(e.supported_api.as_deref()),
        provider_model_id: e.provider_model_id.unwrap_or_default(),
        capabilities,
        disabled_capabilities: from_json_column(e.disabled_capabilities_full),
        context_window: ContextWindow {
            max_input_tokens,
            max_output_tokens: e.ctx_max_output_tokens.and_then(|v| u32::try_from(v).ok()),
            output_vector_size: e.ctx_output_vector_size.and_then(|v| u32::try_from(v).ok()),
        },
        default_parameters: from_json_column(e.default_parameters),
        allow_parameter_override: e.allow_parameter_override,
        allow_extra_params: from_json_column(e.allow_extra_params),
        provider_settings: e.provider_settings.unwrap_or(serde_json::Value::Null),
    })
}

// ═══════════════════════════════════════════════════════════════════════════════
// Model conversions — write path
// ═══════════════════════════════════════════════════════════════════════════════

/// Project every [`ModelInfoV1`] field onto its column: the promoted scalars,
/// the five JSONB sub-objects, the polymorphic `provider_settings`, and the
/// OData-filterable shadows (including the four capability flags).
///
/// The single write projection used by both create and update, so the two
/// paths cannot drift.
fn project_info(info: &ModelInfoV1, am: &mut entity::model::ActiveModel) {
    // — Promoted scalar columns —
    am.display_name = Set(info.display_name.clone());
    am.description = Set(info.description.clone());
    am.size_bytes = Set(info.size_bytes.and_then(|v| i64::try_from(v).ok()));
    am.region = Set(info.region.clone());
    am.hosted_by = Set(info.hosted_by.clone());
    am.last_release_at = Set(info.last_release_at);
    am.reasoning_level = Set(info.reasoning_level.clone());
    am.version = Set(info.version.clone());
    am.sort_order = Set(info.sort_order.map(i64::from));
    am.icon = Set(info.icon.clone());
    am.multiplier_display = Set(info.multiplier_display.clone());
    am.perf_response_latency_ms = Set(info.performance.response_latency_ms.map(i64::from));
    am.perf_tokens_per_second = Set(info.performance.tokens_per_second.map(i64::from));
    am.ctx_max_input_tokens = Set(i64::from(info.context_window.max_input_tokens));
    am.ctx_max_output_tokens = Set(info.context_window.max_output_tokens.map(i64::from));
    am.ctx_output_vector_size = Set(info.context_window.output_vector_size.map(i64::from));
    am.allow_parameter_override = Set(info.allow_parameter_override);

    // — JSONB sub-object columns —
    am.capabilities_full = Set(Some(to_json_column(&info.capabilities)));
    am.default_parameters = Set(Some(to_json_column(&info.default_parameters)));
    am.additional_info = Set(Some(to_json_column(&info.additional_info)));
    am.disabled_capabilities_full = Set(Some(to_json_column(&info.disabled_capabilities)));
    am.allow_extra_params = Set(Some(to_json_column(&info.allow_extra_params)));

    // — Polymorphic provider settings — a JSON `null` payload stores NULL —
    // Already a `serde_json::Value`, so it is cloned rather than re-serialized.
    am.provider_settings =
        Set((!info.provider_settings.is_null()).then(|| info.provider_settings.clone()));

    // — OData-filterable columns —
    am.gts_type = Set(Some(info.gts_type.to_string()));
    am.vendor = Set(info.vendor.clone());
    am.family = Set(info.family.clone());
    am.managed = Set(info.managed);
    am.architecture = Set(info.architecture.clone());
    am.format = Set(info.format.clone());
    am.provider_model_id = Set(Some(info.provider_model_id.clone()));
    am.supported_api = Set(supported_api_to_csv(&info.supported_api));
    am.cap_vision = Set(info.capabilities.vision.enabled);
    am.cap_function_calling = Set(info.capabilities.function_calling);
    am.cap_streaming = Set(info.capabilities.streaming);
    am.cap_reasoning_effort = Set(info.capabilities.reasoning.effort);
}

/// Build a `model::ActiveModel` from a create request.
///
/// Projects every `ModelInfoV1` field into the corresponding promoted column
/// via [`project_info`] and derives `canonical_id` as
/// `{provider_slug}::{info.provider_model_id}`.
#[must_use]
pub fn model_create_active_model(
    tenant_id: Uuid,
    provider_id: Uuid,
    req: &CreateModelRequestV1,
    initial_approval_status: ApprovalStatus,
) -> entity::model::ActiveModel {
    let canonical_id = format!("{}::{}", req.provider_slug, req.info.provider_model_id);
    // Single timestamp for both created_at and updated_at so they match exactly.
    let now = chrono::Utc::now();

    let mut active = entity::model::ActiveModel {
        id: Set(Uuid::new_v4()),
        provider_id: Set(provider_id),
        tenant_id: Set(tenant_id),
        canonical_id: Set(canonical_id),
        lifecycle_status: Set(req.lifecycle_status.as_str().to_owned()),
        deprecated_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        approval_status: Set(initial_approval_status.as_str().to_owned()),
        ..Default::default()
    };
    project_info(&req.info, &mut active);
    active
}

/// Build a `model::ActiveModel` for a PATCH update.
///
/// Reads the existing entity back into `ModelInfoV1`, applies only the
/// `Some(...)` patches from the request, and re-projects every promoted column
/// through [`project_info`].
///
/// `approval_status` is patched in place alongside the other fields when
/// `req.approval_status` is `Some(...)`.
///
/// Immutable fields (`canonical_id`, `provider_slug`, `info.provider_model_id`,
/// `info.gts_type`) are silently ignored if present in the request.
///
/// # Errors
/// [`DomainError::Internal`] when the existing row fails to lift into
/// [`ModelInfoV1`] (see [`model_entity_to_v1`]).
pub fn model_update_active_model(
    existing: &entity::model::Model,
    req: &UpdateModelRequestV1,
) -> Result<entity::model::ActiveModel, DomainError> {
    let mut active: entity::model::ActiveModel = existing.clone().into();

    // — Lifecycle status —
    if let Some(lifecycle) = &req.lifecycle_status {
        active.lifecycle_status = Set(lifecycle.as_str().to_owned());
    }

    // — Approval status —
    if let Some(approval) = req.approval_status {
        active.approval_status = Set(approval.as_str().to_owned());
    }

    // Reconstruct ModelInfoV1 from the existing promoted columns, apply the
    // patches, and re-project every column through the same projection the
    // create path uses.
    let mut info = model_entity_to_info_v1(existing.clone())?;
    let info_changed = apply_info_patches(&mut info, req);
    if info_changed {
        project_info(&info, &mut active);
    }

    // Only bump `updated_at` when at least one field was actually set.
    let changed = req.lifecycle_status.is_some() || req.approval_status.is_some() || info_changed;
    if changed {
        active.updated_at = Set(chrono::Utc::now());
    }
    Ok(active)
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

#[cfg(test)]
#[path = "model_mapper_test.rs"]
mod model_mapper_test;
