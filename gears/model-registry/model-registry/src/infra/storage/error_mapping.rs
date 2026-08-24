//! Shared error-mapping helpers for the storage layer.
//!
//! Used by both [`super::provider_repo`] and [`super::model_repo`] to translate
//! low-level `SeaORM` and scope errors into domain errors.

use sea_orm::DbErr;
use toolkit_db::secure::ScopeError;

use crate::domain::error::DomainError;

/// Check whether a [`sea_orm::DbErr`] represents a foreign-key constraint violation.
///
/// Uses `SeaORM`'s built-in `sql_err()` detection first, then falls back to
/// string matching on the error message for backends that strip the SQLSTATE.
#[must_use]
pub(super) fn is_fk_violation(err: &DbErr) -> bool {
    // Discard unique-constraint violations early.
    if matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) {
        return false;
    }

    // Fallback: string-based detection for FK violations.
    let msg = err.to_string().to_lowercase();
    msg.contains("foreign key")
        || msg.contains("constraint failed")
        || msg.contains("is still referenced")
}

/// Map a [`ScopeError`] to a [`DomainError`].
///
/// Security denials become `Forbidden`; infrastructure problems become
/// `Internal`; invalid states become `Internal` (programming error).
pub(super) fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Denied(msg) => DomainError::forbidden(msg),
        ScopeError::Invalid(msg) => DomainError::internal(format!("scope invalid: {msg}")),
        ScopeError::Db(e) => DomainError::internal(format!("database error: {e}")),
        ScopeError::TenantNotInScope { tenant_id } => {
            DomainError::forbidden(format!("tenant {tenant_id} not in scope"))
        }
    }
}
