// Created: 2026-11-09 by Constructor Tech
use serde_json::json;

use super::*;
use crate::wire::Message;

fn envelope() -> Envelope {
    Envelope {
        request_id: "req".to_owned(),
        created_at: 1_700_000_000,
        ignored_parameters: Vec::new(),
        provider_request_id: Some("req_anthropic".to_owned()),
    }
}

fn request() -> CreateResponseBody {
    serde_json::from_value(json!({ "model": "claude" })).unwrap()
}

fn event(value: Value) -> StreamEvent {
    serde_json::from_value(value).unwrap()
}

fn message_start() -> StreamEvent {
    event(json!({ "type": "message_start", "message": {
        "id": "msg_01", "type": "message", "role": "assistant", "model": "claude",
        "content": [], "stop_reason": null,
        "usage": { "input_tokens": 10, "output_tokens": 1, "cache_read_input_tokens": 2 }
    }}))
}

async fn run(events: Vec<StreamEvent>) -> Vec<StreamingEvent> {
    run_raw(events.into_iter().map(Ok).collect()).await
}

async fn run_raw(events: Vec<Result<StreamEvent, StreamingError>>) -> Vec<StreamingEvent> {
    translate(futures::stream::iter(events), &request(), envelope())
        .collect()
        .await
}

fn kind(event: &StreamingEvent) -> String {
    serde_json::to_value(event).unwrap()["type"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn sequence(event: &StreamingEvent) -> u64 {
    serde_json::to_value(event).unwrap()["sequence_number"]
        .as_u64()
        .unwrap()
}

fn terminal(events: &[StreamingEvent]) -> &ResponseResource {
    match events.last() {
        Some(
            StreamingEvent::Completed(e)
            | StreamingEvent::Incomplete(e)
            | StreamingEvent::Failed(e),
        ) => &e.response,
        other => panic!("expected a terminal event, got {other:?}"),
    }
}

fn full_conversation() -> Vec<StreamEvent> {
    vec![
        message_start(),
        event(json!({ "type": "ping" })),
        event(json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "thinking", "thinking": "", "signature": "" } })),
        event(json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "Hmm" } })),
        event(json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "signature_delta", "signature": "sig" } })),
        event(json!({ "type": "content_block_stop", "index": 0 })),
        event(json!({ "type": "content_block_start", "index": 1,
            "content_block": { "type": "text", "text": "" } })),
        event(json!({ "type": "content_block_delta", "index": 1,
            "delta": { "type": "text_delta", "text": "Hel" } })),
        event(json!({ "type": "content_block_delta", "index": 1,
            "delta": { "type": "text_delta", "text": "lo" } })),
        event(json!({ "type": "content_block_stop", "index": 1 })),
        event(json!({ "type": "content_block_start", "index": 2,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {} } })),
        event(json!({ "type": "content_block_delta", "index": 2,
            "delta": { "type": "input_json_delta", "partial_json": "{\"q\":" } })),
        event(json!({ "type": "content_block_delta", "index": 2,
            "delta": { "type": "input_json_delta", "partial_json": "\"x\"}" } })),
        event(json!({ "type": "content_block_stop", "index": 2 })),
        event(
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" },
            "usage": { "output_tokens": 30 } }),
        ),
        event(json!({ "type": "message_stop" })),
    ]
}

#[tokio::test]
async fn translates_full_stream_in_order() {
    let events = run(full_conversation()).await;
    let kinds: Vec<String> = events.iter().map(kind).collect();
    assert_eq!(
        kinds,
        [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.reasoning.delta",
            "response.reasoning.done",
            "response.output_item.done",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    let sequences: Vec<u64> = events.iter().map(sequence).collect();
    assert_eq!(sequences, (0..19).collect::<Vec<u64>>());
}

#[tokio::test]
async fn final_snapshot_matches_sync_mapping() {
    let events = run(full_conversation()).await;
    let streamed = terminal(&events);

    let message: Message = serde_json::from_value(json!({
        "id": "msg_01",
        "content": [
            { "type": "thinking", "thinking": "Hmm", "signature": "sig" },
            { "type": "text", "text": "Hello" },
            { "type": "tool_use", "id": "toolu_1", "name": "lookup", "input": { "q": "x" } }
        ],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 10, "output_tokens": 30, "cache_read_input_tokens": 2 }
    }))
    .unwrap();
    let sync = response::to_resource(&request(), &envelope(), message);

    assert_eq!(streamed.status, ResponseStatus::Completed);
    assert_eq!(streamed.output, sync.output);
    assert_eq!(streamed.usage, sync.usage);
    assert_eq!(streamed.provider_details, sync.provider_details);
}

#[tokio::test]
async fn max_tokens_ends_incomplete() {
    let events = run(vec![
        message_start(),
        event(
            json!({ "type": "message_delta", "delta": { "stop_reason": "max_tokens" },
            "usage": { "output_tokens": 5 } }),
        ),
        event(json!({ "type": "message_stop" })),
    ])
    .await;
    assert!(matches!(events.last(), Some(StreamingEvent::Incomplete(_))));
}

#[tokio::test]
async fn error_event_fails_the_stream_once() {
    let events = run(vec![
        message_start(),
        event(json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } })),
        event(json!({ "type": "message_stop" })),
    ])
    .await;
    let failed = terminal(&events);
    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    let error = failed.error.as_ref().unwrap();
    assert_eq!(error.code, "provider_unavailable");
    assert_eq!(error.provider_code.as_deref(), Some("overloaded_error"));
    let usage = failed.usage.as_ref().unwrap();
    assert!(usage.estimated);
    assert_eq!(usage.input_tokens, 12);
    let terminals = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                StreamingEvent::Completed(_)
                    | StreamingEvent::Incomplete(_)
                    | StreamingEvent::Failed(_)
            )
        })
        .count();
    assert_eq!(terminals, 1);
}

#[tokio::test]
async fn truncated_stream_fails() {
    let mut events = full_conversation();
    events.truncate(9);
    let events = run(events).await;
    let failed = terminal(&events);
    assert_eq!(failed.status, ResponseStatus::Failed);
    assert_eq!(failed.error.as_ref().unwrap().code, "provider_unavailable");
}

#[tokio::test]
async fn transport_error_fails() {
    let events = run_raw(vec![
        Ok(message_start()),
        Err(StreamingError::ServerEventsParse {
            detail: "bad utf-8".to_owned(),
        }),
    ])
    .await;
    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
}

#[tokio::test]
async fn whole_blocks_are_emitted_at_stop() {
    let events = run(vec![
        message_start(),
        event(json!({ "type": "content_block_start", "index": 0, "content_block":
            { "type": "server_tool_use", "id": "s1", "name": "web_search", "input": { "query": "rust" } } })),
        event(json!({ "type": "content_block_stop", "index": 0 })),
        event(json!({ "type": "content_block_start", "index": 1, "content_block":
            { "type": "web_search_tool_result", "tool_use_id": "s1", "content": [
                { "type": "web_search_result", "url": "https://rust-lang.org", "title": "Rust", "encrypted_content": "e" }
            ] } })),
        event(json!({ "type": "content_block_stop", "index": 1 })),
        event(json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 3 } })),
        event(json!({ "type": "message_stop" })),
    ])
    .await;
    let kinds: Vec<String> = events.iter().map(kind).collect();
    assert_eq!(
        kinds[2..],
        [
            "response.output_item.added",
            "response.output_item.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    let output = serde_json::to_value(&terminal(&events).output).unwrap();
    assert_eq!(
        output,
        json!([{ "type": "cf_gears:web_search_call", "id": "ws_req_0", "status": "completed",
            "queries": ["rust"], "results": [{ "url": "https://rust-lang.org", "title": "Rust" }] }])
    );
}

#[tokio::test]
async fn citations_are_emitted_as_annotations() {
    let events = run(vec![
        message_start(),
        event(json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } })),
        event(json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "citations_delta", "citation":
            { "type": "web_search_result_location", "url": "https://x.test", "title": "X",
              "cited_text": "c", "encrypted_index": "i" } } })),
        event(json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "abc" } })),
        event(json!({ "type": "content_block_stop", "index": 0 })),
        event(json!({ "type": "message_stop" })),
    ])
    .await;
    let annotation = events
        .iter()
        .find_map(|e| match e {
            StreamingEvent::OutputTextAnnotationAdded(a) => a.annotation.clone(),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(annotation).unwrap(),
        json!({ "type": "url_citation", "url": "https://x.test", "title": "X", "start_index": 0, "end_index": 3 })
    );
}
