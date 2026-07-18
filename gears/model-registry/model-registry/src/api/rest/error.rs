//! Error mapping: [`DomainError`] → [`CanonicalError`] (RFC-9457 Problem).
//!
//! Each [`DomainError`] variant maps to a canonical error category with the
//! appropriate HTTP status code per the Technical Details in the plan.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

/// Resource-scoped error type for model-registry entities.
///
/// Generates builder methods (`not_found`, `already_exists`,
/// `permission_denied`, `invalid_argument`, etc.) via the
/// `#[resource_error]` proc macro.
#[resource_error("gts.cf.gears.model_registry.resource.v1~")]
pub struct ModelRegistryResourceError;

impl From<DomainError> for CanonicalError {
    // Flat match on the domain enum is the whole point of this conversion;
    // the structured `tracing::*!` macros count toward cognitive complexity
    // but splitting the arms into helpers would just hide the mapping.
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        match e {
            // ── 404 Not Found ──────────────────────────────────────────
            DomainError::ModelNotFound { canonical_id } => {
                ModelRegistryResourceError::not_found("Model not found")
                    .with_resource(canonical_id)
                    .create()
            }
            DomainError::ProviderNotFound { id } => {
                ModelRegistryResourceError::not_found("Provider not found")
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::ProviderNotFoundBySlug { slug } => {
                ModelRegistryResourceError::not_found("Provider not found")
                    .with_resource(slug)
                    .create()
            }
            DomainError::ModelDeprecated { canonical_id } => {
                ModelRegistryResourceError::not_found("Model is deprecated")
                    .with_resource(canonical_id)
                    .create()
            }

            // ── 403 Permission Denied ──────────────────────────────────
            DomainError::ModelNotApproved { canonical_id } => {
                ModelRegistryResourceError::permission_denied()
                    .with_reason(format!("model not approved: {canonical_id}"))
                    .create()
            }
            DomainError::Forbidden(msg) => {
                tracing::warn!(msg = %msg, "model-registry access forbidden");
                ModelRegistryResourceError::permission_denied()
                    .with_reason(msg)
                    .create()
            }

            // ── 409 Already Exists / Conflict ──────────────────────────
            DomainError::ProviderConflict { slug } => {
                ModelRegistryResourceError::already_exists("Provider slug already exists")
                    .with_resource(slug)
                    .create()
            }
            DomainError::ProviderDisabled { id } => {
                ModelRegistryResourceError::already_exists("Provider is disabled")
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::InvalidTransition { detail } => {
                ModelRegistryResourceError::already_exists("Invalid state transition")
                    .with_resource(detail)
                    .create()
            }

            // ── 422 Invalid Argument ───────────────────────────────────
            DomainError::Validation { message } => ModelRegistryResourceError::invalid_argument()
                .with_format(message)
                .create(),

            // ── 500 Internal ───────────────────────────────────────────
            DomainError::Internal { detail, source } => {
                tracing::error!(
                    detail = %detail,
                    ?source,
                    "model-registry internal error"
                );
                CanonicalError::internal(detail).create()
            }
            DomainError::Database(db_err) => {
                tracing::error!(error = ?db_err, "model-registry database error");
                CanonicalError::internal(db_err.to_string()).create()
            }
        }
    }
}
