use authz_resolver_sdk::pep::EnforcerError;
use toolkit_db::DbError;
use uuid::Uuid;

/// Domain-level errors for the Model Registry gear.
///
/// Each variant maps to a corresponding [`crate::ModelRegistryError`] via the
/// [`From`] impl, providing a clean domain-internal error type that the
/// repository and service layers use. The SDK error type is only constructed
/// at the `LocalClient` boundary.
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    /// Model not found by canonical ID.
    #[error("model not found: {canonical_id}")]
    ModelNotFound { canonical_id: String },

    /// Provider not found by ID.
    #[error("provider not found: {id}")]
    ProviderNotFound { id: Uuid },

    /// Provider not found by slug.
    #[error("provider with slug `{slug}` not found")]
    ProviderNotFoundBySlug { slug: String },

    /// Model is deprecated (only when fetched directly by `canonical_id`).
    #[error("model deprecated: {canonical_id}")]
    ModelDeprecated { canonical_id: String },

    /// Model is not approved for the tenant.
    #[error("model not approved: {canonical_id}")]
    ModelNotApproved { canonical_id: String },

    /// Access denied.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// Provider is disabled.
    #[error("provider disabled: {id}")]
    ProviderDisabled { id: Uuid },

    /// Provider slug already exists (unique constraint).
    #[error("provider slug already exists: {slug}")]
    ProviderConflict { slug: String },

    /// Provider has existing models that must be removed first.
    #[error("provider has existing models: cannot delete provider with {model_count} model(s)")]
    ProviderHasModels { id: Uuid, model_count: u64 },

    /// Invalid state transition.
    #[error("invalid state transition: {detail}")]
    InvalidTransition { detail: String },

    /// Validation error.
    #[error("validation error: {message}")]
    Validation { message: String },

    /// Internal / unexpected failure.
    #[error("internal error: {detail}")]
    Internal {
        detail: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },

    /// Database error.
    #[error("database error: {0}")]
    Database(#[from] DbError),
}

impl DomainError {
    #[must_use]
    pub fn model_not_found(canonical_id: impl Into<String>) -> Self {
        Self::ModelNotFound {
            canonical_id: canonical_id.into(),
        }
    }

    #[must_use]
    pub fn provider_not_found(id: Uuid) -> Self {
        Self::ProviderNotFound { id }
    }

    #[must_use]
    pub fn provider_not_found_by_slug(slug: impl Into<String>) -> Self {
        Self::ProviderNotFoundBySlug { slug: slug.into() }
    }

    #[must_use]
    pub fn model_deprecated(canonical_id: impl Into<String>) -> Self {
        Self::ModelDeprecated {
            canonical_id: canonical_id.into(),
        }
    }

    #[must_use]
    pub fn model_not_approved(canonical_id: impl Into<String>) -> Self {
        Self::ModelNotApproved {
            canonical_id: canonical_id.into(),
        }
    }

    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    #[must_use]
    pub fn provider_disabled(id: Uuid) -> Self {
        Self::ProviderDisabled { id }
    }

    #[must_use]
    pub fn provider_conflict(slug: impl Into<String>) -> Self {
        Self::ProviderConflict { slug: slug.into() }
    }

    #[must_use]
    pub fn provider_has_models(id: Uuid, model_count: u64) -> Self {
        Self::ProviderHasModels { id, model_count }
    }

    #[must_use]
    pub fn invalid_transition(detail: impl Into<String>) -> Self {
        Self::InvalidTransition {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    /// Construct an `Internal` error with a detail string and no source.
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
            source: None,
        }
    }

    /// Construct an `Internal` error wrapping an upstream error.
    pub fn internal_from(
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Internal {
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }
}

// ---------------------------------------------------------------------------
// From<DomainError> for ModelRegistryError — maps every domain variant to the
// corresponding SDK error variant.
// ---------------------------------------------------------------------------

impl From<DomainError> for crate::ModelRegistryError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::ModelNotFound { canonical_id } => Self::model_not_found(canonical_id),
            DomainError::ProviderNotFound { id } => Self::provider_not_found(id),
            DomainError::ProviderNotFoundBySlug { slug } => Self::provider_not_found_by_slug(slug),
            DomainError::ModelDeprecated { canonical_id } => Self::model_deprecated(canonical_id),
            DomainError::ModelNotApproved { canonical_id } => {
                Self::model_not_approved(canonical_id)
            }
            DomainError::Forbidden(msg) => Self::forbidden(msg),
            DomainError::ProviderDisabled { id } => Self::provider_disabled(id),
            DomainError::ProviderConflict { slug } => Self::provider_conflict(slug),
            DomainError::ProviderHasModels { id, model_count } => { Self::provider_has_models(id, model_count) }
            DomainError::InvalidTransition { detail } => Self::invalid_transition(detail),
            DomainError::Validation { message } => Self::validation(message),
            DomainError::Internal { detail, source } => Self::Internal { detail, source },
            DomainError::Database(db_err) => Self::internal(db_err.to_string()),
        }
    }
}

impl From<EnforcerError> for DomainError {
    fn from(e: EnforcerError) -> Self {
        match e {
            EnforcerError::Denied { deny_reason } => {
                let msg = deny_reason.as_ref().map_or_else(
                    || "access denied".to_owned(),
                    |r| format!("access denied: {r:?}"),
                );
                Self::forbidden(msg)
            }
            EnforcerError::EvaluationFailed(err) => {
                Self::internal_from("authorization evaluation failed", err)
            }
            EnforcerError::CompileFailed(err) => {
                Self::internal_from("authorization constraint compilation failed", err)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelRegistryError;

    fn assert_conversion(domain: DomainError, sdk: &ModelRegistryError) {
        let converted: ModelRegistryError = domain.into();
        assert_eq!(converted.to_string(), sdk.to_string());
    }

    #[test]
    fn model_not_found_converts() {
        let domain = DomainError::model_not_found("openai::gpt-4o");
        let sdk = ModelRegistryError::model_not_found("openai::gpt-4o");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn provider_not_found_converts() {
        let id = Uuid::new_v4();
        let domain = DomainError::provider_not_found(id);
        let sdk = ModelRegistryError::provider_not_found(id);
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn provider_not_found_by_slug_converts() {
        let domain = DomainError::provider_not_found_by_slug("openai");
        let sdk = ModelRegistryError::provider_not_found_by_slug("openai");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn model_deprecated_converts() {
        let domain = DomainError::model_deprecated("openai::gpt-4o");
        let sdk = ModelRegistryError::model_deprecated("openai::gpt-4o");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn model_not_approved_converts() {
        let domain = DomainError::model_not_approved("openai::gpt-4o");
        let sdk = ModelRegistryError::model_not_approved("openai::gpt-4o");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn forbidden_converts() {
        let domain = DomainError::forbidden("no access");
        let sdk = ModelRegistryError::forbidden("no access");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn provider_disabled_converts() {
        let id = Uuid::new_v4();
        let domain = DomainError::provider_disabled(id);
        let sdk = ModelRegistryError::provider_disabled(id);
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn provider_conflict_converts() {
        let domain = DomainError::provider_conflict("openai");
        let sdk = ModelRegistryError::provider_conflict("openai");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn invalid_transition_converts() {
        let domain = DomainError::invalid_transition("cannot deprecate from preview");
        let sdk = ModelRegistryError::invalid_transition("cannot deprecate from preview");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn validation_converts() {
        let domain = DomainError::validation("slug cannot be empty");
        let sdk = ModelRegistryError::validation("slug cannot be empty");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn internal_without_source_converts() {
        let domain = DomainError::internal("db pool exhausted");
        let sdk = ModelRegistryError::internal("db pool exhausted");
        assert_conversion(domain, &sdk);
    }

    #[test]
    fn internal_with_source_converts() {
        let upstream = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "rst");
        let domain = DomainError::internal_from("oagw call failed", upstream);
        match domain {
            DomainError::Internal { detail, source } => {
                assert_eq!(detail, "oagw call failed");
                assert!(source.is_some());
                assert!(source.unwrap().to_string().contains("rst"));
            }
            other => panic!("expected Internal variant, got {other:?}"),
        }
    }

    #[test]
    fn internal_with_source_converts_to_sdk() {
        let upstream = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "rst");
        let domain = DomainError::internal_from("oagw call failed", upstream);
        let sdk: ModelRegistryError = domain.into();
        assert_eq!(sdk.to_string(), "internal error: oagw call failed");
        let source = std::error::Error::source(&sdk).expect("source preserved");
        assert!(source.to_string().contains("rst"));
    }

    #[test]
    fn database_converts_to_internal_sdk() {
        // We can't easily construct a DbError in tests without the full db
        // infrastructure. Instead test that Database() converts to Internal.
        let domain = DomainError::Database(DbError::UnknownDsn("test".into()));
        let sdk: ModelRegistryError = domain.into();
        assert!(sdk.to_string().contains("internal error"));
    }

    #[test]
    fn model_not_found_display() {
        let err = DomainError::model_not_found("openai::gpt-4o");
        assert_eq!(err.to_string(), "model not found: openai::gpt-4o");
    }

    #[test]
    fn provider_not_found_display() {
        let id = Uuid::nil();
        let err = DomainError::provider_not_found(id);
        assert_eq!(err.to_string(), format!("provider not found: {id}"));
    }

    #[test]
    fn provider_not_found_by_slug_display() {
        let err = DomainError::provider_not_found_by_slug("openai");
        assert_eq!(
            err.to_string(),
            "provider with slug `openai` not found"
        );
    }

    #[test]
    fn validation_display() {
        let err = DomainError::validation("invalid slug");
        assert_eq!(err.to_string(), "validation error: invalid slug");
    }

    #[test]
    fn forbidden_display() {
        let err = DomainError::forbidden("no access");
        assert_eq!(err.to_string(), "forbidden: no access");
    }

    #[test]
    fn provider_conflict_display() {
        let err = DomainError::provider_conflict("openai");
        assert_eq!(err.to_string(), "provider slug already exists: openai");
    }

    #[test]
    fn invalid_transition_display() {
        let err = DomainError::invalid_transition("bad state");
        assert_eq!(err.to_string(), "invalid state transition: bad state");
    }

    #[test]
    fn provider_has_models_converts_to_sdk() {
        let id = Uuid::new_v4();
        let domain = DomainError::provider_has_models(id, 5);
        let sdk: ModelRegistryError = domain.into();
        assert_eq!(sdk.to_string(), format!("provider has 5 existing model(s): {id}"));
    }
}
