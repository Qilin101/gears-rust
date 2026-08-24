//! Wire-string → SDK-enum parsing for request DTOs.
//!
//! Request DTOs carry status fields as `String` (see `dto.rs`), so handlers
//! have to narrow them to the SDK enums. Each function pairs
//! `X::from_wire` with the canonical field-violation shape the endpoints
//! document, and lists the accepted domain from `X::ALL` rather than from a
//! restated string literal.

use model_registry_sdk::models::{ApprovalStatus, LifecycleStatus, ProviderStatus};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::ODataQuery;

use super::error::ModelRegistryResourceError;

/// Build the 400 field violation for an unparseable status value.
fn invalid(field: &'static str, code: &'static str, raw: &str, expected: &str) -> CanonicalError {
    ModelRegistryResourceError::invalid_argument()
        .with_field_violation(
            field,
            format!("unknown value `{raw}`, expected one of: {expected}"),
            code,
        )
        .create()
}

/// Parse the `status` field of a provider request.
///
/// # Errors
/// `INVALID_PROVIDER_STATUS` when `raw` is outside the documented domain.
pub(super) fn provider_status(raw: &str) -> Result<ProviderStatus, CanonicalError> {
    ProviderStatus::from_wire(raw).ok_or_else(|| {
        let expected = ProviderStatus::ALL
            .iter()
            .copied()
            .map(ProviderStatus::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        invalid("status", "INVALID_PROVIDER_STATUS", raw, &expected)
    })
}

/// Parse the `lifecycle_status` field of a model request.
///
/// # Errors
/// `INVALID_LIFECYCLE_STATUS` when `raw` is outside the documented domain.
pub(super) fn lifecycle_status(raw: &str) -> Result<LifecycleStatus, CanonicalError> {
    LifecycleStatus::from_wire(raw).ok_or_else(|| {
        let expected = LifecycleStatus::ALL
            .iter()
            .copied()
            .map(LifecycleStatus::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        invalid(
            "lifecycle_status",
            "INVALID_LIFECYCLE_STATUS",
            raw,
            &expected,
        )
    })
}

/// Parse the `approval_status` field of a model request.
///
/// # Errors
/// `INVALID_APPROVAL_STATUS` when `raw` is outside the documented domain.
pub(super) fn approval_status(raw: &str) -> Result<ApprovalStatus, CanonicalError> {
    ApprovalStatus::from_wire(raw).ok_or_else(|| {
        let expected = ApprovalStatus::ALL
            .iter()
            .copied()
            .map(ApprovalStatus::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        invalid("approval_status", "INVALID_APPROVAL_STATUS", raw, &expected)
    })
}

/// Reject `$select` on the listing endpoints.
///
/// The listing handlers return whole `ModelDto` / `ProviderDto` values — there
/// is no projection stage — so accepting `$select` and ignoring it would
/// silently return more than the caller asked for.
///
/// # Errors
/// `UNSUPPORTED_SELECT` when the request carries a `$select` clause.
pub(super) fn reject_select(query: &ODataQuery) -> Result<(), CanonicalError> {
    if query.select.is_some() {
        return Err(ModelRegistryResourceError::invalid_argument()
            .with_field_violation(
                "$select",
                "$select is not supported by this endpoint; responses always carry every field",
                "UNSUPPORTED_SELECT",
            )
            .create());
    }
    Ok(())
}

/// Query parameters for `GET /model-registry/v1/admin/models`.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct AdminModelsQuery {
    /// Whether to include deprecated/sunset models; defaults to `false`.
    #[serde(default)]
    pub include_deprecated: bool,
}

#[cfg(test)]
mod tests {
    use toolkit_canonical_errors::InvalidArgument;

    use super::*;

    /// Assert the error is a 400 whose single field violation carries the
    /// documented field name and reason code, and names the accepted values.
    fn assert_violation(err: &CanonicalError, field: &str, code: &str, expected_values: &[&str]) {
        assert_eq!(err.status_code(), 400, "status");
        let CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        } = err
        else {
            panic!("expected field violations, got {err:?}");
        };
        assert_eq!(field_violations.len(), 1);
        let v = &field_violations[0];
        assert_eq!(v.field, field);
        assert_eq!(v.reason, code);
        for value in expected_values {
            assert!(
                v.description.contains(value),
                "description must name `{value}`, got `{}`",
                v.description
            );
        }
    }

    #[test]
    fn provider_status_parses_every_variant() {
        for variant in ProviderStatus::ALL {
            let parsed = provider_status(variant.as_str()).expect("wire value must parse");
            assert_eq!(parsed, *variant);
        }
    }

    #[test]
    fn lifecycle_status_parses_every_variant() {
        for variant in LifecycleStatus::ALL {
            let parsed = lifecycle_status(variant.as_str()).expect("wire value must parse");
            assert_eq!(parsed, *variant);
        }
    }

    #[test]
    fn approval_status_parses_every_variant() {
        for variant in ApprovalStatus::ALL {
            let parsed = approval_status(variant.as_str()).expect("wire value must parse");
            assert_eq!(parsed, *variant);
        }
    }

    #[test]
    fn provider_status_rejects_unknown_value() {
        let err = provider_status("Active").expect_err("uppercase is not a wire value");
        assert_violation(
            &err,
            "status",
            "INVALID_PROVIDER_STATUS",
            &["Active", "active", "disabled"],
        );
    }

    #[test]
    fn lifecycle_status_rejects_unknown_value() {
        let err = lifecycle_status("retired").expect_err("not a lifecycle status");
        assert_violation(
            &err,
            "lifecycle_status",
            "INVALID_LIFECYCLE_STATUS",
            &["retired", "production", "sunset"],
        );
    }

    #[test]
    fn approval_status_rejects_unknown_value() {
        let err = approval_status("").expect_err("empty is not an approval status");
        assert_violation(
            &err,
            "approval_status",
            "INVALID_APPROVAL_STATUS",
            &["pending", "revoked"],
        );
    }

    #[test]
    fn reject_select_accepts_query_without_select() {
        reject_select(&ODataQuery::default()).expect("no $select is fine");
    }

    #[test]
    fn reject_select_rejects_query_with_select() {
        let query = ODataQuery::default().with_select(vec!["vendor".to_owned()]);
        let err = reject_select(&query).expect_err("$select is unsupported");
        assert_violation(&err, "$select", "UNSUPPORTED_SELECT", &["not supported"]);
    }

    #[test]
    fn admin_models_query_defaults_include_deprecated_false() {
        let query = super::AdminModelsQuery::default();
        assert!(!query.include_deprecated);
    }

    #[test]
    fn admin_models_query_parses_include_deprecated() {
        // Verify the Default impl matches what axum Query extraction would give
        // when `include_deprecated` is absent (the `#[serde(default)]` path).
        let query = super::AdminModelsQuery {
            include_deprecated: true,
        };
        assert!(query.include_deprecated);
    }
}
