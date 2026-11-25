// Created: 2026-11-09 by Constructor Tech
use std::fmt::Write;

use http::header::AUTHORIZATION;

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
        "anthropic_version": "",
        "anthropic_beta": beta,
        "max_tokens": null,
        "stop_sequences": null,
        "system": null,
        "inference_geo": null,
        "service_tier": null,
        "thinking": null,
        "tool_choice": null,
        "output_config": null
    })
}

fn call(beta: &[&str]) -> ProviderCallCtx {
    ProviderCallCtx::new(
        model_info(&settings(beta)),
        "req_1",
        CancellationToken::new(),
    )
}

fn call_to(base_url: &str) -> ProviderCallCtx {
    let mut call = call(&[]);
    call.model_info.base_url = Some(base_url.to_owned());
    call
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
pub struct FakeGateway {
    respond: Box<dyn Fn() -> http::Response<Body> + Send + Sync>,
    recorded: std::sync::Mutex<Vec<Recorded>>,
}

impl FakeGateway {
    pub fn new(respond: impl Fn() -> http::Response<Body> + Send + Sync + 'static) -> Arc<Self> {
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
            &call(&[
                "interleaved-thinking-2025-05-14",
                "files-api-2025-04-14",
                "effort-2025-11-24",
            ]),
            request(),
        )
        .await
        .unwrap();

    let sent = gateway.last();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(sent.uri, "/api.anthropic.com/v1/messages");
    assert!(sent.headers.get(AUTHORIZATION).is_none());
    assert_eq!(
        sent.headers[ANTHROPIC_VERSION_HEADER],
        DEFAULT_ANTHROPIC_VERSION
    );
    assert_eq!(
        sent.headers[ANTHROPIC_BETA_HEADER],
        "interleaved-thinking-2025-05-14,effort-2025-11-24"
    );
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
async fn no_allowed_beta_flags_send_no_beta_header() {
    for beta in [
        &[][..],
        &["files-api-2025-04-14", "code-execution-2025-08-25"],
    ] {
        let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
        let plugin = AnthropicPlugin::new(gateway.clone());
        plugin
            .create_response(&SecurityContext::anonymous(), &call(beta), request())
            .await
            .unwrap();
        assert!(gateway.last().headers.get(ANTHROPIC_BETA_HEADER).is_none());
    }
}

#[tokio::test]
async fn request_goes_to_the_model_base_url() {
    let cases = [
        (
            "https://llm.example.com:8443/proxy/",
            "/llm.example.com:8443/proxy/v1/messages",
        ),
        (
            "https://llm.example.com:443",
            "/llm.example.com/v1/messages",
        ),
        ("https://llm.example.com", "/llm.example.com/v1/messages"),
    ];
    for (base_url, uri) in cases {
        let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
        let plugin = AnthropicPlugin::new(gateway.clone());
        plugin
            .create_response(&SecurityContext::anonymous(), &call_to(base_url), request())
            .await
            .unwrap();
        assert_eq!(gateway.last().uri, uri, "{base_url}");
    }
}

#[tokio::test]
async fn invalid_base_url_is_an_internal_error() {
    for base_url in ["not a url", "/v1"] {
        let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
        let plugin = AnthropicPlugin::new(gateway.clone());
        let err = plugin
            .create_response(&SecurityContext::anonymous(), &call_to(base_url), request())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "internal", "{base_url}");
        assert!(gateway.recorded.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn model_with_secret_ref_is_not_sent() {
    let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
    let plugin = AnthropicPlugin::new(gateway.clone());
    let mut call = call(&[]);
    call.model_info.secret_ref = Some("anthropic-key".to_owned());
    let err = plugin
        .create_response(&SecurityContext::anonymous(), &call, request())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "credentials_unavailable");
    assert!(gateway.recorded.lock().unwrap().is_empty());
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
        .create_response(&SecurityContext::anonymous(), &call(&[]), request())
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
        .create_response_stream(&SecurityContext::anonymous(), &call(&[]), request())
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
        .create_response_stream(&SecurityContext::anonymous(), &call(&[]), request())
        .await
        .err()
        .unwrap();
    assert_eq!(err.code(), "authentication_failed");
}

#[tokio::test]
async fn cancelled_call_does_not_reach_the_provider() {
    let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
    let plugin = AnthropicPlugin::new(gateway.clone());
    let call = call(&[]);
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
        .create_embedding(&SecurityContext::anonymous(), &call(&[]), req)
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

// ---------------------------------------------------------------------------
// Pass-through
// ---------------------------------------------------------------------------

fn native_body() -> Value {
    json!({
        "model": "anthropic::claude",
        "max_tokens": 16,
        "metadata": { "user_id": "u1" },
        "messages": [{ "role": "user", "content": "Hi" }]
    })
}

fn post(path: &str, body: Value) -> PassthroughRequest {
    PassthroughRequest::new(PassthroughApi::Anthropic, PassthroughMethod::Post, path)
        .with_body(body)
}

fn native_request() -> PassthroughRequest {
    post("/v1/messages", native_body())
}

async fn forward(
    gateway: &Arc<FakeGateway>,
    call: &ProviderCallCtx,
    req: PassthroughRequest,
) -> Result<PassthroughResponse, LlmGatewayError> {
    AnthropicPlugin::new(gateway.clone())
        .forward_passthrough(&SecurityContext::anonymous(), call, req)
        .await
}

fn json_of(response: PassthroughResponse) -> llm_gateway_sdk::PassthroughJsonResponse {
    match response {
        PassthroughResponse::Json(json) => *json,
        other => panic!("expected a JSON response, got {other:?}"),
    }
}

#[test]
fn declares_messages_and_count_tokens_passthrough() {
    let plugin = AnthropicPlugin::new(FakeGateway::new(|| {
        json_response(200, &[], &message_json())
    }));
    let passthrough = plugin.capabilities().passthrough.unwrap();
    assert_eq!(passthrough.api, PassthroughApi::Anthropic);
    assert_eq!(
        passthrough.endpoints,
        vec![
            PassthroughEndpoint::metered(PassthroughMethod::Post, "/v1/messages"),
            PassthroughEndpoint::not_metered(PassthroughMethod::Post, "/v1/messages/count_tokens"),
        ]
    );
}

#[tokio::test]
async fn passthrough_forwards_the_query_string() {
    let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
    let req = native_request().with_query("beta=true");
    forward(&gateway, &call(&[]), req).await.unwrap();
    assert_eq!(
        gateway.last().uri,
        "/api.anthropic.com/v1/messages?beta=true"
    );
}

#[tokio::test]
async fn passthrough_forwards_the_native_body_and_maps_the_model_back() {
    let gateway =
        FakeGateway::new(|| json_response(200, &[("request-id", "req_a")], &message_json()));
    let req = native_request()
        .with_header("anthropic-version", "2023-06-01")
        .with_header(
            "anthropic-beta",
            "interleaved-thinking-2025-05-14, files-api-2025-04-14",
        )
        .with_header("anthropic-beta", "effort-2025-11-24");
    let response = json_of(
        forward(&gateway, &call(&["settings-beta"]), req)
            .await
            .unwrap(),
    );

    let sent = gateway.last();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(sent.uri, "/api.anthropic.com/v1/messages");
    assert!(sent.headers.get(AUTHORIZATION).is_none());
    assert_eq!(sent.headers[ANTHROPIC_VERSION_HEADER], "2023-06-01");
    assert_eq!(
        sent.headers[ANTHROPIC_BETA_HEADER],
        "interleaved-thinking-2025-05-14,effort-2025-11-24"
    );
    assert_eq!(sent.headers[ACCEPT], JSON);
    let mut expected = native_body();
    expected["model"] = json!("claude-sonnet-5-5");
    assert_eq!(sent.body, expected);

    assert_eq!(response.status, 200);
    let mut expected = message_json();
    expected["model"] = json!("anthropic::claude");
    assert_eq!(response.body, expected);
    assert_eq!(response.usage.unwrap().total_tokens, 5);
    assert_eq!(response.error_category, None);
    assert_eq!(
        response.details.provider_request_id.as_deref(),
        Some("req_a")
    );
}

#[tokio::test]
async fn passthrough_without_protocol_headers_uses_the_default_version() {
    let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
    forward(&gateway, &call(&["settings-beta"]), native_request())
        .await
        .unwrap();
    let sent = gateway.last();
    assert_eq!(
        sent.headers[ANTHROPIC_VERSION_HEADER],
        DEFAULT_ANTHROPIC_VERSION
    );
    assert!(sent.headers.get(ANTHROPIC_BETA_HEADER).is_none());
}

#[tokio::test]
async fn passthrough_rejects_before_sending() {
    let mut stateful = native_body();
    stateful["container"] = json!("cnt_1");
    let mut paid = native_body();
    paid["tools"] = json!([{ "type": "web_search_20250305", "name": "web_search" }]);
    let mut no_model = native_body();
    no_model.as_object_mut().unwrap().remove("model");
    let mut stateful_count = count_tokens_body();
    stateful_count["messages"][0]["content"] =
        json!([{ "type": "document", "source": { "type": "file", "file_id": "file_1" } }]);
    let batches = post("/v1/messages/batches", native_body());
    let openai = PassthroughRequest::new(
        PassthroughApi::OpenAi,
        PassthroughMethod::Post,
        "/v1/messages",
    )
    .with_body(native_body());
    let get = PassthroughRequest::new(
        PassthroughApi::Anthropic,
        PassthroughMethod::Get,
        "/v1/messages",
    );
    let no_body = PassthroughRequest::new(
        PassthroughApi::Anthropic,
        PassthroughMethod::Post,
        "/v1/messages",
    );
    let cases = [
        (
            post("/v1/messages", stateful),
            "capability_not_supported",
            Some("container"),
        ),
        (
            post("/v1/messages", paid),
            "capability_not_supported",
            Some("tools[0]"),
        ),
        (
            post("/v1/messages", no_model),
            "validation_error",
            Some("model"),
        ),
        (
            post("/v1/messages/count_tokens", stateful_count),
            "capability_not_supported",
            Some("messages[0].content[0].source"),
        ),
        (batches, "capability_not_supported", None),
        (openai, "capability_not_supported", None),
        (get, "capability_not_supported", None),
        (no_body, "validation_error", Some("model")),
    ];
    for (req, code, param) in cases {
        let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
        let err = forward(&gateway, &call(&[]), req).await.unwrap_err();
        assert_eq!(err.code(), code, "{param:?}");
        assert_eq!(err.to_response_error().param.as_deref(), param);
        assert!(gateway.recorded.lock().unwrap().is_empty());
    }
}

fn count_tokens_body() -> Value {
    let mut body = native_body();
    body.as_object_mut().unwrap().remove("max_tokens");
    body
}

fn count_tokens_request() -> PassthroughRequest {
    post("/v1/messages/count_tokens", count_tokens_body())
}

#[tokio::test]
async fn passthrough_forwards_count_tokens_without_usage() {
    let counted = json!({ "input_tokens": 12 });
    let reply = counted.clone();
    let gateway = FakeGateway::new(move || json_response(200, &[("request-id", "req_c")], &reply));
    let response = json_of(
        forward(&gateway, &call(&[]), count_tokens_request())
            .await
            .unwrap(),
    );

    let sent = gateway.last();
    assert_eq!(sent.uri, "/api.anthropic.com/v1/messages/count_tokens");
    assert_eq!(sent.headers[ACCEPT], JSON);
    let mut expected = count_tokens_body();
    expected["model"] = json!("claude-sonnet-5-5");
    assert_eq!(sent.body, expected);

    assert_eq!(response.status, 200);
    assert_eq!(response.body, counted);
    assert!(response.usage.is_none());
    assert_eq!(
        response.details.provider_request_id.as_deref(),
        Some("req_c")
    );
}

#[tokio::test]
async fn passthrough_model_with_secret_ref_is_not_sent() {
    let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
    let mut call = call(&[]);
    call.model_info.secret_ref = Some("anthropic-key".to_owned());
    let err = forward(&gateway, &call, native_request())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "credentials_unavailable");
    assert!(gateway.recorded.lock().unwrap().is_empty());
}

#[tokio::test]
async fn passthrough_relays_a_provider_error() {
    let error =
        json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "Too many" } });
    for stream in [false, true] {
        let body = error.clone();
        let gateway = FakeGateway::new(move || json_response(429, &[("retry-after", "12")], &body));
        let req = native_request().with_stream(stream);
        let response = json_of(forward(&gateway, &call(&[]), req).await.unwrap());
        assert_eq!(response.status, 429);
        assert_eq!(response.body, error);
        assert_eq!(
            response.error_category,
            Some(ProviderErrorCategory::RateLimited)
        );
        assert_eq!(response.details.retry_after, Some(12));
        assert_eq!(response.usage, None);
    }
}

#[tokio::test]
async fn passthrough_stream_relays_native_events() {
    let events = [
        json!({ "type": "message_start", "message": {
            "id": "msg_01", "type": "message", "role": "assistant", "model": "claude-sonnet-5-5",
            "content": [], "stop_reason": null, "usage": { "input_tokens": 3, "output_tokens": 0 } } }),
        json!({ "type": "ping" }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hi!" } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"a\"" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 2 } }),
        json!({ "type": "message_stop" }),
    ];
    let reply = events.clone();
    let gateway = FakeGateway::new(move || sse_response(&reply));
    let req = native_request().with_stream(true);
    let response = forward(&gateway, &call(&[]), req).await.unwrap();
    assert_eq!(gateway.last().headers[ACCEPT], EVENT_STREAM);

    let PassthroughResponse::Stream {
        events: relayed, ..
    } = response
    else {
        panic!("expected a stream, got {response:?}");
    };
    let relayed: Vec<_> = relayed.map(Result::unwrap).collect().await;
    assert_eq!(relayed.len(), events.len());

    for (relayed, native) in relayed.iter().zip(&events) {
        assert_eq!(relayed.event.as_deref(), native["type"].as_str());
        assert!(relayed.relay);
        let mut expected = native.clone();
        if expected["type"] == "message_start" {
            expected["message"]["model"] = json!("anthropic::claude");
        }
        let data: Value = serde_json::from_str(&relayed.data).unwrap();
        assert_eq!(data, expected);
    }
    // Events without a model are relayed byte for byte.
    assert_eq!(relayed[1].data, events[1].to_string());
    let usages: Vec<_> = relayed.iter().filter_map(|e| e.usage.as_ref()).collect();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].total_tokens, 5);
    assert!(relayed[6].usage.is_some());
}

#[tokio::test]
async fn passthrough_stream_without_events_is_a_malformed_response() {
    let gateway = FakeGateway::new(|| json_response(200, &[], &message_json()));
    let req = native_request().with_stream(true);
    let err = forward(&gateway, &call(&[]), req).await.unwrap_err();
    assert_eq!(err.code(), "provider_error");
}
