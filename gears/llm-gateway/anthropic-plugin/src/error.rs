// Created: 2026-11-09 by Constructor Tech
//! Normalization of OAGW and Anthropic errors to [`LlmGatewayError`].

use http::{HeaderMap, StatusCode};
use llm_gateway_sdk::{LlmGatewayError, ProviderErrorCategory, ProviderFailure};
use oagw_sdk::ServiceGatewayError;
use oagw_sdk::gts::Resource;
use toolkit_canonical_errors::CanonicalError;

use crate::wire::{ErrorBody, ErrorObject};

/// Response header carrying Anthropic's request id.
const REQUEST_ID_HEADER: &str = "request-id";
const RETRY_AFTER_MS_HEADER: &str = "retry-after-ms";

/// Map an OAGW `proxy_request` failure.
pub fn from_gateway(err: CanonicalError) -> LlmGatewayError {
    match ServiceGatewayError::from(err) {
        ServiceGatewayError::NotFound {
            resource: Resource::Upstream | Resource::Route,
            ..
        }
        | ServiceGatewayError::PermissionDenied { .. } => LlmGatewayError::UnapprovedDestination,
        ServiceGatewayError::RateLimited { retry_after_secs } => gateway_provider(
            ProviderErrorCategory::RateLimited,
            "rate limited by the outbound gateway",
            retry_after_secs,
        ),
        ServiceGatewayError::Timeout => gateway_provider(
            ProviderErrorCategory::ProviderUnavailable,
            "provider call timed out",
            None,
        ),
        ServiceGatewayError::Unavailable { retry_after_secs } => gateway_provider(
            ProviderErrorCategory::ProviderUnavailable,
            "provider unavailable",
            retry_after_secs,
        ),
        ServiceGatewayError::AuthFailed { .. } => gateway_provider(
            ProviderErrorCategory::AuthenticationFailed,
            "provider authentication failed",
            None,
        ),
        other => LlmGatewayError::internal_from("outbound gateway call failed", other),
    }
}

fn gateway_provider(
    category: ProviderErrorCategory,
    message: &str,
    retry_after: Option<u64>,
) -> LlmGatewayError {
    let mut details = ProviderFailure::default();
    if let Some(seconds) = retry_after {
        details = details.with_retry_after(seconds);
    }
    LlmGatewayError::provider(category, message, details)
}

/// Map a non-success HTTP response from the Messages endpoint.
pub fn from_http(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> LlmGatewayError {
    let parsed = serde_json::from_slice::<ErrorBody>(body).ok();
    let (object, body_request_id) = match parsed {
        Some(body) => (Some(body.error), body.request_id),
        None => (None, None),
    };
    let category = categorize(object.as_ref(), Some(status));
    let message = object.as_ref().map_or_else(
        || format!("provider returned HTTP {}", status.as_u16()),
        |o| o.message.clone(),
    );

    let mut details = ProviderFailure::default();
    if let Some(object) = &object {
        details = details
            .with_provider_code(object.kind.clone())
            .with_provider_message(object.message.clone());
    }
    if let Some(id) = body_request_id.or_else(|| request_id(headers)) {
        details = details.with_provider_request_id(id);
    }
    if let Some(seconds) = retry_after(headers) {
        details = details.with_retry_after(seconds);
    }
    LlmGatewayError::provider(category, message, details)
}

/// Map an in-band SSE `error` event.
pub fn from_stream_error(object: &ErrorObject, request_id: Option<&str>) -> LlmGatewayError {
    let mut details = ProviderFailure::default()
        .with_provider_code(object.kind.clone())
        .with_provider_message(object.message.clone());
    if let Some(id) = request_id {
        details = details.with_provider_request_id(id);
    }
    LlmGatewayError::provider(
        categorize(Some(object), None),
        object.message.clone(),
        details,
    )
}

fn categorize(object: Option<&ErrorObject>, status: Option<StatusCode>) -> ProviderErrorCategory {
    object
        .and_then(category_for_type)
        .or_else(|| status.map(category_for_status))
        .unwrap_or(ProviderErrorCategory::ProviderError)
}

fn category_for_type(object: &ErrorObject) -> Option<ProviderErrorCategory> {
    let category = match object.kind.as_str() {
        "invalid_request_error" if is_context_length(&object.message) => {
            ProviderErrorCategory::ContextLengthExceeded
        }
        "invalid_request_error" | "not_found_error" | "request_too_large" => {
            ProviderErrorCategory::InvalidRequest
        }
        "authentication_error" | "permission_error" => ProviderErrorCategory::AuthenticationFailed,
        "rate_limit_error" => ProviderErrorCategory::RateLimited,
        "overloaded_error" | "api_error" | "timeout_error" => {
            ProviderErrorCategory::ProviderUnavailable
        }
        "billing_error" => ProviderErrorCategory::ProviderError,
        _ => return None,
    };
    Some(category)
}

fn category_for_status(status: StatusCode) -> ProviderErrorCategory {
    match status.as_u16() {
        400 | 404 | 413 | 422 => ProviderErrorCategory::InvalidRequest,
        401 | 403 => ProviderErrorCategory::AuthenticationFailed,
        429 => ProviderErrorCategory::RateLimited,
        408 | 500..=599 => ProviderErrorCategory::ProviderUnavailable,
        _ => ProviderErrorCategory::ProviderError,
    }
}

fn is_context_length(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("prompt is too long") || message.contains("context window")
}

/// Anthropic's request id from the response headers.
pub fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Retry delay in whole seconds from `retry-after-ms` or `retry-after`.
fn retry_after(headers: &HeaderMap) -> Option<u64> {
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(ms) = header(RETRY_AFTER_MS_HEADER).and_then(|v| v.trim().parse::<f64>().ok()) {
        return seconds_from_millis(ms);
    }
    header(http::header::RETRY_AFTER.as_str()).and_then(|v| v.trim().parse::<u64>().ok())
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn seconds_from_millis(ms: f64) -> Option<u64> {
    (ms.is_finite() && ms >= 0.0).then(|| (ms / 1000.0).ceil() as u64)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
