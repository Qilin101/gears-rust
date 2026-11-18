// Created: 2026-11-09 by Constructor Tech
use serde_json::json;

use super::*;

#[test]
fn request_omits_unset_fields_and_extras_override() {
    let mut extra = Map::new();
    extra.insert("inference_geo".to_owned(), json!("eu"));
    extra.insert("speed".to_owned(), json!("fast"));
    let request = MessagesRequest {
        model: "claude-x".to_owned(),
        max_tokens: 64,
        messages: vec![MessageParam {
            role: Role::User,
            content: vec![ContentBlockParam::text("hi")],
        }],
        inference_geo: Some("us".to_owned()),
        extra,
        ..MessagesRequest::default()
    };
    assert_eq!(
        request.to_body().unwrap(),
        json!({
            "model": "claude-x",
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
            "inference_geo": "eu",
            "speed": "fast"
        })
    );
}

#[test]
fn raw_block_serializes_verbatim() {
    let raw = json!({ "type": "server_tool_use", "id": "srv_1", "name": "web_fetch", "input": {} });
    assert_eq!(
        serde_json::to_value(ContentBlockParam::Raw(raw.clone())).unwrap(),
        raw
    );
}

#[test]
fn blocks_parse_by_type() {
    assert_eq!(
        Block::parse(json!({ "type": "text", "text": "hi" })),
        Block::Text {
            text: "hi".to_owned(),
            citations: Vec::new()
        }
    );
    assert_eq!(
        Block::parse(json!({ "type": "tool_use", "id": "t1", "name": "f", "input": { "a": 1 } })),
        Block::ToolUse {
            id: "t1".to_owned(),
            name: "f".to_owned(),
            input: json!({ "a": 1 })
        }
    );
    assert_eq!(
        Block::parse(json!({
            "type": "server_tool_use", "id": "s1", "name": "web_search", "input": { "query": "rust" }
        })),
        Block::WebSearchCall {
            id: "s1".to_owned(),
            query: Some("rust".to_owned())
        }
    );
    assert_eq!(
        Block::parse(json!({
            "type": "web_search_tool_result",
            "tool_use_id": "s1",
            "content": { "type": "web_search_tool_result_error", "error_code": "unavailable" }
        })),
        Block::WebSearchResult {
            tool_use_id: "s1".to_owned(),
            results: Vec::new(),
            failed: true
        }
    );
}

#[test]
fn non_web_search_server_tool_stays_raw() {
    let raw = json!({ "type": "server_tool_use", "id": "s1", "name": "web_fetch", "input": {} });
    assert_eq!(Block::parse(raw.clone()), Block::Other(raw));
}

#[test]
fn usage_counts_read_server_tool_use() {
    let counts = UsageCounts::from_raw(&json!({
        "input_tokens": 4,
        "server_tool_use": { "web_search_requests": 2, "web_fetch_requests": 0 }
    }));
    assert_eq!(counts.input_tokens, Some(4));
    assert_eq!(
        counts.server_tool_use,
        Some(ServerToolUse {
            web_search_requests: 2
        })
    );
}

#[test]
fn stream_events_decode() {
    let event = |data: &str| {
        StreamEvent::from_server_event(ServerEvent {
            data: data.to_owned(),
            ..ServerEvent::default()
        })
        .unwrap()
    };
    assert_eq!(event(r#"{"type":"ping"}"#), StreamEvent::Ignored);
    assert_eq!(event(""), StreamEvent::Ignored);
    assert_eq!(
        event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"a"}}"#
        ),
        StreamEvent::ContentBlockDelta {
            index: 0,
            delta: BlockDelta::TextDelta {
                text: "a".to_owned()
            }
        }
    );
    assert_eq!(
        event(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
        StreamEvent::Error {
            error: ErrorObject {
                kind: "overloaded_error".to_owned(),
                message: "Overloaded".to_owned()
            }
        }
    );
}

#[test]
fn malformed_stream_event_is_a_parse_error() {
    let result = StreamEvent::from_server_event(ServerEvent {
        data: "{not json".to_owned(),
        ..ServerEvent::default()
    });
    assert!(matches!(
        result,
        Err(StreamingError::ServerEventsParse { .. })
    ));
}
