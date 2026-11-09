// Created: 2026-11-09 by Constructor Tech
use crate::settings::AnthropicOutputConfig;

use super::*;

fn settings() -> AnthropicProviderSettingsV1 {
    AnthropicProviderSettingsV1 {
        oagw_alias: "anthropic".to_owned(),
        ..AnthropicProviderSettingsV1::default()
    }
}

fn body(value: Value) -> CreateResponseBody {
    serde_json::from_value(value).unwrap()
}

fn build_with(
    settings: &AnthropicProviderSettingsV1,
    body_json: Value,
) -> Result<Built, LlmGatewayError> {
    let target = Target {
        provider_model_id: "claude-sonnet-5-5",
        max_output_tokens: Some(8192),
        settings,
    };
    build(&target, &body(body_json), false)
}

fn wire(settings: &AnthropicProviderSettingsV1, body_json: Value) -> Value {
    build_with(settings, body_json)
        .unwrap()
        .request
        .to_body()
        .unwrap()
}

#[test]
fn text_input_with_instructions() {
    let wire = wire(
        &settings(),
        json!({ "model": "m", "input": "Hello", "instructions": "Be brief", "temperature": 0.2 }),
    );
    assert_eq!(
        wire,
        json!({
            "model": "claude-sonnet-5-5",
            "max_tokens": 8192,
            "system": [{ "type": "text", "text": "Be brief" }],
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Hello" }] }],
            "temperature": 0.2
        })
    );
}

#[test]
fn settings_fill_defaults_and_request_overrides() {
    let settings = AnthropicProviderSettingsV1 {
        temperature: Some(0.5),
        top_k: Some(40),
        max_tokens: Some(1024),
        stop_sequences: Some(vec!["END".to_owned()]),
        system: Some("Default system".to_owned()),
        ..settings()
    };
    let wire = wire(
        &settings,
        json!({ "model": "m", "input": "Hi", "temperature": 0.9, "max_output_tokens": 256 }),
    );
    assert_eq!(wire["temperature"], json!(0.9));
    assert_eq!(wire["max_tokens"], json!(256));
    assert_eq!(wire["top_k"], json!(40));
    assert_eq!(wire["stop_sequences"], json!(["END"]));
    assert_eq!(
        wire["system"],
        json!([{ "type": "text", "text": "Default system" }])
    );
}

#[test]
fn missing_max_tokens_is_a_validation_error() {
    let settings = settings();
    let target = Target {
        provider_model_id: "claude",
        max_output_tokens: None,
        settings: &settings,
    };
    let err = build(
        &target,
        &body(json!({ "model": "m", "input": "Hi" })),
        false,
    )
    .unwrap_err();
    assert_eq!(err.code(), "validation_error");
}

#[test]
fn conversation_merges_roles_and_maps_tools_and_media() {
    let wire = wire(
        &settings(),
        json!({
            "model": "m",
            "input": [
                { "type": "message", "role": "developer", "content": "Use tools" },
                { "type": "message", "role": "user", "content": [
                    { "type": "input_text", "text": "What is this?" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAAA" },
                    { "type": "input_file", "filename": "a.pdf", "file_url": "https://x.test/a.pdf" }
                ]},
                { "type": "message", "role": "assistant", "content": [
                    { "type": "output_text", "text": "Let me check.", "annotations": [] }
                ]},
                { "type": "function_call", "call_id": "toolu_1", "name": "lookup", "arguments": "{\"q\":\"x\"}" },
                { "type": "function_call_output", "call_id": "toolu_1", "output": "42" }
            ]
        }),
    );
    assert_eq!(
        wire["system"],
        json!([{ "type": "text", "text": "Use tools" }])
    );
    assert_eq!(
        wire["messages"],
        json!([
            { "role": "user", "content": [
                { "type": "text", "text": "What is this?" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } },
                { "type": "document", "source": { "type": "url", "url": "https://x.test/a.pdf" }, "title": "a.pdf" }
            ]},
            { "role": "assistant", "content": [
                { "type": "text", "text": "Let me check." },
                { "type": "tool_use", "id": "toolu_1", "name": "lookup", "input": { "q": "x" } }
            ]},
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": [{ "type": "text", "text": "42" }] }
            ]}
        ])
    );
}

#[test]
fn reasoning_items_round_trip_to_thinking_blocks() {
    let wire = wire(
        &settings(),
        json!({
            "model": "m",
            "input": [
                { "type": "message", "role": "user", "content": "Hi" },
                { "type": "reasoning", "summary": [],
                  "content": [{ "type": "reasoning_text", "text": "hmm" }], "encrypted_content": "sig" },
                { "type": "reasoning", "summary": [], "encrypted_content": "opaque" },
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "foreign" }] }
            ]
        }),
    );
    assert_eq!(
        wire["messages"][1]["content"],
        json!([
            { "type": "thinking", "thinking": "hmm", "signature": "sig" },
            { "type": "redacted_thinking", "data": "opaque" }
        ])
    );
}

#[test]
fn provider_items_and_tools_pass_through() {
    let wire = wire(
        &settings(),
        json!({
            "model": "m",
            "input": [
                { "type": "message", "role": "user", "content": "Fetch it" },
                { "type": "anthropic:server_tool_use", "id": "srv_1", "name": "web_fetch", "input": {} },
                { "type": "cf_gears:web_search_call", "id": "ws_1", "status": "completed" }
            ],
            "tools": [
                { "type": "anthropic:web_fetch_20250910", "name": "web_fetch", "max_uses": 2 }
            ]
        }),
    );
    assert_eq!(
        wire["messages"][1],
        json!({ "role": "assistant", "content": [
            { "type": "server_tool_use", "id": "srv_1", "name": "web_fetch", "input": {} }
        ]})
    );
    assert_eq!(
        wire["tools"],
        json!([{ "type": "web_fetch_20250910", "name": "web_fetch", "max_uses": 2 }])
    );
}

#[test]
fn function_and_web_search_tools_with_choice() {
    let wire = wire(
        &settings(),
        json!({
            "model": "m",
            "input": "Hi",
            "tools": [
                { "type": "function", "name": "lookup", "description": "Find", "parameters": { "type": "object" } },
                { "type": "cf_gears:web_search", "allowed_domains": ["docs.rs"], "max_uses": 3, "search_context_size": "high" }
            ],
            "tool_choice": "required",
            "parallel_tool_calls": false
        }),
    );
    assert_eq!(
        wire["tools"],
        json!([
            { "name": "lookup", "description": "Find", "input_schema": { "type": "object" } },
            { "type": "web_search_20250305", "name": "web_search", "allowed_domains": ["docs.rs"], "max_uses": 3 }
        ])
    );
    assert_eq!(
        wire["tool_choice"],
        json!({ "type": "any", "disable_parallel_tool_use": true })
    );
}

#[test]
fn tool_choice_is_omitted_without_tools() {
    let wire = wire(
        &settings(),
        json!({ "model": "m", "input": "Hi", "tool_choice": "auto" }),
    );
    assert!(wire.get("tool_choice").is_none());
}

#[test]
fn reasoning_and_structured_output() {
    let out = wire(
        &settings(),
        json!({
            "model": "m",
            "input": "Hi",
            "reasoning": { "effort": "high", "summary": "auto" },
            "text": { "format": { "type": "json_schema", "name": "r", "schema": { "type": "object" }, "strict": true } }
        }),
    );
    assert_eq!(
        out["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(
        out["output_config"],
        json!({ "effort": "high", "format": { "type": "json_schema", "schema": { "type": "object" } } })
    );

    let budget = wire(
        &settings(),
        json!({ "model": "m", "input": "Hi", "reasoning": { "budget_tokens": 2048 } }),
    );
    assert_eq!(
        budget["thinking"],
        json!({ "type": "enabled", "budget_tokens": 2048 })
    );

    let disabled = wire(
        &settings(),
        json!({ "model": "m", "input": "Hi", "reasoning": { "effort": "none" } }),
    );
    assert_eq!(disabled["thinking"], json!({ "type": "disabled" }));
}

#[test]
fn settings_reasoning_defaults_apply_without_request_reasoning() {
    let settings = AnthropicProviderSettingsV1 {
        thinking: Some(AnthropicThinking::Adaptive { display: None }),
        output_config: Some(AnthropicOutputConfig {
            effort: Some(AnthropicOutputEffort::Low),
            format: None,
        }),
        ..settings()
    };
    let wire = wire(&settings, json!({ "model": "m", "input": "Hi" }));
    assert_eq!(wire["thinking"], json!({ "type": "adaptive" }));
    assert_eq!(wire["output_config"], json!({ "effort": "low" }));
}

#[test]
fn misc_parameter_mappings() {
    let wire = wire(
        &settings(),
        json!({
            "model": "m",
            "input": "Hi",
            "safety_identifier": "user-1",
            "service_tier": "default",
            "prompt_cache_key": "k",
            "extra_fields": { "top_k": 3 }
        }),
    );
    assert_eq!(wire["metadata"], json!({ "user_id": "user-1" }));
    assert_eq!(wire["service_tier"], json!("standard_only"));
    assert_eq!(wire["cache_control"], json!({ "type": "ephemeral" }));
    assert_eq!(wire["top_k"], json!(3));
}

#[test]
fn unsupported_parameters_reject_or_drop() {
    let err = build_with(
        &settings(),
        json!({ "model": "m", "input": "Hi", "seed": 7 }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "unsupported_parameter");

    let built = build_with(
        &settings(),
        json!({ "model": "m", "input": "Hi", "seed": 7, "presence_penalty": 0.1, "drop_unsupported_params": true }),
    )
    .unwrap();
    assert_eq!(built.ignored_parameters, vec!["seed", "presence_penalty"]);
}

#[test]
fn unsupported_inputs_are_rejected() {
    let cases = [
        (
            json!({ "type": "item_reference", "id": "x" }),
            "capability_not_supported",
        ),
        (
            json!({ "type": "message", "role": "user", "content": [
                { "type": "input_audio", "data": "AAAA", "format": "wav" }
            ]}),
            "capability_not_supported",
        ),
        (
            json!({ "type": "message", "role": "user", "content": [
                { "type": "input_image", "file_id": "f1" }
            ]}),
            "capability_not_supported",
        ),
        (
            json!({ "type": "openai:web_search_call", "id": "x" }),
            "validation_error",
        ),
        (
            json!({ "type": "function_call", "call_id": "c", "name": "f", "arguments": "{bad" }),
            "validation_error",
        ),
    ];
    for (item, code) in cases {
        let err = build_with(&settings(), json!({ "model": "m", "input": [item] })).unwrap_err();
        assert_eq!(err.code(), code, "{item}");
    }
}

#[test]
fn json_object_format_is_not_supported() {
    let err = build_with(
        &settings(),
        json!({ "model": "m", "input": "Hi", "text": { "format": { "type": "json_object" } } }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "capability_not_supported");
}

#[test]
fn stream_flag_is_set_only_for_streaming() {
    let settings = settings();
    let target = Target {
        provider_model_id: "claude",
        max_output_tokens: Some(16),
        settings: &settings,
    };
    let request = build(&target, &body(json!({ "model": "m", "input": "Hi" })), true)
        .unwrap()
        .request;
    assert_eq!(request.stream, Some(true));
}
