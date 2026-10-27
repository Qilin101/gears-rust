//! Domain errors and their single mapping to `CanonicalError` (DESIGN §4
//! Error Handling).
//!
//! Every reason code and field reason comes from the SDK (`reason`, `field`),
//! and the two resource scopes carry the SDK resource types, so the
//! `ModelRegistryError` projection of a consumer sees exactly what this gear
//! emits.

use std::fmt;

use authz_resolver_sdk::EnforcerError;
use model_registry_sdk::{ApprovalState, field, reason::permission};
use sea_orm::DbErr;
use tenant_resolver_sdk::TenantResolverError;
use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_db::DbError;
use toolkit_db::contention::is_retryable_contention;
use uuid::Uuid;

/// Error scope of model-addressed outcomes; its literal equals
/// `model_registry_sdk::gts::MODEL_RESOURCE_TYPE`.
#[resource_error(gts_id!("cf.genai.model_registry.model.v1~"))]
pub struct ModelScope;

/// Error scope of provider-addressed outcomes; its literal equals
/// `model_registry_sdk::gts::PROVIDER_RESOURCE_TYPE`.
#[resource_error(gts_id!("cf.genai.model_registry.provider.v1~"))]
pub struct ProviderScope;

/// A dependency that could not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dependency {
    TenantResolver,
    Pdp,
    TypesRegistry,
    Database,
}

impl fmt::Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TenantResolver => "tenant-resolver",
            Self::Pdp => "authz-resolver",
            Self::TypesRegistry => "types-registry",
            Self::Database => "database",
        })
    }
}

/// One field violation of an `invalid_argument` error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub field: String,
    pub description: String,
    pub reason: field::Reason,
}

impl Violation {
    #[must_use]
    pub fn new(
        field: impl Into<String>,
        description: impl Into<String>,
        reason: field::Reason,
    ) -> Self {
        Self {
            field: field.into(),
            description: description.into(),
            reason,
        }
    }
}

/// Domain-level error of every catalog operation.
#[derive(Debug, Error)]
pub enum DomainError {
    /// Not in the effective catalog, malformed canonical id, or an unknown or
    /// invisible model id. `name` is the canonical id or the id as supplied.
    #[error("model not found: {name}")]
    ModelNotFound { name: String },

    #[error("provider not found: {id}")]
    ProviderNotFound { id: Uuid },

    #[error("provider of {canonical_id} is disabled")]
    ProviderDisabled { canonical_id: String },

    #[error("model {canonical_id} is not approved ({})", state.as_str())]
    ModelNotApproved {
        canonical_id: String,
        state: ApprovalState,
    },

    #[error("not permitted: {operation}")]
    NotPermitted { operation: &'static str },

    #[error("{operation} on inherited entry {id} is not allowed")]
    InheritedReadOnly { operation: &'static str, id: Uuid },

    #[error("provider slug already exists: {slug}")]
    ProviderAlreadyExists { slug: String },

    #[error("model already exists: {canonical_id}")]
    ModelAlreadyExists { canonical_id: String },

    #[error("invalid request: {} violation(s)", .0.len())]
    Invalid(Vec<Violation>),

    #[error(transparent)]
    OData(toolkit_odata::Error),

    /// The dependency did not answer within its deadline or failed. The
    /// diagnostic stays in the logs.
    #[error("{dependency} unavailable: {diagnostic}")]
    DependencyUnavailable {
        dependency: Dependency,
        diagnostic: String,
    },

    #[error("internal error: {detail}")]
    Internal { detail: String },
}

impl DomainError {
    #[must_use]
    pub fn model_not_found(name: impl Into<String>) -> Self {
        Self::ModelNotFound { name: name.into() }
    }

    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn unavailable(dependency: Dependency, diagnostic: impl fmt::Display) -> Self {
        Self::DependencyUnavailable {
            dependency,
            diagnostic: diagnostic.to_string(),
        }
    }

    #[must_use]
    pub fn invalid(violation: Violation) -> Self {
        Self::Invalid(vec![violation])
    }

    /// Enforcer outcome mapping of DESIGN §4 Authorization rule 5.
    #[must_use]
    pub fn from_enforcer(err: EnforcerError, operation: &'static str) -> Self {
        match err {
            EnforcerError::Denied { deny_reason } => {
                tracing::debug!(operation, ?deny_reason, "pdp denied");
                Self::NotPermitted { operation }
            }
            EnforcerError::CompileFailed(e) => {
                tracing::warn!(operation, error = %e, "pdp constraints could not be compiled");
                Self::NotPermitted { operation }
            }
            EnforcerError::EvaluationFailed(e) => Self::unavailable(Dependency::Pdp, e),
        }
    }

    /// Tenant Resolver outcome mapping of DESIGN §4 Error Handling.
    #[must_use]
    pub fn from_tenant_resolver(err: TenantResolverError, operation: &'static str) -> Self {
        match err {
            TenantResolverError::TenantNotFound { .. } | TenantResolverError::Unauthorized => {
                tracing::warn!(operation, error = %err, "tenant resolver refused the caller's tenant");
                Self::NotPermitted { operation }
            }
            TenantResolverError::NoPluginAvailable
            | TenantResolverError::ServiceUnavailable(_)
            | TenantResolverError::Internal(_) => {
                Self::unavailable(Dependency::TenantResolver, err)
            }
        }
    }

    /// A database failure the pagination helper reports as text.
    #[must_use]
    pub fn from_db_message(message: &str) -> Self {
        Self::from_db_err(&DbErr::Custom(message.to_owned()))
    }

    /// Database error classification: connectivity, pool, timeouts, and
    /// contention are dependency failures; everything else is internal.
    #[must_use]
    pub fn from_db_err(err: &DbErr) -> Self {
        if is_unavailable_db_err(err) {
            return Self::unavailable(Dependency::Database, err);
        }
        Self::internal(format!("database error: {err}"))
    }
}

fn is_unavailable_db_err(err: &DbErr) -> bool {
    if matches!(err, DbErr::Conn(_) | DbErr::ConnectionAcquire(_)) {
        return true;
    }
    let backends = [
        sea_orm::DbBackend::Postgres,
        sea_orm::DbBackend::Sqlite,
        sea_orm::DbBackend::MySql,
    ];
    if backends.iter().any(|b| is_retryable_contention(*b, err)) {
        return true;
    }
    let msg = err.to_string().to_lowercase();
    msg.contains("statement timeout")
        || msg.contains("canceling statement")
        || msg.contains("pool timed out")
        || msg.contains("timed out")
}

impl From<DbError> for DomainError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Sea(e) => Self::from_db_err(&e),
            // Driver-level errors reach the gear only through connect and
            // pool failures, which are dependency failures.
            DbError::Sqlx(e) => Self::unavailable(Dependency::Database, e),
            other => Self::internal(format!("database error: {other}")),
        }
    }
}

impl From<toolkit_odata::Error> for DomainError {
    fn from(err: toolkit_odata::Error) -> Self {
        Self::OData(err)
    }
}

impl From<DomainError> for CanonicalError {
    // One flat match is the whole point of this mapping.
    #[allow(clippy::cognitive_complexity)]
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::ModelNotFound { name } => ModelScope::not_found("model not found")
                .with_resource(name)
                .create(),
            DomainError::ProviderNotFound { id } => ProviderScope::not_found("provider not found")
                .with_resource(id.to_string())
                .create(),
            DomainError::ProviderDisabled { canonical_id } => {
                tracing::debug!(canonical_id, "provider disabled");
                ModelScope::permission_denied()
                    .with_reason(permission::PROVIDER_DISABLED)
                    .create()
            }
            DomainError::ModelNotApproved {
                canonical_id,
                state,
            } => {
                tracing::debug!(canonical_id, state = state.as_str(), "model not approved");
                ModelScope::permission_denied()
                    .with_reason(permission::MODEL_NOT_APPROVED)
                    .create()
            }
            DomainError::NotPermitted { operation } => {
                tracing::warn!(operation, "not permitted");
                ModelScope::permission_denied()
                    .with_reason(permission::NOT_PERMITTED)
                    .create()
            }
            DomainError::InheritedReadOnly { operation, id } => {
                tracing::warn!(operation, %id, "write against an inherited entry");
                ModelScope::permission_denied()
                    .with_reason(permission::INHERITED_READ_ONLY)
                    .create()
            }
            DomainError::ProviderAlreadyExists { slug } => {
                ProviderScope::already_exists("provider slug already exists in the tenant")
                    .with_resource(slug)
                    .create()
            }
            DomainError::ModelAlreadyExists { canonical_id } => {
                ModelScope::already_exists("model already exists in the tenant")
                    .with_resource(canonical_id)
                    .create()
            }
            DomainError::Invalid(violations) => {
                let mut iter = violations.into_iter();
                let Some(first) = iter.next() else {
                    return ModelScope::invalid_argument()
                        .with_format("invalid request")
                        .create();
                };
                let mut builder = ModelScope::invalid_argument().with_field_violation(
                    first.field,
                    first.description,
                    first.reason.as_wire(),
                );
                for v in iter {
                    builder =
                        builder.with_field_violation(v.field, v.description, v.reason.as_wire());
                }
                builder.create()
            }
            DomainError::OData(e) => e.into(),
            DomainError::DependencyUnavailable {
                dependency,
                diagnostic,
            } => {
                tracing::warn!(%dependency, diagnostic, "dependency unavailable");
                CanonicalError::service_unavailable()
                    .with_detail(format!("{dependency} unavailable"))
                    .create()
            }
            DomainError::Internal { detail } => {
                tracing::error!(detail, "model-registry internal error");
                CanonicalError::internal("internal error").create()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_registry_sdk::gts::{MODEL_RESOURCE_TYPE, PROVIDER_RESOURCE_TYPE};
    use model_registry_sdk::{ModelRegistryError, gts::Resource};
    use toolkit_canonical_errors::Problem;

    fn problem(err: DomainError) -> serde_json::Value {
        serde_json::to_value(Problem::from(CanonicalError::from(err))).unwrap()
    }

    #[test]
    fn scopes_carry_the_sdk_resource_types() {
        let json = problem(DomainError::model_not_found("openai::gpt-4o"));
        assert_eq!(json["context"]["resource_type"], MODEL_RESOURCE_TYPE);
        assert_eq!(json["context"]["resource_name"], "openai::gpt-4o");
        assert_eq!(json["status"], 404);

        let id = Uuid::new_v4();
        let json = problem(DomainError::ProviderNotFound { id });
        assert_eq!(json["context"]["resource_type"], PROVIDER_RESOURCE_TYPE);
        assert_eq!(json["context"]["resource_name"], id.to_string());
    }

    #[test]
    fn permission_reasons_are_the_sdk_constants() {
        let cases = [
            (
                DomainError::ProviderDisabled {
                    canonical_id: "a::b".into(),
                },
                permission::PROVIDER_DISABLED,
            ),
            (
                DomainError::ModelNotApproved {
                    canonical_id: "a::b".into(),
                    state: ApprovalState::Revoked,
                },
                permission::MODEL_NOT_APPROVED,
            ),
            (
                DomainError::NotPermitted {
                    operation: "resolve_model",
                },
                permission::NOT_PERMITTED,
            ),
            (
                DomainError::InheritedReadOnly {
                    operation: "update_model",
                    id: Uuid::nil(),
                },
                permission::INHERITED_READ_ONLY,
            ),
        ];
        for (err, reason) in cases {
            let json = problem(err);
            assert_eq!(json["status"], 403);
            assert_eq!(json["context"]["reason"], reason);
        }
    }

    #[test]
    fn already_exists_projects_typed_resource() {
        let err =
            ModelRegistryError::from(CanonicalError::from(DomainError::ProviderAlreadyExists {
                slug: "openai".into(),
            }));
        assert!(matches!(
            err,
            ModelRegistryError::AlreadyExists {
                resource: Resource::Provider,
                ..
            }
        ));
        let err = ModelRegistryError::from(CanonicalError::from(DomainError::ModelAlreadyExists {
            canonical_id: "openai::gpt-4o".into(),
        }));
        assert!(matches!(
            err,
            ModelRegistryError::AlreadyExists {
                resource: Resource::Model,
                ..
            }
        ));
    }

    #[test]
    fn field_violations_carry_every_sdk_reason() {
        let violations: Vec<Violation> = field::Reason::ALL
            .iter()
            .map(|r| Violation::new("f", "d", *r))
            .collect();
        let json = problem(DomainError::Invalid(violations));
        assert_eq!(json["status"], 400);
        let fv = json["context"]["field_violations"].as_array().unwrap();
        assert_eq!(fv.len(), field::Reason::ALL.len());
        for (v, r) in fv.iter().zip(field::Reason::ALL) {
            assert_eq!(v["reason"], r.as_wire());
            assert_eq!(v["field"], "f");
        }
    }

    #[test]
    fn dependency_unavailable_is_503_without_diagnostic() {
        let json = problem(DomainError::unavailable(
            Dependency::TenantResolver,
            "connection refused to 10.0.0.1",
        ));
        assert_eq!(json["status"], 503);
        assert!(!json["detail"].as_str().unwrap().contains("10.0.0.1"));
    }

    #[test]
    fn internal_strips_detail() {
        let json = problem(DomainError::internal("SELECT * FROM models failed"));
        assert_eq!(json["status"], 500);
        assert!(!json["detail"].as_str().unwrap().contains("SELECT"));
    }

    #[test]
    fn enforcer_mapping() {
        let e = DomainError::from_enforcer(EnforcerError::Denied { deny_reason: None }, "op");
        assert!(matches!(e, DomainError::NotPermitted { .. }));
        let e = DomainError::from_enforcer(
            EnforcerError::EvaluationFailed(CanonicalError::service_unavailable().create()),
            "op",
        );
        assert!(matches!(
            e,
            DomainError::DependencyUnavailable {
                dependency: Dependency::Pdp,
                ..
            }
        ));
    }

    #[test]
    fn tenant_resolver_mapping() {
        let e = DomainError::from_tenant_resolver(TenantResolverError::Unauthorized, "op");
        assert!(matches!(e, DomainError::NotPermitted { .. }));
        let e = DomainError::from_tenant_resolver(
            TenantResolverError::TenantNotFound {
                tenant_id: tenant_resolver_sdk::TenantId(Uuid::nil()),
            },
            "op",
        );
        assert!(matches!(e, DomainError::NotPermitted { .. }));
        for err in [
            TenantResolverError::NoPluginAvailable,
            TenantResolverError::ServiceUnavailable("x".into()),
            TenantResolverError::Internal("x".into()),
        ] {
            let e = DomainError::from_tenant_resolver(err, "op");
            assert!(matches!(
                e,
                DomainError::DependencyUnavailable {
                    dependency: Dependency::TenantResolver,
                    ..
                }
            ));
        }
    }

    #[test]
    fn db_classification() {
        let e = DomainError::from_db_err(&DbErr::Custom("deadlock detected".into()));
        assert!(matches!(
            e,
            DomainError::DependencyUnavailable {
                dependency: Dependency::Database,
                ..
            }
        ));
        let e = DomainError::from_db_err(&DbErr::Custom("syntax error".into()));
        assert!(matches!(e, DomainError::Internal { .. }));
        let e = DomainError::from_db_err(&DbErr::Custom(
            "canceling statement due to statement timeout".into(),
        ));
        assert!(matches!(e, DomainError::DependencyUnavailable { .. }));
    }

    #[test]
    fn odata_errors_are_invalid_argument() {
        let json = problem(DomainError::OData(toolkit_odata::Error::InvalidCursor));
        assert_eq!(json["status"], 400);
    }
}
