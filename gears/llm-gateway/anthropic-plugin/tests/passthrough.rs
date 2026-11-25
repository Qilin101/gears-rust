// Created: 2026-11-18 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Pass-through over the recorded replies of the conformance cases: each
//! case's `anthropic_request.json` is sent as a native request and the
//! recorded reply must come back unchanged apart from `model`.

#[allow(dead_code)]
mod support;

use futures::StreamExt;
use llm_gateway_anthropic_plugin::AnthropicPlugin;
use llm_gateway_sdk::{
    LlmGatewayProviderPluginClientV1, PassthroughApi, PassthroughRequest, PassthroughResponse,
    ProviderCallCtx,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;

use support::{Case, FakeGateway, REQUEST_ID, Reply, difference};

/// Gateway model identifier the consumer sends.
const GATEWAY_MODEL: &str = "anthropic::claude";

/// The case's native request under the Gateway model id, when the reply is a
/// recorded one and the stateless guard admits the request (web search cases
/// are rejected on this route).
fn native_request(case: &Case, stream: bool) -> Option<PassthroughRequest> {
    if case.config.synthetic {
        return None;
    }
    let mut body = case.json("anthropic_request.json")?;
    let paid_tool = |tool: &Value| {
        tool["type"]
            .as_str()
            .is_some_and(|t| t.starts_with("web_search_"))
    };
    if body["tools"]
        .as_array()
        .is_some_and(|t| t.iter().any(paid_tool))
    {
        return None;
    }
    body["model"] = json!(GATEWAY_MODEL);
    if stream {
        body["stream"] = json!(true);
    }
    Some(
        PassthroughRequest::new(PassthroughApi::Anthropic, "/v1/messages", body)
            .with_stream(stream),
    )
}

async fn forward(
    case: &Case,
    req: PassthroughRequest,
    reply: Reply,
) -> (Value, PassthroughResponse) {
    let gateway = FakeGateway::new(reply);
    let plugin = AnthropicPlugin::new(gateway.clone());
    let call = ProviderCallCtx::new(case.model_info(), REQUEST_ID, CancellationToken::new());
    let response = plugin
        .forward_passthrough(&SecurityContext::anonymous(), &call, req)
        .await
        .unwrap_or_else(|e| panic!("{}: {e}", case.name));
    let sent = gateway.sent().expect("a provider request").body;
    (sent, response)
}

/// `value` with the provider model id replaced by the Gateway one.
fn with_gateway_model(mut value: Value, model: &str) -> Value {
    if value["model"] == json!(model) {
        value["model"] = json!(GATEWAY_MODEL);
    }
    value
}

fn sse_events(sse: &[u8]) -> Vec<(Option<String>, String)> {
    let text = String::from_utf8(sse.to_vec())
        .unwrap()
        .replace("\r\n", "\n");
    text.split("\n\n")
        .filter_map(|frame| {
            let field = |name: &str| {
                frame
                    .lines()
                    .find_map(|line| line.strip_prefix(name))
                    .map(|v| v.trim_start().to_owned())
            };
            Some((field("event:"), field("data:")?))
        })
        .collect()
}

#[tokio::test]
async fn sync_replies_are_relayed() {
    let mut failures = Vec::new();
    let mut relayed = 0;
    for case in support::cases() {
        let (Some(req), Some(meta)) = (native_request(&case, false), case.json("reply.json"))
        else {
            continue;
        };
        let native = req.body.clone();
        let (sent, response) = forward(&case, req, Reply::from_file(&meta, None)).await;
        let PassthroughResponse::Json(response) = response else {
            panic!("{}: expected a JSON response", case.name);
        };
        relayed += 1;

        let model = case.model_info().provider_model_id;
        let mut expected_sent = native;
        expected_sent["model"] = json!(model);
        let expected_body = with_gateway_model(meta["body"].clone(), &model);
        let mut diffs = vec![
            difference(&expected_sent, &sent, "sent"),
            difference(&expected_body, &response.body, "body"),
            difference(&meta["status"], &json!(response.status), "status"),
        ];
        if let Some(expected) = case.json("response.json") {
            let usage = serde_json::to_value(&response.usage).unwrap();
            diffs.push(difference(&expected["usage"], &usage, "usage"));
        } else {
            assert!(response.error_category.is_some(), "{}", case.name);
        }
        failures.extend(
            diffs
                .into_iter()
                .flatten()
                .map(|d| format!("{}: {d}", case.name)),
        );
    }
    assert!(relayed > 50, "only {relayed} cases were relayed");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn stream_replies_are_relayed() {
    let mut failures = Vec::new();
    let mut relayed = 0;
    for case in support::cases() {
        let (Some(req), Some(meta), Ok(sse)) = (
            native_request(&case, true),
            case.json("stream_reply.json"),
            std::fs::read(case.path("stream_reply.sse")),
        ) else {
            continue;
        };
        let reply = Reply::from_file(&meta, Some(sse.clone())).rechunked(7);
        let (_, response) = forward(&case, req, reply).await;
        let PassthroughResponse::Stream { events, .. } = response else {
            panic!("{}: expected a stream", case.name);
        };
        let events: Vec<_> = events.map(Result::unwrap).collect().await;
        relayed += 1;

        let native = sse_events(&sse);
        assert_eq!(events.len(), native.len(), "{}", case.name);
        let model = case.model_info().provider_model_id;
        for (event, (name, data)) in events.iter().zip(&native) {
            assert_eq!(&event.event, name, "{}", case.name);
            let mut expected: Value = serde_json::from_str(data).unwrap();
            if expected["type"] == "message_start" {
                expected["message"] = with_gateway_model(expected["message"].take(), &model);
            } else {
                assert_eq!(&event.data, data, "{}", case.name);
            }
            let actual: Value = serde_json::from_str(&event.data).unwrap();
            failures.extend(
                difference(&expected, &actual, "event").map(|d| format!("{}: {d}", case.name)),
            );
        }

        // The unified stream of the same reply ends with the same usage.
        let unified = case.json("events.json").unwrap_or_default();
        let last = unified
            .as_array()
            .and_then(|e| e.last())
            .cloned()
            .unwrap_or_default();
        if last["type"] == "response.completed" {
            let usage = events.iter().rev().find_map(|e| e.usage.as_ref());
            let usage = serde_json::to_value(usage).unwrap();
            failures.extend(
                difference(&last["response"]["usage"], &usage, "usage")
                    .map(|d| format!("{}: {d}", case.name)),
            );
        }
    }
    assert!(relayed > 50, "only {relayed} cases were relayed");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
