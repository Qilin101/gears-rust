//! Error mapping: [`DomainError`] → [`CanonicalError`] (RFC-9457 Problem).
//!
//! Each [`DomainError`] variant maps to a canonical error category, which
//! fixes both the HTTP status and the problem type. The per-variant table and
//! its rationale live in `docs/DESIGN.md` §4 Error Handling.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

/// Resource-scoped error type for model-registry entities.
///
/// Generates builder methods (`not_found`, `already_exists`,
/// `permission_denied`, `invalid_argument`, etc.) via the
/// `#[resource_error]` proc macro.
#[resource_error("gts.cf.gears.model_registry.resource.v1~")]
pub struct ModelRegistryResourceError;

/// `violations[].subject` discriminator for provider-state preconditions.
const PROVIDER_SUBJECT: &str = "provider";

/// `violations[].subject` discriminator for model-state preconditions.
const MODEL_SUBJECT: &str = "model";

/// `violations[].type` token — provider still owns models on delete.
const PROVIDER_HAS_MODELS_TYPE: &str = "PROVIDER_HAS_MODELS";

/// `violations[].type` token — refused lifecycle/approval transition.
const INVALID_TRANSITION_TYPE: &str = "INVALID_TRANSITION";

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
            DomainError::ModelNotFoundById { id } => {
                ModelRegistryResourceError::not_found("Model not found")
                    .with_resource(id.to_string())
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

            // ── 409 Already Exists ─────────────────────────────────────
            DomainError::ProviderConflict { slug } => {
                ModelRegistryResourceError::already_exists("Provider slug already exists")
                    .with_resource(slug)
                    .create()
            }
            DomainError::ProviderDisabled { id } => ModelRegistryResourceError::permission_denied()
                .with_reason(format!("Provider {id} is disabled"))
                .create(),

            // ── 400 Failed Precondition ────────────────────────────────
            // Both are state guards, not bad arguments: the request is
            // refused because of the row's current state, so the reason
            // travels as a precondition violation rather than a field one.
            DomainError::ProviderHasModels { id, model_count } => {
                // `model_count` is 0 on the FK-violation fallback, where the
                // count is unknown rather than zero.
                let description = if model_count == 0 {
                    "provider still owns models".to_owned()
                } else {
                    format!("provider still owns {model_count} model(s)")
                };
                ModelRegistryResourceError::failed_precondition()
                    .with_resource(id.to_string())
                    .with_precondition_violation(
                        PROVIDER_SUBJECT,
                        description,
                        PROVIDER_HAS_MODELS_TYPE,
                    )
                    .create()
            }
            DomainError::InvalidTransition { detail } => {
                ModelRegistryResourceError::failed_precondition()
                    .with_precondition_violation(MODEL_SUBJECT, detail, INVALID_TRANSITION_TYPE)
                    .create()
            }

            // ── 400 Invalid Argument ───────────────────────────────────
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
