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
fn provider_not_found_by_slug_maps_to_404() {
    assert_mapping(
        DomainError::provider_not_found_by_slug("openai"),
        404,
        "gts.cf.core.errors.err.v1~cf.core.err.not_found",
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
// 409 — Already Exists
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
fn provider_not_owned_maps_to_403() {
    assert_mapping(
        DomainError::provider_not_owned("openai"),
        403,
        "gts.cf.core.errors.err.v1~cf.core.err.permission_denied",
    );
}

#[test]
fn provider_disabled_maps_to_403() {
    assert_mapping(
        DomainError::provider_disabled(Uuid::nil()),
        403,
        "gts.cf.core.errors.err.v1~cf.core.err.permission_denied",
    );
}

// ---------------------------------------------------------------------------
// 400 — Failed Precondition
// ---------------------------------------------------------------------------

#[test]
fn invalid_transition_maps_to_400_failed_precondition() {
    let canon: CanonicalError =
        DomainError::invalid_transition("cannot deprecate from preview").into();
    assert_eq!(canon.status_code(), 400);
    assert!(
        canon
            .gts_type()
            .starts_with("gts.cf.core.errors.err.v1~cf.core.err.failed_precondition"),
        "expected failed_precondition, got '{}'",
        canon.gts_type()
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canon else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations.len(), 1);
    assert_eq!(ctx.violations[0].subject, "model");
    assert_eq!(ctx.violations[0].type_, "INVALID_TRANSITION");
    assert_eq!(
        ctx.violations[0].description,
        "cannot deprecate from preview"
    );
}

#[test]
fn provider_has_models_maps_to_400_failed_precondition() {
    let canon: CanonicalError = DomainError::provider_has_models(Uuid::nil(), 3).into();
    assert_eq!(canon.status_code(), 400);
    assert_eq!(
        canon.resource_name(),
        Some(Uuid::nil().to_string().as_str())
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canon else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "provider");
    assert_eq!(ctx.violations[0].type_, "PROVIDER_HAS_MODELS");
    assert!(
        ctx.violations[0].description.contains('3'),
        "expected the model count in the violation description, got '{}'",
        ctx.violations[0].description
    );
}

/// The FK-violation fallback passes `model_count: 0` (count unknown, not
/// zero), so the description must not claim zero models.
#[test]
fn provider_has_models_omits_unknown_count() {
    let canon: CanonicalError = DomainError::provider_has_models(Uuid::nil(), 0).into();
    assert_eq!(canon.status_code(), 400);
    let CanonicalError::FailedPrecondition { ctx, .. } = canon else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].description, "provider still owns models");
}

// ---------------------------------------------------------------------------
// 400 — Invalid Argument
// ---------------------------------------------------------------------------

#[test]
fn validation_maps_to_400() {
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
        (DomainError::provider_not_owned("slug"), 403),
        (DomainError::model_deprecated("m1"), 404),
        (DomainError::model_not_approved("m1"), 403),
        (DomainError::forbidden("x"), 403),
        (DomainError::provider_conflict("s"), 409),
        (DomainError::provider_has_models(Uuid::nil(), 3), 400),
        (DomainError::provider_disabled(Uuid::nil()), 403),
        (DomainError::invalid_transition("t"), 400),
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
