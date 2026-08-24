//! `provider::Model` ↔ [`ProviderV1`] mapper.
//!
//! Converts the `providers` entity to its SDK counterpart and provides
//! [`ActiveModel`] builders for create/update operations. The model-side
//! equivalents live in [`super::model_mapper`].
//!
//! Every `ProviderV1` field is a real column — there is no promoted-column
//! projection here and no polymorphic blob. `metadata` is carried through as an
//! opaque `Option<serde_json::Value>`, so this module needs none of the JSONB
//! codecs the model mapper relies on.
//!
//! # Immutability enforcement
//!
//! `slug` is immutable: changing it would invalidate every
//! `canonical_id = {provider_slug}::{provider_model_id}` referencing it. This is
//! enforced structurally rather than by a check — `UpdateProviderRequestV1` has
//! no `slug` field, so [`provider_update_active_model`] has nothing to write.
//!
//! # Read path is fallible
//!
//! [`ProviderV1`] is built with a struct literal, so adding a field is a compile
//! error here rather than a runtime failure. The single way a row can fail to
//! lift into the SDK type — an out-of-domain `status` string — surfaces as
//! [`DomainError::Internal`] instead of a panic.

use model_registry_sdk::models::{
    CreateProviderRequestV1, ProviderStatus, ProviderV1, UpdateProviderRequestV1,
};
use sea_orm::Set;
use uuid::Uuid;

use crate::domain::error::DomainError;

use super::entity;

// ═══════════════════════════════════════════════════════════════════════════════
// Provider conversions
// ═══════════════════════════════════════════════════════════════════════════════

/// Convert a `provider::Model` entity to [`ProviderV1`].
///
/// # Errors
/// [`DomainError::Internal`] when `status` holds a value outside the
/// [`ProviderStatus`] domain (structurally prevented by the write path;
/// reachable only through legacy / manually-repaired rows).
pub fn provider_entity_to_v1(e: entity::provider::Model) -> Result<ProviderV1, DomainError> {
    let status = ProviderStatus::from_wire(&e.status).ok_or_else(|| {
        DomainError::internal(format!(
            "providers.status out-of-domain value `{}` on provider {}",
            e.status, e.id
        ))
    })?;

    Ok(ProviderV1 {
        id: e.id,
        tenant_id: e.tenant_id,
        slug: e.slug,
        name: e.name,
        gts_type: gts::GtsTypeId::new(&e.gts_type),
        status,
        managed: e.managed,
        metadata: e.metadata,
        discovery_enabled: e.discovery_enabled,
        discovery_interval_seconds: e
            .discovery_interval_seconds
            .and_then(|v| u32::try_from(v).ok()),
        created_at: e.created_at,
        updated_at: e.updated_at,
    })
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
        status: Set(ProviderStatus::Active.as_str().to_owned()),
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
        active.status = Set(status.as_str().to_owned());
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

#[cfg(test)]
#[path = "provider_mapper_test.rs"]
mod provider_mapper_test;
