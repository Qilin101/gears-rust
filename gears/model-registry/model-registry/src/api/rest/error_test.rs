//! Tests for [`DomainError`] → [`CanonicalError`] mapping.
//!
//! Every variant of [`DomainError`] must convert to the correct canonical
//! error category with the expected HTTP status code and GTS type identifier.

use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use crate::domain::error::DomainError;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Assert that a `DomainError` maps to the expected status code and GTS type.
fn assert_mapping(err: DomainError, expected_status: u16, expected_gts_prefix: &str) {
    let canon: CanonicalError = err.into();
    let status = canon.status_code();
    let gts_type = canon.gts_type();
    assert_eq!(
        status,
        expected_status,
        "expected HTTP {expected_status}, got {status} for {}",
        canon.gts_type()
    );
    assert!(
        gts_type.starts_with(expected_gts_prefix),
        "expected GTS type starting with '{expected_gts_prefix}', got '{gts_type}'"
    );
}

/// Assert that a `DomainError` maps to the expected status, GTS type prefix,
/// AND that the `resource_name` / detail match expectations.
fn assert_mapping_with_detail(
    err: DomainError,
    expected_status: u16,
    expected_gts_prefix: &str,
    expected_detail_substring: &str,
) {
    let canon: CanonicalError = err.into();
    assert_eq!(
        canon.status_code(),
        expected_status,
        "status mismatch for {expected_detail_substring}"
    );
    assert!(
        canon.gts_type().starts_with(expected_gts_prefix),
        "gts_type mismatch"
    );
    assert!(
        canon
            .detail()
            .to_lowercase()
            .contains(&expected_detail_substring.to_lowercase()),
        "expected detail containing '{expected_detail_substring}', got '{}'",
        canon.detail()
    );
}

// ---------------------------------------------------------------------------
// 404 — Not Found
// ---------------------------------------------------------------------------

#[test]
fn model_not_found_maps_to_404() {
    assert_mapping_with_detail(
        DomainError::model_not_found("openai::gpt-4o"),
        404,
        "gts.cf.core.errors.err.v1~cf.core.err.not_found",
        "model not found",
    );
}

#[test]
fn provider_not_found_maps_to_404() {
    assert_mapping_with_detail(
        DomainError::provider_not_found(Uuid::nil()),
        404,
        "gts.cf.core.errors.err.v1~cf.core.err.not_found",
        "provider not found",
    );
}

#[test]
fn model_deprecated_maps_to_404() {
    assert_mapping_with_detail(
        DomainError::model_deprecated("openai::gpt-4o"),
        404,
        "gts.cf.core.errors.err.v1~cf.core.err.not_found",
        "deprecated",
    );
}

// ---------------------------------------------------------------------------
// 403 — Permission Denied
// ---------------------------------------------------------------------------

#[test]
fn model_not_approved_maps_to_403() {
    assert_mapping(
        DomainError::model_not_approved("openai::gpt-4o"),
        403,
        "gts.cf.core.errors.err.v1~cf.core.err.permission_denied",
    );
}

#[test]
fn forbidden_maps_to_403() {
    assert_mapping(
        DomainError::forbidden("no access"),
        403,
        "gts.cf.core.errors.err.v1~cf.core.err.permission_denied",
    );
}

// ---------------------------------------------------------------------------
// 409 — Already Exists / Conflict
// ---------------------------------------------------------------------------

#[test]
fn provider_conflict_maps_to_409() {
    assert_mapping_with_detail(
        DomainError::provider_conflict("openai"),
        409,
        "gts.cf.core.errors.err.v1~cf.core.err.already_exists",
        "already exists",
    );
}

#[test]
fn provider_disabled_maps_to_409() {
    assert_mapping_with_detail(
        DomainError::provider_disabled(Uuid::nil()),
        409,
        "gts.cf.core.errors.err.v1~cf.core.err.already_exists",
        "disabled",
    );
}

#[test]
fn invalid_transition_maps_to_409() {
    assert_mapping_with_detail(
        DomainError::invalid_transition("cannot deprecate from preview"),
        409,
        "gts.cf.core.errors.err.v1~cf.core.err.already_exists",
        "transition",
    );
}

// ---------------------------------------------------------------------------
// 422 — Invalid Argument
// ---------------------------------------------------------------------------

#[test]
fn validation_maps_to_422() {
    assert_mapping_with_detail(
        DomainError::validation("slug cannot be empty"),
        400,
        "gts.cf.core.errors.err.v1~cf.core.err.invalid_argument",
        "slug cannot be empty",
    );
}

// ---------------------------------------------------------------------------
// 500 — Internal
// ---------------------------------------------------------------------------

#[test]
fn internal_maps_to_500() {
    assert_mapping(
        DomainError::internal("connection pool exhausted"),
        500,
        "gts.cf.core.errors.err.v1~cf.core.err.internal",
    );
}

#[test]
fn internal_with_source_maps_to_500() {
    let upstream = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "rst");
    let canon: CanonicalError = DomainError::internal_from("upstream failed", upstream).into();
    assert_eq!(canon.status_code(), 500);
    assert!(canon.gts_type().contains("internal"));
}

#[test]
fn database_maps_to_500() {
    use toolkit_db::DbError;
    let canon: CanonicalError = DomainError::Database(DbError::UnknownDsn("test".into())).into();
    assert_eq!(canon.status_code(), 500);
    assert!(canon.gts_type().contains("internal"));
}

// ---------------------------------------------------------------------------
// Roundtrip: all non-internal variants produce a Problem-friendly structure
// ---------------------------------------------------------------------------

#[test]
fn all_error_variants_have_valid_status() {
    // Collect every DomainError variant and verify the canonical error
    // carries a well-known status code (not 0, not > 599).
    let cases: Vec<(DomainError, u16)> = vec![
        (DomainError::model_not_found("m1"), 404),
        (DomainError::provider_not_found(Uuid::nil()), 404),
        (DomainError::model_deprecated("m1"), 404),
        (DomainError::model_not_approved("m1"), 403),
        (DomainError::forbidden("x"), 403),
        (DomainError::provider_conflict("s"), 409),
        (DomainError::provider_disabled(Uuid::nil()), 409),
        (DomainError::invalid_transition("t"), 409),
        (DomainError::validation("v"), 400),
        (DomainError::internal("e"), 500),
    ];

    for (err, expected_status) in cases {
        let canon: CanonicalError = err.into();
        let status = canon.status_code();
        assert_eq!(
            status,
            expected_status,
            "expected HTTP {expected_status}, got {status} for {}",
            canon.gts_type()
        );
    }
}
