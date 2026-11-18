// Created: 2026-11-18 by Constructor Tech
use serde_json::{Value, json};

use super::*;

fn request(extra: &Value) -> Value {
    let mut body = json!({
        "model": "claude",
        "max_tokens": 16,
        "messages": [{ "role": "user", "content": "Hi" }]
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn rejection(body: &Value) -> (String, Option<String>) {
    let err = check_request(body).unwrap_err();
    assert_eq!(err.code(), "capability_not_supported");
    let LlmGatewayError::CapabilityNotSupported {
        capability, param, ..
    } = err
    else {
        unreachable!()
    };
    (capability, param)
}

#[test]
fn plain_request_passes() {
    check_request(&request(&json!({
        "system": "Be brief",
        "tools": [{ "name": "lookup", "input_schema": { "type": "object" } }],
        "metadata": { "user_id": "u1" }
    })))
    .unwrap();
}

#[test]
fn stateful_fields_are_rejected_with_their_path() {
    let cases = [
        (json!({ "container": "cnt_1" }), "container", "container"),
        (
            json!({ "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "Read" },
                { "type": "document", "source": { "type": "file", "file_id": "file_1" } }
            ] }] }),
            "files",
            "messages[0].content[1].source",
        ),
        (
            json!({ "messages": [{ "role": "user", "content": [
                { "type": "container_upload", "id": "x" }
            ] }] }),
            "files",
            "messages[0].content[0]",
        ),
        (
            json!({ "messages": [{ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": [
                    { "type": "image", "file_id": "file_1" }
                ] }
            ] }] }),
            "files",
            "messages[0].content[0].content[0].file_id",
        ),
        (
            json!({ "mcp_servers": [{ "name": "m", "vector_store_ids": ["vs_1"] }] }),
            "vector_stores",
            "mcp_servers[0].vector_store_ids",
        ),
    ];
    for (extra, capability, param) in cases {
        let (got_capability, got_param) = rejection(&request(&extra));
        assert_eq!(got_capability, capability, "{param}");
        assert_eq!(got_param.as_deref(), Some(param));
    }
}

#[test]
fn stateful_and_paid_tools_are_rejected() {
    let cases = [
        (
            json!({ "type": "code_execution_20250825", "name": "code_execution" }),
            "code_execution",
        ),
        (
            json!({ "type": "web_search_20250305", "name": "web_search" }),
            "web_search",
        ),
    ];
    for (tool, capability) in cases {
        let body = request(&json!({ "tools": [{ "name": "lookup", "input_schema": {} }, tool] }));
        let (got_capability, got_param) = rejection(&body);
        assert_eq!(got_capability, capability);
        assert_eq!(got_param.as_deref(), Some("tools[1]"));
    }
}

#[test]
fn consumer_defined_subtrees_are_not_scanned() {
    let properties = json!({ "type": "object", "properties": {
        "file_id": { "type": "string" },
        "container": { "type": "string" },
        "vector_store_ids": { "type": "array" }
    } });
    check_request(&request(&json!({
        "tools": [{ "name": "open_file", "input_schema": properties }],
        "output_config": { "format": { "type": "json_schema", "schema": properties } },
        "metadata": { "file_id": "mine" },
        "messages": [
            { "role": "user", "content": "Open it" },
            { "role": "assistant", "content": [
                { "type": "tool_use", "id": "t1", "name": "open_file",
                  "input": { "file_id": "mine", "container": "box" } }
            ] }
        ]
    })))
    .unwrap();
}

#[test]
fn skipped_names_do_not_hide_other_fields() {
    let (_, param) = rejection(&request(&json!({
        "output_config": { "format": { "type": "json_schema", "file_id": "file_1" } }
    })));
    assert_eq!(param.as_deref(), Some("output_config.format.file_id"));

    let (_, param) = rejection(&request(&json!({
        "messages": [{ "role": "assistant", "content": [
            { "type": "server_tool_use", "id": "s1", "name": "x", "input": { "file_id": "file_1" } }
        ] }]
    })));
    assert_eq!(
        param.as_deref(),
        Some("messages[0].content[0].input.file_id")
    );
}

#[test]
fn a_single_value_is_checked_under_its_path() {
    let block = json!({ "type": "code_execution_tool_result", "content": {
        "type": "code_execution_result", "content": [{ "type": "output", "file_id": "file_1" }]
    } });
    let err = check_value(&block, "input[2]").unwrap_err();
    assert_eq!(
        err.to_response_error().param.as_deref(),
        Some("input[2].content.content[0].file_id")
    );
    check_value(&json!({ "type": "text", "text": "file_id" }), "input[0]").unwrap();
}
