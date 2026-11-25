// Created: 2026-11-18 by Constructor Tech
//! Anthropic pass-through: forwards a native `POST /v1/messages` or
//! `POST /v1/messages/count_tokens` request and relays the native response or
//! SSE stream, with the generic usage read from it.

use futures::StreamExt;
use http::response::Parts;
use llm_gateway_sdk::{
    LlmGatewayError, PassthroughApi, PassthroughEvent, PassthroughJsonResponse, PassthroughMethod,
    PassthroughRequest, PassthroughResponse, ProviderCallCtx, ProviderErrorCategory,
    ProviderFailure,
};
use oagw_sdk::ServiceGatewayClientV1;
use oagw_sdk::sse::{ServerEvent, ServerEventsResponse, ServerEventsStream};
use serde_json::{Value, json};
use toolkit_security::SecurityContext;

use crate::client::{
    ANTHROPIC_BETA_HEADER, ANTHROPIC_VERSION_HEADER, http_request, malformed_response, read_body,
    version, with_cancel,
};
use crate::settings::AnthropicProviderSettingsV1;
use crate::wire::{COUNT_TOKENS_PATH, MESSAGES_PATH};
use crate::{error, guard, response, stream};

/// The endpoints forwarded on the pass-through route, all `POST`.
const ENDPOINTS: [&str; 2] = [MESSAGES_PATH, COUNT_TOKENS_PATH];

const CAPABILITY: &str = "anthropic_passthrough";
const MODEL: &str = "model";

/// Forward a native Messages or token counting request. See
/// [`llm_gateway_sdk::LlmGatewayProviderPluginClientV1::forward_passthrough`].
pub async fn forward(
    gateway: &dyn ServiceGatewayClientV1,
    ctx: &SecurityContext,
    call: &ProviderCallCtx,
    req: PassthroughRequest,
) -> Result<PassthroughResponse, LlmGatewayError> {
    if req.api != PassthroughApi::Anthropic
        || req.method != PassthroughMethod::Post
        || !ENDPOINTS.contains(&req.path.as_str())
    {
        return Err(LlmGatewayError::capability_not_supported(CAPABILITY));
    }
    let Some(mut body) = req.body else {
        return Err(LlmGatewayError::validation_param(
            "model is required",
            MODEL,
        ));
    };
    guard::check_request(&body)?;

    let settings: AnthropicProviderSettingsV1 = call
        .provider_settings()
        .map_err(|e| LlmGatewayError::internal_from("model is not an Anthropic model", e))?;
    let info = &call.model_info;
    // OAGW cannot yet be told which credential to use.
    if info.secret_ref.is_some() {
        return Err(LlmGatewayError::CredentialsUnavailable);
    }

    let PassthroughRequest {
        path,
        query,
        stream,
        headers,
        ..
    } = req;
    let model = match body.get_mut(MODEL) {
        Some(slot) if slot.is_string() => {
            std::mem::replace(slot, json!(info.provider_model_id.as_str()))
        }
        _ => {
            return Err(LlmGatewayError::validation_param(
                "model is required",
                MODEL,
            ));
        }
    };
    let payload = serde_json::to_vec(&body)
        .map_err(|e| LlmGatewayError::internal_from("failed to serialize the request", e))?;

    let version = header_values(&headers, ANTHROPIC_VERSION_HEADER)
        .next()
        .unwrap_or_else(|| version(&settings));
    let beta = guard::beta_header(header_values(&headers, ANTHROPIC_BETA_HEADER));
    let path = match query {
        Some(query) => format!("{path}?{query}"),
        None => path,
    };
    let request = http_request(
        &path,
        version,
        beta.as_deref(),
        info.base_url.as_deref(),
        payload,
        stream,
    )?;

    let response = with_cancel(&call.cancel, gateway.proxy_request(ctx.clone(), request))
        .await?
        .map_err(error::from_gateway)?;
    let provider_request_id = error::request_id(response.headers());

    if !stream {
        return json_response(call, response, model, false).await;
    }
    match ServerEventsStream::from_response::<ServerEvent>(response) {
        ServerEventsResponse::Events(events) => {
            let mut relay = Relay {
                model,
                usage: Value::Null,
            };
            let request_id = provider_request_id.clone();
            let events = events
                .map(move |event| match event {
                    Ok(event) => Ok(relay.on_event(event)),
                    Err(err) => Err(interrupted(request_id.as_deref(), &err.to_string())),
                })
                // An error ends the stream.
                .scan(false, |failed, item| {
                    let item = (!*failed).then_some(item);
                    *failed |= matches!(item, Some(Err(_)));
                    std::future::ready(item)
                })
                .take_until(call.cancel.clone().cancelled_owned());
            Ok(PassthroughResponse::Stream {
                events: Box::pin(events),
                provider_request_id,
            })
        }
        ServerEventsResponse::Response(response) => {
            json_response(call, response, model, true).await
        }
    }
}

/// Non-empty values of the protocol header `name`.
fn header_values<'a>(
    headers: &'a [(String, String)],
    name: &'static str,
) -> impl Iterator<Item = &'a str> {
    headers
        .iter()
        .filter(move |(n, v)| n.eq_ignore_ascii_case(name) && !v.is_empty())
        .map(|(_, v)| v.as_str())
}

/// A native JSON response: the message of a non-streaming call, or a provider
/// error. `stream` is whether an event stream was expected instead.
async fn json_response(
    call: &ProviderCallCtx,
    response: http::Response<oagw_sdk::Body>,
    model: Value,
    stream: bool,
) -> Result<PassthroughResponse, LlmGatewayError> {
    let (parts, body) = response.into_parts();
    let bytes = read_body(&call.cancel, body).await?;
    let Parts {
        status, headers, ..
    } = parts;
    let provider_request_id = error::request_id(&headers);

    if !status.is_success() {
        let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
            return Err(error::from_http(status, &headers, &bytes));
        };
        let (category, _, details) = error::http_failure(status, &headers, &bytes);
        let response = PassthroughJsonResponse::new(status.as_u16(), body)
            .with_error_category(category)
            .with_details(details);
        return Ok(PassthroughResponse::Json(Box::new(response)));
    }
    if stream {
        return Err(malformed_response(
            provider_request_id.as_deref(),
            "provider did not return an event stream",
        ));
    }

    let mut body: Value = serde_json::from_slice(&bytes).map_err(|e| {
        malformed_response(
            provider_request_id.as_deref(),
            &format!("malformed provider response: {e}"),
        )
    })?;
    if let Some(slot) = body.get_mut(MODEL) {
        *slot = model;
    }
    let usage = body
        .get("usage")
        .filter(|usage| usage.is_object())
        .map(|usage| response::usage(usage, true));
    let mut response = PassthroughJsonResponse::new(status.as_u16(), body);
    if let Some(usage) = usage {
        response = response.with_usage(usage);
    }
    if let Some(id) = provider_request_id {
        response = response.with_details(ProviderFailure::default().with_provider_request_id(id));
    }
    Ok(PassthroughResponse::Json(Box::new(response)))
}

/// Relays native stream events, reading what the Gateway needs from them.
struct Relay {
    /// The Gateway model identifier of the request.
    model: Value,
    /// Usage accumulated from `message_start` and `message_delta`.
    usage: Value,
}

impl Relay {
    fn on_event(&mut self, event: ServerEvent) -> PassthroughEvent {
        let ServerEvent {
            event: name,
            mut data,
            ..
        } = event;
        let mut usage = None;

        if let Ok(mut payload) = serde_json::from_str::<Value>(&data) {
            match payload.get("type").and_then(Value::as_str) {
                Some("message_start") => {
                    if let Some(message) = payload.get_mut("message") {
                        self.usage = message.get("usage").cloned().unwrap_or_default();
                        if let Some(slot) = message.get_mut(MODEL) {
                            *slot = self.model.clone();
                            data = payload.to_string();
                        }
                    }
                }
                Some("message_delta") => {
                    stream::merge_usage(&mut self.usage, payload["usage"].take());
                    usage = Some(response::usage(&self.usage, true));
                }
                _ => {}
            }
        }

        let mut relayed = PassthroughEvent::new(data);
        if let Some(name) = name {
            relayed = relayed.with_event(name);
        }
        if let Some(usage) = usage {
            relayed = relayed.with_usage(usage);
        }
        relayed
    }
}

fn interrupted(provider_request_id: Option<&str>, cause: &str) -> LlmGatewayError {
    let mut details = ProviderFailure::default();
    if let Some(id) = provider_request_id {
        details = details.with_provider_request_id(id);
    }
    LlmGatewayError::provider(
        ProviderErrorCategory::ProviderUnavailable,
        format!("provider stream interrupted: {cause}"),
        details,
    )
}
