// Created: 2026-11-09 by Constructor Tech
use std::fmt::Write;

use serde_json::{Value, json};

use super::*;

fn model_info(settings: &Value) -> model_registry_sdk::ModelInfoV1 {
    serde_json::from_value(json!({
        "gts_type": "gts.cf.genai.model.provider.v1~cf.genai._.anthropic.v1~",
        "provider_model_id": "claude-sonnet-5-5",
        "base_url": null,
        "secret_ref": null,
        "display_name": "Claude",
        "description": null,
        "family": null,
        "vendor": null,
        "managed": false,
        "architecture": null,
        "size_bytes": null,
        "format": null,
        "region": null,
        "hosted_by": null,
        "last_release_at": null,
        "reasoning_level": null,
        "model_version": null,
        "sort_order": 0,
        "icon": null,
        "multiplier_display": null,
        "cost": {},
        "performance": {},
        "additional_info": {},
        "supported_api": ["completion"],
        "capabilities": {
            "vision": { "enabled": true, "supported_mime_types": ["image/png"] },
            "reasoning": { "effort": true, "toggle": true, "resume": true, "budget": true },
            "tools": {
                "enabled": true, "parallel_tool_calls": true, "max_tool_calls": false,
                "tool_choice_required": true, "tool_choice_none": true, "tool_choice_named": true
            },
            "output": { "json_mode": false, "json_schema": true, "strict_schema": true, "verbosity": false },
            "sampling": {
                "temperature": true, "top_p": true, "seed": false,
                "presence_penalty": false, "frequency_penalty": false, "logprobs": false
            },
            "streaming": true,
            "file_input": { "enabled": true, "supported_mime_types": ["application/pdf"] },
            "image_generation": { "enabled": false, "supported_mime_types": [] },
            "audio_input": { "enabled": false, "supported_mime_types": [] },
            "audio_output": { "enabled": false, "supported_mime_types": [] },
            "code_interpreter": false,
            "web_search": { "enabled": true, "allowed_domains": true, "excluded_domains": true }
        },
        "disabled_capabilities": {},
        "context_window": { "max_input_tokens": 200_000, "max_output_tokens": 8192, "output_vector_size": null },
        "default_parameters": {},
        "allow_parameter_override": true,
        "allow_extra_params": [],
        "provider_settings": settings,
    }))
    .unwrap()
}

fn settings(beta: &[&str]) -> Value {
    json!({
        "oagw_alias": "anthropic",
        "anthropic_version": "",
        "anthropic_beta": beta,
        "max_tokens": null,
        "stop_sequences": null,
        "system": null,
        "inference_geo": null,
        "service_tier": null,
        "container": null,
        "thinking": null,
        "tool_choice": null,
        "output_config": null
    })
}

fn call(api_key: Option<&str>, beta: &[&str]) -> ProviderCallCtx {
    let call = ProviderCallCtx::new(
        model_info(&settings(beta)),
        "req_1",
        CancellationToken::new(),
    );
    match api_key {
        Some(key) => call.with_api_key(key.to_owned().into()),
        None => call,
    }
}

fn request() -> CreateResponseBody {
    serde_json::from_value(json!({ "model": "claude", "input": "Hi" })).unwrap()
}

/// A recorded outbound request.
struct Recorded {
    method: Method,
    uri: String,
    headers: http::HeaderMap,
    body: Value,
}

/// Fake OAGW: records the proxied request and answers with a canned response.
struct FakeGateway {
    respond: Box<dyn Fn() -> http::Response<Body> + Send + Sync>,
    recorded: std::sync::Mutex<Vec<Recorded>>,
}

impl FakeGateway {
    fn new(respond: impl Fn() -> http::Response<Body> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            respond: Box::new(respond),
            recorded: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn last(&self) -> Recorded {
        self.recorded.lock().unwrap().pop().unwrap()
    }
}

fn unused<T>() -> Result<T, toolkit_canonical_errors::CanonicalError> {
    Err(toolkit_canonical_errors::CanonicalError::internal("not used by the plugin").create())
}

#[async_trait]
impl ServiceGatewayClientV1 for FakeGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn list_upstreams(
        &self,
        _: SecurityContext,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn update_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn delete_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn create_route(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn get_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn list_routes(
        &self,
        _: SecurityContext,
        _: Option<uuid::Uuid>,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn update_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn delete_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), toolkit_canonical_errors::CanonicalError> {
        unused()
    }
    async fn resolve_proxy_target(
        &self,
        _: SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), toolkit_canonical_errors::CanonicalError>
    {
        unused()
    }
    async fn proxy_request(
        &self,
        _: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, toolkit_canonical_errors::CanonicalError> {
        let (parts, body) = req.into_parts();
        let bytes = body.into_bytes().await.unwrap();
        self.recorded.lock().unwrap().push(Recorded {
            method: parts.method,
            uri: parts.uri.to_string(),
            headers: parts.headers,
            body: serde_json::from_slice(&bytes).unwrap(),
        });
        Ok((self.respond)())
    }
}

fn json_response(status: u16, headers: &[(&str, &str)], body: &Value) -> http::Response<Body> {
    let mut builder = http::Response::builder()
        .status(status)
        .header(CONTENT_TYPE, JSON);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(Body::Bytes(serde_json::to_vec(body).unwrap().into()))
        .unwrap()
}

fn sse_response(events: &[Value]) -> http::Response<Body> {
    let payload = events.iter().fold(String::new(), |mut payload, e| {
        write!(
            payload,
            "event: {}\ndata: {e}\n\n",
            e["type"].as_str().unwrap()
        )
        .unwrap();
        payload
    });
    let chunks: Vec<Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>>> =
        vec![Ok(payload.into())];
    http::Response::builder()
        .status(200)
        .header(CONTENT_TYPE, EVENT_STREAM)
        .body(Body::Stream(Box::pin(futures::stream::iter(chunks))))
        .unwrap()
}

fn message_json() -> Value {
    json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-5-5",
        "content": [{ "type": "text", "text": "Hello!" }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": 3, "output_tokens": 2 }
    })
}

#[tokio::test]
async fn sync_call_sends_messages_request_and_maps_response() {
    let gateway =
        FakeGateway::new(|| json_response(200, &[("request-id", "req_a")], &message_json()));
    let plugin = AnthropicPlugin::new(gateway.clone());
    let resource = plugin
        .create_response(
            &SecurityContext::anonymous(),
            &call(Some("sk-test"), &["beta-a", "beta-b"]),
            request(),
        )
        .await
        .unwrap();

    let sent = gateway.last();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(sent.uri, "/anthropic/v1/messages");
    assert_eq!(sent.headers[AUTHORIZATION], "Bearer sk-test");
    assert!(sent.headers[AUTHORIZATION].is_sensitive());
    assert_eq!(
        sent.headers[ANTHROPIC_VERSION_HEADER],
        DEFAULT_ANTHROPIC_VERSION
    );
    assert_eq!(sent.headers[ANTHROPIC_BETA_HEADER], "beta-a,beta-b");
    assert_eq!(sent.headers[ACCEPT], JSON);
    assert_eq!(
        sent.body,
        json!({
            "model": "claude-sonnet-5-5",
            "max_tokens": 8192,
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Hi" }] }]
        })
    );

    assert_eq!(resource.id, "req_1");
    assert_eq!(resource.usage.as_ref().unwrap().total_tokens, 5);
    assert_eq!(
        resource
            .provider_details
            .unwrap()
            .provider_request_id
            .as_deref(),
        Some("req_a")
    );
}

#[tokio::test]
async fn no_api_key_sends_no_auth_header() {
    let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
    let plugin = AnthropicPlugin::new(gateway.clone());
    plugin
        .create_response(&SecurityContext::anonymous(), &call(None, &[]), request())
        .await
        .unwrap();
    let sent = gateway.last();
    assert!(sent.headers.get(AUTHORIZATION).is_none());
    assert!(sent.headers.get(ANTHROPIC_BETA_HEADER).is_none());
}

#[tokio::test]
async fn http_error_is_normalized() {
    let gateway = FakeGateway::new(|| {
        json_response(
            429,
            &[("retry-after", "12")],
            &json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "Too many" } }),
        )
    });
    let plugin = AnthropicPlugin::new(gateway);
    let err = plugin
        .create_response(&SecurityContext::anonymous(), &call(None, &[]), request())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "rate_limited");
    assert_eq!(err.to_response_error().retry_after, Some(12));
}

#[tokio::test]
async fn streaming_call_translates_events() {
    let gateway = FakeGateway::new(|| {
        sse_response(&[
            json!({ "type": "message_start", "message": {
                "id": "msg_01", "type": "message", "role": "assistant", "model": "claude",
                "content": [], "stop_reason": null, "usage": { "input_tokens": 3, "output_tokens": 0 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hi!" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 2 } }),
            json!({ "type": "message_stop" }),
        ])
    });
    let plugin = AnthropicPlugin::new(gateway.clone());
    let events: Vec<_> = plugin
        .create_response_stream(&SecurityContext::anonymous(), &call(None, &[]), request())
        .await
        .unwrap()
        .collect()
        .await;

    let sent = gateway.last();
    assert_eq!(sent.headers[ACCEPT], EVENT_STREAM);
    assert_eq!(sent.body["stream"], json!(true));

    let Some(llm_gateway_sdk::StreamingEvent::Completed(done)) = events.last() else {
        panic!("expected response.completed, got {:?}", events.last());
    };
    assert_eq!(
        serde_json::to_value(&done.response.output).unwrap()[0]["content"][0]["text"],
        json!("Hi!")
    );
}

#[tokio::test]
async fn streaming_error_response_is_an_outer_error() {
    let gateway = FakeGateway::new(|| {
        json_response(
            401,
            &[],
            &json!({ "type": "error", "error": { "type": "authentication_error", "message": "bad key" } }),
        )
    });
    let plugin = AnthropicPlugin::new(gateway);
    let err = plugin
        .create_response_stream(&SecurityContext::anonymous(), &call(None, &[]), request())
        .await
        .err()
        .unwrap();
    assert_eq!(err.code(), "authentication_failed");
}

#[tokio::test]
async fn cancelled_call_does_not_reach_the_provider() {
    let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
    let plugin = AnthropicPlugin::new(gateway.clone());
    let call = call(None, &[]);
    call.cancel.cancel();
    let err = plugin
        .create_response(&SecurityContext::anonymous(), &call, request())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "provider_unavailable");
    assert!(gateway.recorded.lock().unwrap().is_empty());
}

#[tokio::test]
async fn embeddings_are_not_supported() {
    let plugin = AnthropicPlugin::new(FakeGateway::new(|| {
        json_response(200, &[], &message_json())
    }));
    let req = EmbeddingRequest::new("m", llm_gateway_sdk::EmbeddingInput::Single("x".to_owned()));
    let err = plugin
        .create_embedding(&SecurityContext::anonymous(), &call(None, &[]), req)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "capability_not_supported");
}

#[test]
fn reports_streaming_and_web_search_capabilities() {
    let plugin = AnthropicPlugin::new(FakeGateway::new(|| {
        json_response(200, &[], &message_json())
    }));
    let capabilities = plugin.capabilities();
    assert!(capabilities.streaming_transport);
    assert!(capabilities.web_search);
    assert_eq!(capabilities.media_input, MediaInputMode::UrlForward);
}
