// Created: 2026-11-09 by Constructor Tech
//! [`AnthropicPlugin`]: the provider-plugin client for the Anthropic Messages
//! API, calling Anthropic through OAGW.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderValue, Method};
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayError,
    LlmGatewayProviderPluginClientV1, MediaInputMode, ProviderCallCtx, ProviderErrorCategory,
    ProviderFailure, ProviderPluginCapabilities, ResponseEventStream, ResponseResource,
};
use oagw_sdk::sse::{ServerEventsResponse, ServerEventsStream};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use secrecy::ExposeSecret;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;

use crate::error;
use crate::request::{self, Target};
use crate::response::{self, Envelope};
use crate::settings::AnthropicProviderSettingsV1;
use crate::stream;
use crate::wire::{DEFAULT_ANTHROPIC_VERSION, MESSAGES_PATH, Message, StreamEvent};

const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";
const ANTHROPIC_BETA_HEADER: &str = "anthropic-beta";
const JSON: &str = "application/json";
const EVENT_STREAM: &str = "text/event-stream";

/// Provider plugin for the Anthropic Messages API (`POST /v1/messages`).
///
/// Sends every request through OAGW to the model's upstream alias
/// (`provider_settings.oagw_alias`). The API key from [`ProviderCallCtx`] is
/// sent as `Authorization: Bearer`; without a key no auth header is sent and
/// OAGW is expected to authenticate the upstream call.
#[derive(Clone)]
pub struct AnthropicPlugin {
    gateway: Arc<dyn ServiceGatewayClientV1>,
}

impl std::fmt::Debug for AnthropicPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicPlugin").finish_non_exhaustive()
    }
}

/// A sent request and what the response needs from it.
struct Sent {
    response: http::Response<Body>,
    envelope: Envelope,
}

impl AnthropicPlugin {
    #[must_use]
    pub fn new(gateway: Arc<dyn ServiceGatewayClientV1>) -> Self {
        Self { gateway }
    }

    async fn send(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        body: &CreateResponseBody,
        stream: bool,
    ) -> Result<Sent, LlmGatewayError> {
        let created_at = response::now_unix();
        let settings: AnthropicProviderSettingsV1 = call
            .provider_settings()
            .map_err(|e| LlmGatewayError::internal_from("model is not an Anthropic model", e))?;
        let info = &call.model_info;
        let target = Target {
            provider_model_id: &info.provider_model_id,
            max_output_tokens: info.context_window.max_output_tokens,
            settings: &settings,
        };
        let built = request::build(&target, body, stream)?;
        let payload = built
            .request
            .to_body()
            .and_then(|b| serde_json::to_vec(&b))
            .map_err(|e| LlmGatewayError::internal_from("failed to serialize the request", e))?;
        let http_request = http_request(&settings, call, payload, stream)?;

        let response = with_cancel(
            &call.cancel,
            self.gateway.proxy_request(ctx.clone(), http_request),
        )
        .await?
        .map_err(error::from_gateway)?;

        let envelope = Envelope {
            request_id: call.request_id.clone(),
            created_at,
            ignored_parameters: built.ignored_parameters,
            provider_request_id: error::request_id(response.headers()),
        };
        Ok(Sent { response, envelope })
    }
}

#[async_trait]
impl LlmGatewayProviderPluginClientV1 for AnthropicPlugin {
    fn capabilities(&self) -> ProviderPluginCapabilities {
        ProviderPluginCapabilities::new(MediaInputMode::UrlForward)
            .with_streaming_transport()
            .with_web_search()
    }

    async fn create_response(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError> {
        let Sent { response, envelope } = self.send(ctx, call, &body, false).await?;
        let (parts, response_body) = response.into_parts();
        let bytes = read_body(&call.cancel, response_body).await?;
        if !parts.status.is_success() {
            return Err(error::from_http(parts.status, &parts.headers, &bytes));
        }
        let message: Message = serde_json::from_slice(&bytes)
            .map_err(|_| malformed_response(&envelope, "malformed provider response"))?;
        Ok(response::to_resource(&body, &envelope, message))
    }

    async fn create_response_stream(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        let Sent { response, envelope } = self.send(ctx, call, &body, true).await?;
        match ServerEventsStream::from_response::<StreamEvent>(response) {
            ServerEventsResponse::Events(events) => {
                let events = stream::translate(events, &body, envelope)
                    .take_until(call.cancel.clone().cancelled_owned());
                Ok(Box::pin(events))
            }
            ServerEventsResponse::Response(response) => {
                let (parts, response_body) = response.into_parts();
                let bytes = read_body(&call.cancel, response_body).await?;
                if parts.status.is_success() {
                    Err(malformed_response(
                        &envelope,
                        "provider did not return an event stream",
                    ))
                } else {
                    Err(error::from_http(parts.status, &parts.headers, &bytes))
                }
            }
        }
    }

    async fn create_embedding(
        &self,
        _ctx: &SecurityContext,
        _call: &ProviderCallCtx,
        _req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        Err(LlmGatewayError::capability_not_supported("embeddings"))
    }
}

fn http_request(
    settings: &AnthropicProviderSettingsV1,
    call: &ProviderCallCtx,
    payload: Vec<u8>,
    stream: bool,
) -> Result<http::Request<Body>, LlmGatewayError> {
    let version = if settings.anthropic_version.is_empty() {
        DEFAULT_ANTHROPIC_VERSION
    } else {
        &settings.anthropic_version
    };
    let mut builder = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/{}/{MESSAGES_PATH}", settings.oagw_alias))
        .header(CONTENT_TYPE, JSON)
        .header(ACCEPT, if stream { EVENT_STREAM } else { JSON })
        .header(ANTHROPIC_VERSION_HEADER, version);
    if !settings.anthropic_beta.is_empty() {
        builder = builder.header(ANTHROPIC_BETA_HEADER, settings.anthropic_beta.join(","));
    }
    if let Some(key) = &call.api_key {
        let mut value =
            HeaderValue::try_from(format!("Bearer {}", key.expose_secret())).map_err(|_| {
                LlmGatewayError::internal("provider API key is not a valid header value")
            })?;
        value.set_sensitive(true);
        builder = builder.header(AUTHORIZATION, value);
    }
    builder
        .body(Body::Bytes(payload.into()))
        .map_err(|e| LlmGatewayError::internal_from("failed to build the provider request", e))
}

/// Run `future` unless the request is cancelled first.
async fn with_cancel<T>(
    cancel: &CancellationToken,
    future: impl Future<Output = T>,
) -> Result<T, LlmGatewayError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(cancelled()),
        value = future => Ok(value),
    }
}

async fn read_body(
    cancel: &CancellationToken,
    body: Body,
) -> Result<bytes::Bytes, LlmGatewayError> {
    with_cancel(cancel, body.into_bytes()).await?.map_err(|e| {
        LlmGatewayError::provider(
            ProviderErrorCategory::ProviderUnavailable,
            format!("failed to read the provider response: {e}"),
            ProviderFailure::default(),
        )
    })
}

fn cancelled() -> LlmGatewayError {
    LlmGatewayError::provider(
        ProviderErrorCategory::ProviderUnavailable,
        "provider call cancelled",
        ProviderFailure::default(),
    )
}

fn malformed_response(envelope: &Envelope, message: &str) -> LlmGatewayError {
    let mut details = ProviderFailure::default();
    if let Some(id) = &envelope.provider_request_id {
        details = details.with_provider_request_id(id.clone());
    }
    LlmGatewayError::provider(ProviderErrorCategory::ProviderError, message, details)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
