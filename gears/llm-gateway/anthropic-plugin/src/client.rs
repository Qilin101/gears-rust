// Created: 2026-11-09 by Constructor Tech
//! [`AnthropicPlugin`]: the provider-plugin client for the Anthropic Messages
//! API, calling Anthropic through OAGW.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use http::header::{ACCEPT, CONTENT_TYPE};
use http::{Method, Uri};
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayError,
    LlmGatewayProviderPluginClientV1, MediaInputMode, PassthroughApi, PassthroughCapability,
    PassthroughEndpoint, PassthroughRequest, PassthroughResponse, ProviderCallCtx,
    ProviderErrorCategory, ProviderFailure, ProviderPluginCapabilities, ResponseEventStream,
    ResponseResource,
};
use oagw_sdk::sse::{ServerEventsResponse, ServerEventsStream};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;

use crate::error;
use crate::passthrough;
use crate::request::{self, Target};
use crate::response::{self, Envelope};
use crate::settings::AnthropicProviderSettingsV1;
use crate::stream;
use crate::wire::{
    DEFAULT_ANTHROPIC_VERSION, DEFAULT_BASE_URL, MESSAGES_PATH, Message, StreamEvent,
};

pub const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";
pub const ANTHROPIC_BETA_HEADER: &str = "anthropic-beta";
const JSON: &str = "application/json";
const EVENT_STREAM: &str = "text/event-stream";

/// Provider plugin for the Anthropic Messages API (`POST /v1/messages`), for
/// both the unified API and the Anthropic pass-through route.
///
/// Sends every request through OAGW to the model's destination: `base_url`,
/// or [`DEFAULT_BASE_URL`] when the model has none; its host and port name the
/// OAGW upstream alias. No credential is sent: the OAGW upstream authenticates
/// the call. A model with a `secret_ref` fails with `credentials_unavailable`.
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
        // OAGW cannot yet be told which credential to use.
        if info.secret_ref.is_some() {
            return Err(LlmGatewayError::CredentialsUnavailable);
        }
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
        let beta = (!settings.anthropic_beta.is_empty()).then(|| settings.anthropic_beta.join(","));
        let http_request = http_request(
            version(&settings),
            beta.as_deref(),
            info.base_url.as_deref(),
            payload,
            stream,
        )?;

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
            echo: built.echo,
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
            .with_passthrough(PassthroughCapability::new(
                PassthroughApi::Anthropic,
                vec![PassthroughEndpoint::metered(passthrough::MESSAGES_ENDPOINT)],
            ))
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
        let message: Message = serde_json::from_slice(&bytes).map_err(|e| {
            malformed_response(
                envelope.provider_request_id.as_deref(),
                &format!("malformed provider response: {e}"),
            )
        })?;
        response::to_resource(&body, &envelope, message)
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
                    let content_type = parts
                        .headers
                        .get(CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("none");
                    Err(malformed_response(
                        envelope.provider_request_id.as_deref(),
                        &format!(
                            "provider did not return an event stream (content type: {content_type})"
                        ),
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

    async fn forward_passthrough(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        req: PassthroughRequest,
    ) -> Result<PassthroughResponse, LlmGatewayError> {
        passthrough::forward(self.gateway.as_ref(), ctx, call, req).await
    }
}

/// The `anthropic-version` of the model's settings, or the default.
pub fn version(settings: &AnthropicProviderSettingsV1) -> &str {
    if settings.anthropic_version.is_empty() {
        DEFAULT_ANTHROPIC_VERSION
    } else {
        &settings.anthropic_version
    }
}

pub fn http_request(
    version: &str,
    beta: Option<&str>,
    base_url: Option<&str>,
    payload: Vec<u8>,
    stream: bool,
) -> Result<http::Request<Body>, LlmGatewayError> {
    let mut builder = http::Request::builder()
        .method(Method::POST)
        .uri(destination(base_url)?)
        .header(CONTENT_TYPE, JSON)
        .header(ACCEPT, if stream { EVENT_STREAM } else { JSON })
        .header(ANTHROPIC_VERSION_HEADER, version);
    if let Some(beta) = beta {
        builder = builder.header(ANTHROPIC_BETA_HEADER, beta);
    }
    builder
        .body(Body::Bytes(payload.into()))
        .map_err(|e| LlmGatewayError::internal_from("failed to build the provider request", e))
}

/// OAGW proxy path of the Messages endpoint for the model's `base_url`:
/// `/{alias}{path prefix}/v1/messages`. The alias is the host, with the port
/// unless it is 80 or 443.
fn destination(base_url: Option<&str>) -> Result<String, LlmGatewayError> {
    let invalid = || LlmGatewayError::internal("model base_url is not a valid URL");
    let url: Uri = base_url
        .unwrap_or(DEFAULT_BASE_URL)
        .parse()
        .map_err(|_| invalid())?;
    let host = url.host().filter(|h| !h.is_empty()).ok_or_else(invalid)?;
    let alias = match url.port_u16() {
        Some(port) if port != 80 && port != 443 => format!("{host}:{port}"),
        _ => host.to_owned(),
    };
    let prefix = url.path().trim_end_matches('/');
    Ok(format!("/{alias}{prefix}/{MESSAGES_PATH}"))
}

/// Run `future` unless the request is cancelled first.
pub async fn with_cancel<T>(
    cancel: &CancellationToken,
    future: impl Future<Output = T>,
) -> Result<T, LlmGatewayError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(cancelled()),
        value = future => Ok(value),
    }
}

pub async fn read_body(
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

pub fn malformed_response(provider_request_id: Option<&str>, message: &str) -> LlmGatewayError {
    let mut details = ProviderFailure::default();
    if let Some(id) = provider_request_id {
        details = details.with_provider_request_id(id);
    }
    LlmGatewayError::provider(ProviderErrorCategory::ProviderError, message, details)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
