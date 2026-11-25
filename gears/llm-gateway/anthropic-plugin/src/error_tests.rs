// Created: 2026-11-09 by Constructor Tech
use toolkit_canonical_errors::resource_error;

use super::*;

#[resource_error(gts_id!("cf.core.oagw.upstream.v1~"))]
struct UpstreamScope;

fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(*name, http::HeaderValue::from_static(value));
    }
    map
}

fn error_body(kind: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "type": "error",
        "error": { "type": kind, "message": message },
        "request_id": "req_body"
    }))
    .unwrap()
}

fn provider_parts(err: LlmGatewayError) -> (ProviderErrorCategory, ProviderFailure) {
    match err {
        LlmGatewayError::Provider {
            category, details, ..
        } => (category, details),
        other => panic!("expected a provider error, got {other:?}"),
    }
}

#[test]
fn http_error_keeps_provider_originals() {
    let err = from_http(
        StatusCode::TOO_MANY_REQUESTS,
        &headers(&[("retry-after", "7")]),
        &error_body("rate_limit_error", "slow down"),
    );
    assert_eq!(err.code(), "rate_limited");
    let (category, details) = provider_parts(err);
    assert_eq!(category, ProviderErrorCategory::RateLimited);
    assert_eq!(details.provider_code.as_deref(), Some("rate_limit_error"));
    assert_eq!(details.provider_message.as_deref(), Some("slow down"));
    assert_eq!(details.provider_request_id.as_deref(), Some("req_body"));
    assert_eq!(details.retry_after, Some(7));
}

#[test]
fn retry_after_ms_wins_and_rounds_up() {
    let err = from_http(
        StatusCode::TOO_MANY_REQUESTS,
        &headers(&[("retry-after", "7"), ("retry-after-ms", "1500")]),
        &error_body("rate_limit_error", "slow down"),
    );
    assert_eq!(provider_parts(err).1.retry_after, Some(2));
}

#[test]
fn error_types_map_to_categories() {
    let cases = [
        (
            "invalid_request_error",
            "prompt is too long: 250000 tokens > 200000 maximum",
            ProviderErrorCategory::ContextLengthExceeded,
        ),
        (
            "invalid_request_error",
            "max_tokens: field required",
            ProviderErrorCategory::InvalidRequest,
        ),
        (
            "authentication_error",
            "invalid x-api-key",
            ProviderErrorCategory::AuthenticationFailed,
        ),
        (
            "permission_error",
            "no access",
            ProviderErrorCategory::AuthenticationFailed,
        ),
        (
            "overloaded_error",
            "overloaded",
            ProviderErrorCategory::ProviderUnavailable,
        ),
        (
            "billing_error",
            "no credits",
            ProviderErrorCategory::ProviderError,
        ),
    ];
    for (kind, message, expected) in cases {
        let err = from_http(
            StatusCode::BAD_REQUEST,
            &HeaderMap::new(),
            &error_body(kind, message),
        );
        assert_eq!(provider_parts(err).0, expected, "{kind}: {message}");
    }
}

#[test]
fn unparseable_body_falls_back_to_status() {
    let err = from_http(
        StatusCode::from_u16(529).unwrap(),
        &headers(&[("request-id", "req_header")]),
        b"<html>overloaded</html>",
    );
    let (category, details) = provider_parts(err);
    assert_eq!(category, ProviderErrorCategory::ProviderUnavailable);
    assert_eq!(details.provider_code, None);
    assert_eq!(details.provider_request_id.as_deref(), Some("req_header"));
}

#[test]
fn stream_error_event_is_categorized() {
    let object = ErrorObject {
        kind: "overloaded_error".to_owned(),
        message: "Overloaded".to_owned(),
    };
    let (category, details) = provider_parts(from_stream_error(&object, Some("req_1")));
    assert_eq!(category, ProviderErrorCategory::ProviderUnavailable);
    assert_eq!(details.provider_request_id.as_deref(), Some("req_1"));
}

#[test]
fn missing_upstream_is_unapproved_destination() {
    let err = UpstreamScope::not_found("no upstream for alias")
        .with_resource("anthropic")
        .create();
    assert!(matches!(
        from_gateway(err),
        LlmGatewayError::UnapprovedDestination
    ));
}

#[test]
fn gateway_unavailable_keeps_retry_after() {
    let err = CanonicalError::service_unavailable()
        .with_retry_after_seconds(30)
        .create();
    let (category, details) = provider_parts(from_gateway(err));
    assert_eq!(category, ProviderErrorCategory::ProviderUnavailable);
    assert_eq!(details.retry_after, Some(30));
}

#[test]
fn unknown_target_host_is_unapproved_destination() {
    let err = UpstreamScope::invalid_argument()
        .with_field_violation(
            "x-target-host",
            "no endpoint for host",
            oagw_sdk::field::UNKNOWN_TARGET_HOST,
        )
        .create();
    assert!(matches!(
        from_gateway(err),
        LlmGatewayError::UnapprovedDestination
    ));
}

#[test]
fn unreachable_upstream_secret_is_credentials_unavailable() {
    let err = UpstreamScope::failed_precondition()
        .with_precondition_violation(
            "auth.config.secret_ref",
            "secret is not provisioned",
            oagw_sdk::precondition::STATE,
        )
        .create();
    assert!(matches!(
        from_gateway(err),
        LlmGatewayError::CredentialsUnavailable
    ));
}

#[test]
fn other_precondition_failure_is_internal() {
    let err = UpstreamScope::failed_precondition()
        .with_precondition_violation("STATE_DRIFT", "state is stale", "STATE")
        .create();
    assert_eq!(from_gateway(err).code(), "internal");
}

#[test]
fn oversized_body_is_invalid_request() {
    let err = UpstreamScope::out_of_range("body too big")
        .with_field_violation("body", "exceeds limit", oagw_sdk::field::PAYLOAD_TOO_LARGE)
        .create();
    let (category, _) = provider_parts(from_gateway(err));
    assert_eq!(category, ProviderErrorCategory::InvalidRequest);
}

#[test]
fn unmodeled_gateway_error_is_internal() {
    let err = CanonicalError::internal("boom").create();
    assert_eq!(from_gateway(err).code(), "internal");
}
