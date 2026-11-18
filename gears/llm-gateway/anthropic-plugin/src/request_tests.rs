// Created: 2026-11-09 by Constructor Tech
use crate::settings::AnthropicOutputConfig;

use super::*;

fn settings() -> AnthropicProviderSettingsV1 {
    AnthropicProviderSettingsV1::default()
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
        json!({ "model": "m", "input": "Hello", "instructions": "Be brief" }),
    );
    assert_eq!(
        wire,
        json!({
            "model": "claude-sonnet-5-5",
            "max_tokens": 8192,
            "system": [{ "type": "text", "text": "Be brief" }],
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Hello" }] }]
        })
    );
}

#[test]
fn settings_fill_defaults_and_request_overrides() {
    let settings = AnthropicProviderSettingsV1 {
        max_tokens: Some(1024),
        stop_sequences: Some(vec!["END".to_owned()]),
        system: Some("Default system".to_owned()),
        ..settings()
    };
    let wire = wire(
        &settings,
        json!({ "model": "m", "input": "Hi", "max_output_tokens": 256 }),
    );
    assert_eq!(wire["max_tokens"], json!(256));
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
                { "type": "anthropic:bash_20250124", "name": "bash" }
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
        json!([{ "type": "bash_20250124", "name": "bash" }])
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
                { "type": "cf_gears:web_search", "allowed_domains": ["docs.rs"], "max_uses": 3 }
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
        json!({ "effort": "high", "format": { "type": "json_schema", "schema":
            { "type": "object", "properties": {}, "additionalProperties": false } } })
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
fn summary_alone_requests_summarized_thinking() {
    let request = json!({ "model": "m", "input": "Hi", "reasoning": { "summary": "auto" } });
    let out = wire(&settings(), request.clone());
    assert_eq!(
        out["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );

    let budget = AnthropicProviderSettingsV1 {
        thinking: Some(AnthropicThinking::Enabled {
            budget_tokens: 1024,
            display: None,
        }),
        ..settings()
    };
    assert_eq!(
        wire(&budget, request.clone())["thinking"],
        json!({ "type": "enabled", "budget_tokens": 1024, "display": "summarized" })
    );

    let disabled = AnthropicProviderSettingsV1 {
        thinking: Some(AnthropicThinking::Disabled),
        ..settings()
    };
    assert_eq!(
        wire(&disabled, request)["thinking"],
        json!({ "type": "disabled" })
    );
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
        json!({ "model": "m", "input": "Hi", "temperature": 0.2, "top_p": 0.9, "seed": 7, "presence_penalty": 0.1, "drop_unsupported_params": true }),
    )
    .unwrap();
    assert_eq!(
        built.ignored_parameters,
        vec!["temperature", "top_p", "seed", "presence_penalty"]
    );
    let wire = built.request.to_body().unwrap();
    assert!(wire.get("temperature").is_none() && wire.get("top_p").is_none());
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
fn inexpressible_output_schema_is_unsupported() {
    let err = build_with(
        &settings(),
        json!({ "model": "m", "input": "Hi", "text": { "format":
            { "type": "json_schema", "name": "r", "schema": { "properties": {} }, "strict": true } } }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "unsupported_schema");
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

#[test]
fn extra_fields_cannot_set_built_fields() {
    let err = build_with(
        &settings(),
        json!({ "model": "m", "input": "Hi", "extra_fields": { "max_tokens": 32 } }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "validation_error");
    assert_eq!(
        err.to_response_error().param.as_deref(),
        Some("extra_fields.max_tokens")
    );

    let wire = wire(
        &settings(),
        json!({ "model": "m", "input": "Hi",
            "extra_fields": { "diagnostics": { "previous_message_id": null } } }),
    );
    assert_eq!(wire["diagnostics"], json!({ "previous_message_id": null }));
}

#[test]
fn only_client_run_builtin_tools_are_forwarded() {
    let err = build_with(
        &settings(),
        json!({ "model": "m", "input": "Hi", "tools": [
            { "type": "anthropic:code_execution_20250825", "name": "code_execution" }
        ] }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "capability_not_supported");
}

#[test]
fn options_without_an_equivalent_are_unsupported_parameters() {
    let cases = [
        (
            json!({ "text": { "format": { "type": "text" }, "verbosity": "low" } }),
            "text.verbosity",
        ),
        (
            json!({ "tools": [{ "type": "cf_gears:web_search", "search_context_size": "high" }] }),
            "tools[0].search_context_size",
        ),
        (
            json!({ "input": [{ "type": "message", "role": "user", "content": [
                { "type": "input_image", "image_url": "https://x.test/a.png", "detail": "high" }
            ] }] }),
            "input[0].content[0].detail",
        ),
    ];
    for (extra, param) in cases {
        let mut request = json!({ "model": "m", "input": "Hi" });
        merge(&mut request, extra.clone());
        let err = build_with(&settings(), request.clone()).unwrap_err();
        assert_eq!(err.code(), "unsupported_parameter", "{param}");
        assert_eq!(err.to_response_error().param.as_deref(), Some(param));

        request["drop_unsupported_params"] = json!(true);
        let built = build_with(&settings(), request).unwrap();
        assert_eq!(built.ignored_parameters, vec![param.to_owned()]);
    }
}

fn merge(base: &mut Value, patch: Value) {
    if let (Some(base), Value::Object(patch)) = (base.as_object_mut(), patch) {
        base.extend(patch);
    }
}

#[test]
fn forced_tool_choice_needs_matching_tools() {
    for (request, param) in [
        (
            json!({ "model": "m", "input": "Hi", "tool_choice": "required" }),
            "tool_choice",
        ),
        (
            json!({ "model": "m", "input": "Hi",
                "tools": [{ "type": "function", "name": "lookup" }],
                "tool_choice": { "type": "function", "name": "other" } }),
            "tool_choice",
        ),
    ] {
        let err = build_with(&settings(), request).unwrap_err();
        assert_eq!(err.code(), "validation_error");
        assert_eq!(err.to_response_error().param.as_deref(), Some(param));
    }
}

#[test]
fn forced_tool_choice_is_unsupported_with_thinking() {
    let request = json!({ "model": "m", "input": "Hi",
        "tools": [{ "type": "function", "name": "lookup" }],
        "tool_choice": "required",
        "reasoning": { "effort": "low" } });
    let err = build_with(&settings(), request.clone()).unwrap_err();
    assert_eq!(err.code(), "unsupported_parameter");

    let mut request = request;
    request["drop_unsupported_params"] = json!(true);
    let built = build_with(&settings(), request).unwrap();
    assert_eq!(built.ignored_parameters, vec!["tool_choice".to_owned()]);
    assert_eq!(
        built.request.tool_choice,
        Some(ToolChoice::Auto {
            disable_parallel_tool_use: None
        })
    );
}

#[test]
fn budget_with_effort_none_is_rejected() {
    let err = build_with(
        &settings(),
        json!({ "model": "m", "input": "Hi",
            "reasoning": { "effort": "none", "budget_tokens": 1024 } }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "validation_error");
}

#[test]
fn function_call_arguments_must_be_an_object() {
    let err = build_with(
        &settings(),
        json!({ "model": "m", "input": [
            { "type": "function_call", "call_id": "toolu_1", "name": "lookup", "arguments": "[1]" }
        ] }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "validation_error");
}

#[test]
fn echo_reports_the_values_sent() {
    let settings = AnthropicProviderSettingsV1 {
        max_tokens: Some(512),
        service_tier: Some(AnthropicServiceTier::StandardOnly),
        tool_choice: Some(AnthropicToolChoice::Any {
            disable_parallel_tool_use: Some(true),
        }),
        ..settings()
    };
    let built = build_with(
        &settings,
        json!({ "model": "m", "input": "Hi", "tools": [{ "type": "function", "name": "lookup" }] }),
    )
    .unwrap();
    assert_eq!(
        built.echo,
        Echo {
            max_output_tokens: Some(512),
            tool_choice: Some(InputChoice::Mode(ToolChoiceMode::Required)),
            parallel_tool_calls: Some(false),
            service_tier: Some("default".to_owned()),
        }
    );
}

#[test]
fn settings_output_schema_is_adapted() {
    let settings = AnthropicProviderSettingsV1 {
        output_config: Some(AnthropicOutputConfig {
            effort: None,
            format: Some(AnthropicJsonOutputFormat::JsonSchema {
                schema: json!({ "type": "object", "properties": {
                    "phone": { "type": "string", "maxLength": 20 }
                } }),
            }),
        }),
        ..settings()
    };
    let wire = wire(&settings, json!({ "model": "m", "input": "Hi" }));
    assert_eq!(
        wire["output_config"]["format"]["schema"],
        json!({ "type": "object", "additionalProperties": false, "properties": {
            "phone": { "type": "string", "description": "{maxLength: 20}" }
        } })
    );
}

#[test]
fn extra_fields_cannot_set_a_container() {
    let err = build_with(
        &settings(),
        json!({ "model": "m", "input": "Hi", "extra_fields": { "container": "container_1" } }),
    )
    .unwrap_err();
    assert_eq!(
        err.to_response_error().param.as_deref(),
        Some("extra_fields.container")
    );
}
