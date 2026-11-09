// Created: 2026-11-09 by Constructor Tech
use llm_gateway_sdk::ReasoningOutput;

use super::*;

fn envelope() -> Envelope {
    Envelope {
        request_id: "req".to_owned(),
        created_at: 1_700_000_000,
        ignored_parameters: vec!["seed".to_owned()],
        provider_request_id: Some("req_anthropic".to_owned()),
    }
}

fn request() -> CreateResponseBody {
    serde_json::from_value(json!({ "model": "claude", "instructions": "Be brief" })).unwrap()
}

fn message(content: &Value, stop_reason: &str) -> Message {
    serde_json::from_value(json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-5-5",
        "content": content,
        "stop_reason": stop_reason,
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20,
            "cache_creation_input_tokens": 3,
            "cache_read_input_tokens": 5,
            "output_tokens_details": { "thinking_tokens": 7 }
        }
    }))
    .unwrap()
}

#[test]
fn text_and_tool_use_become_message_and_function_call() {
    let resource = to_resource(
        &request(),
        &envelope(),
        message(
            &json!([
                { "type": "text", "text": "Hello " },
                { "type": "text", "text": "world" },
                { "type": "tool_use", "id": "toolu_1", "name": "lookup", "input": { "q": "x" } }
            ]),
            "tool_use",
        ),
    );
    assert_eq!(resource.id, "req");
    assert_eq!(resource.model, "claude");
    assert_eq!(resource.instructions.as_deref(), Some("Be brief"));
    assert_eq!(resource.status, ResponseStatus::Completed);
    assert_eq!(resource.ignored_parameters, vec!["seed".to_owned()]);
    assert_eq!(
        serde_json::to_value(&resource.output).unwrap(),
        json!([
            { "type": "message", "id": "msg_req_0", "status": "completed", "role": "assistant", "content": [
                { "type": "output_text", "text": "Hello ", "annotations": [] },
                { "type": "output_text", "text": "world", "annotations": [] }
            ]},
            { "type": "function_call", "id": "fc_req_1", "status": "completed",
              "call_id": "toolu_1", "name": "lookup", "arguments": "{\"q\":\"x\"}" }
        ])
    );
    let details = resource.provider_details.unwrap();
    assert_eq!(details.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        details.provider_request_id.as_deref(),
        Some("req_anthropic")
    );
}

#[test]
fn usage_counts_cache_tokens_as_input() {
    let resource = to_resource(&request(), &envelope(), message(&json!([]), "end_turn"));
    let usage = resource.usage.unwrap();
    assert_eq!(usage.input_tokens, 18);
    assert_eq!(usage.output_tokens, 20);
    assert_eq!(usage.total_tokens, 38);
    assert_eq!(usage.input_tokens_details.cached_tokens, 5);
    assert_eq!(usage.output_tokens_details.reasoning_tokens, 7);
    assert_eq!(usage.cost, Decimal::ZERO);
    assert!(usage.complete);
    assert_eq!(
        usage.provider_raw.unwrap()["cache_read_input_tokens"],
        json!(5)
    );
}

#[test]
fn stop_reasons_map_to_status() {
    let cases = [
        ("end_turn", ResponseStatus::Completed, None),
        ("stop_sequence", ResponseStatus::Completed, None),
        ("pause_turn", ResponseStatus::Completed, None),
        (
            "max_tokens",
            ResponseStatus::Incomplete,
            Some(IncompleteDetails::MAX_OUTPUT_TOKENS),
        ),
        (
            "model_context_window_exceeded",
            ResponseStatus::Incomplete,
            Some(IncompleteDetails::MAX_OUTPUT_TOKENS),
        ),
        (
            "refusal",
            ResponseStatus::Incomplete,
            Some(IncompleteDetails::CONTENT_FILTER),
        ),
    ];
    for (stop_reason, status, reason) in cases {
        let (actual, details) = status_for(Some(stop_reason));
        assert_eq!(actual, status, "{stop_reason}");
        assert_eq!(
            details.map(|d| d.reason).as_deref(),
            reason,
            "{stop_reason}"
        );
    }
}

#[test]
fn thinking_blocks_become_reasoning_items() {
    let resource = to_resource(
        &request(),
        &envelope(),
        message(
            &json!([
                { "type": "thinking", "thinking": "Let me think", "signature": "sig" },
                { "type": "redacted_thinking", "data": "opaque" }
            ]),
            "end_turn",
        ),
    );
    assert_eq!(
        resource.output,
        vec![
            OutputItem::Reasoning(ReasoningOutput {
                id: "rs_req_0".to_owned(),
                summary: None,
                content: Some(vec![ReasoningContentPart::ReasoningText {
                    text: "Let me think".to_owned()
                }]),
                encrypted_content: Some("sig".to_owned()),
            }),
            OutputItem::Reasoning(ReasoningOutput {
                id: "rs_req_1".to_owned(),
                summary: None,
                content: None,
                encrypted_content: Some("opaque".to_owned()),
            }),
        ]
    );
}

#[test]
fn web_search_is_normalized_with_citations() {
    let resource = to_resource(
        &request(),
        &envelope(),
        message(
            &json!([
                { "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": { "query": "rust serde" } },
                { "type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [
                    { "type": "web_search_result", "url": "https://serde.rs", "title": "Serde", "encrypted_content": "e" }
                ]},
                { "type": "text", "text": "Serde rocks", "citations": [
                    { "type": "web_search_result_location", "url": "https://serde.rs", "title": "Serde",
                      "cited_text": "Serde", "encrypted_index": "i" },
                    { "type": "char_location", "cited_text": "x", "document_index": 0,
                      "start_char_index": 0, "end_char_index": 1 }
                ]}
            ]),
            "end_turn",
        ),
    );
    let output = serde_json::to_value(&resource.output).unwrap();
    assert_eq!(
        output[0],
        json!({
            "type": "cf_gears:web_search_call", "id": "ws_req_0", "status": "completed",
            "queries": ["rust serde"], "results": [{ "url": "https://serde.rs", "title": "Serde" }]
        })
    );
    assert_eq!(output.as_array().unwrap().len(), 2);
    assert_eq!(
        output[1]["content"][0]["annotations"],
        json!([
            { "type": "url_citation", "url": "https://serde.rs", "title": "Serde", "start_index": 0, "end_index": 11 },
            { "type": "anthropic:char_location", "cited_text": "x", "document_index": 0,
              "start_char_index": 0, "end_char_index": 1 }
        ])
    );
}

#[test]
fn unknown_blocks_are_preserved_with_prefix() {
    let resource = to_resource(
        &request(),
        &envelope(),
        message(
            &json!([{ "type": "server_tool_use", "id": "s1", "name": "web_fetch", "input": { "url": "https://x.test" } }]),
            "end_turn",
        ),
    );
    assert_eq!(
        serde_json::to_value(&resource.output).unwrap(),
        json!([{ "type": "anthropic:server_tool_use", "id": "s1", "name": "web_fetch", "input": { "url": "https://x.test" } }])
    );
}

#[test]
fn estimated_usage_without_counts_is_unknown() {
    let usage = usage(&Value::Null, false);
    assert!(!usage.complete);
    assert!(!usage.estimated);
    let usage = super::usage(&json!({ "input_tokens": 4 }), false);
    assert!(usage.estimated);
    assert_eq!(usage.input_tokens, 4);
}
