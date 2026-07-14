// Created: 2026-07-09 by Constructor Tech
//! Streaming event models — from `schemas/streaming/`.
//!
//! Server-sent events emitted while a response is streamed. Every event shares
//! a base (`type` + `sequence_number`); concrete events are `type`-discriminated
//! and map to one tagged enum, [`StreamingEvent`]. The stream terminates with a
//! `data: [DONE]` sentinel, which is not a JSON event and is not modeled here.
//!
//! Payload structs are shared across events with identical shapes (snapshot,
//! content-part, data, summary-part, and content-delta groups).

use crate::models::content::{Annotation, LogProb, OutputContentPart};
use crate::models::core::{ResponseError, ResponseResource};
use crate::models::extension::{Extension, from_tagged, serialize_tagged, tag_of};
use crate::models::items::{DataOutput, OutputItem, ReasoningSummaryPart};

// ---------------------------------------------------------------------------
// StreamingEvent
// ---------------------------------------------------------------------------

/// A streaming response event, discriminated by `type` (the SSE event name).
///
/// Any `type` the core does not own — a provider extension
/// (`{provider_slug}:{event_type}`) or third-party plugin event — is preserved
/// verbatim in [`StreamingEvent::Other`] and forwarded without interpretation.
// The core-owned variants carry a full response snapshot; the size gap versus
// `Other` is inherent to the protocol and not worth an allocation on the hot
// path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, schemars::JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum StreamingEvent {
    /// The response was created.
    #[serde(rename = "response.created")]
    Created(ResponseSnapshotEvent),
    /// The response is in progress.
    #[serde(rename = "response.in_progress")]
    InProgress(ResponseSnapshotEvent),
    /// The response was queued.
    #[serde(rename = "response.queued")]
    Queued(ResponseSnapshotEvent),
    /// The response completed.
    #[serde(rename = "response.completed")]
    Completed(ResponseSnapshotEvent),
    /// The response ended incomplete.
    #[serde(rename = "response.incomplete")]
    Incomplete(ResponseSnapshotEvent),
    /// The response failed.
    #[serde(rename = "response.failed")]
    Failed(ResponseSnapshotEvent),

    /// An output item was added.
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded(OutputItemAddedEvent),
    /// An output item was completed.
    #[serde(rename = "response.output_item.done")]
    OutputItemDone(OutputItemDoneEvent),

    /// A content part was added.
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded(ContentPartEvent),
    /// A content part was completed.
    #[serde(rename = "response.content_part.done")]
    ContentPartDone(ContentPartEvent),

    /// Output text was incrementally added.
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta(OutputTextDeltaEvent),
    /// Output text was completed.
    #[serde(rename = "response.output_text.done")]
    OutputTextDone(OutputTextDoneEvent),
    /// An output-text annotation was added.
    #[serde(rename = "response.output_text.annotation.added")]
    OutputTextAnnotationAdded(OutputTextAnnotationAddedEvent),

    /// Refusal text was incrementally added.
    #[serde(rename = "response.refusal.delta")]
    RefusalDelta(ContentDeltaEvent),
    /// Refusal text was completed.
    #[serde(rename = "response.refusal.done")]
    RefusalDone(RefusalDoneEvent),

    /// Function-call arguments were incrementally added.
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta(FunctionCallArgumentsDeltaEvent),
    /// Function-call arguments were completed.
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone(FunctionCallArgumentsDoneEvent),

    /// Reasoning text was incrementally added.
    #[serde(rename = "response.reasoning.delta")]
    ReasoningDelta(ContentDeltaEvent),
    /// Reasoning text was completed.
    #[serde(rename = "response.reasoning.done")]
    ReasoningDone(ReasoningDoneEvent),

    /// A reasoning-summary part was added.
    #[serde(rename = "response.reasoning_summary_part.added")]
    ReasoningSummaryPartAdded(SummaryPartEvent),
    /// A reasoning-summary part was completed.
    #[serde(rename = "response.reasoning_summary_part.done")]
    ReasoningSummaryPartDone(SummaryPartEvent),
    /// Reasoning-summary text was incrementally added.
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta(ReasoningSummaryTextDeltaEvent),
    /// Reasoning-summary text was completed.
    #[serde(rename = "response.reasoning_summary_text.done")]
    ReasoningSummaryTextDone(ReasoningSummaryTextDoneEvent),

    /// An error was emitted.
    #[serde(rename = "error")]
    Error(ErrorEvent),

    /// A binary data output started processing (Gears extension).
    #[serde(rename = "cf_gears:response.data.in_progress")]
    DataInProgress(DataEvent),
    /// A binary data output completed (Gears extension).
    #[serde(rename = "cf_gears:response.data.done")]
    DataDone(DataEvent),

    /// An event `type` the core does not own, preserved verbatim.
    #[serde(skip)]
    Other(Extension),
}

impl serde::Serialize for StreamingEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Created(v) => serialize_tagged(serializer, "response.created", v),
            Self::InProgress(v) => serialize_tagged(serializer, "response.in_progress", v),
            Self::Queued(v) => serialize_tagged(serializer, "response.queued", v),
            Self::Completed(v) => serialize_tagged(serializer, "response.completed", v),
            Self::Incomplete(v) => serialize_tagged(serializer, "response.incomplete", v),
            Self::Failed(v) => serialize_tagged(serializer, "response.failed", v),
            Self::OutputItemAdded(v) => {
                serialize_tagged(serializer, "response.output_item.added", v)
            }
            Self::OutputItemDone(v) => {
                serialize_tagged(serializer, "response.output_item.done", v)
            }
            Self::ContentPartAdded(v) => {
                serialize_tagged(serializer, "response.content_part.added", v)
            }
            Self::ContentPartDone(v) => {
                serialize_tagged(serializer, "response.content_part.done", v)
            }
            Self::OutputTextDelta(v) => {
                serialize_tagged(serializer, "response.output_text.delta", v)
            }
            Self::OutputTextDone(v) => {
                serialize_tagged(serializer, "response.output_text.done", v)
            }
            Self::OutputTextAnnotationAdded(v) => {
                serialize_tagged(serializer, "response.output_text.annotation.added", v)
            }
            Self::RefusalDelta(v) => serialize_tagged(serializer, "response.refusal.delta", v),
            Self::RefusalDone(v) => serialize_tagged(serializer, "response.refusal.done", v),
            Self::FunctionCallArgumentsDelta(v) => {
                serialize_tagged(serializer, "response.function_call_arguments.delta", v)
            }
            Self::FunctionCallArgumentsDone(v) => {
                serialize_tagged(serializer, "response.function_call_arguments.done", v)
            }
            Self::ReasoningDelta(v) => serialize_tagged(serializer, "response.reasoning.delta", v),
            Self::ReasoningDone(v) => serialize_tagged(serializer, "response.reasoning.done", v),
            Self::ReasoningSummaryPartAdded(v) => {
                serialize_tagged(serializer, "response.reasoning_summary_part.added", v)
            }
            Self::ReasoningSummaryPartDone(v) => {
                serialize_tagged(serializer, "response.reasoning_summary_part.done", v)
            }
            Self::ReasoningSummaryTextDelta(v) => {
                serialize_tagged(serializer, "response.reasoning_summary_text.delta", v)
            }
            Self::ReasoningSummaryTextDone(v) => {
                serialize_tagged(serializer, "response.reasoning_summary_text.done", v)
            }
            Self::Error(v) => serialize_tagged(serializer, "error", v),
            Self::DataInProgress(v) => {
                serialize_tagged(serializer, "cf_gears:response.data.in_progress", v)
            }
            Self::DataDone(v) => serialize_tagged(serializer, "cf_gears:response.data.done", v),
            Self::Other(ext) => serde::Serialize::serialize(&ext.0, serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for StreamingEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(tag) = tag_of(&value) {
            match tag {
                "response.created" => return from_tagged(&value).map(Self::Created),
                "response.in_progress" => return from_tagged(&value).map(Self::InProgress),
                "response.queued" => return from_tagged(&value).map(Self::Queued),
                "response.completed" => return from_tagged(&value).map(Self::Completed),
                "response.incomplete" => return from_tagged(&value).map(Self::Incomplete),
                "response.failed" => return from_tagged(&value).map(Self::Failed),
                "response.output_item.added" => {
                    return from_tagged(&value).map(Self::OutputItemAdded);
                }
                "response.output_item.done" => {
                    return from_tagged(&value).map(Self::OutputItemDone);
                }
                "response.content_part.added" => {
                    return from_tagged(&value).map(Self::ContentPartAdded);
                }
                "response.content_part.done" => {
                    return from_tagged(&value).map(Self::ContentPartDone);
                }
                "response.output_text.delta" => {
                    return from_tagged(&value).map(Self::OutputTextDelta);
                }
                "response.output_text.done" => {
                    return from_tagged(&value).map(Self::OutputTextDone);
                }
                "response.output_text.annotation.added" => {
                    return from_tagged(&value).map(Self::OutputTextAnnotationAdded);
                }
                "response.refusal.delta" => return from_tagged(&value).map(Self::RefusalDelta),
                "response.refusal.done" => return from_tagged(&value).map(Self::RefusalDone),
                "response.function_call_arguments.delta" => {
                    return from_tagged(&value).map(Self::FunctionCallArgumentsDelta);
                }
                "response.function_call_arguments.done" => {
                    return from_tagged(&value).map(Self::FunctionCallArgumentsDone);
                }
                "response.reasoning.delta" => {
                    return from_tagged(&value).map(Self::ReasoningDelta);
                }
                "response.reasoning.done" => return from_tagged(&value).map(Self::ReasoningDone),
                "response.reasoning_summary_part.added" => {
                    return from_tagged(&value).map(Self::ReasoningSummaryPartAdded);
                }
                "response.reasoning_summary_part.done" => {
                    return from_tagged(&value).map(Self::ReasoningSummaryPartDone);
                }
                "response.reasoning_summary_text.delta" => {
                    return from_tagged(&value).map(Self::ReasoningSummaryTextDelta);
                }
                "response.reasoning_summary_text.done" => {
                    return from_tagged(&value).map(Self::ReasoningSummaryTextDone);
                }
                "error" => return from_tagged(&value).map(Self::Error),
                "cf_gears:response.data.in_progress" => {
                    return from_tagged(&value).map(Self::DataInProgress);
                }
                "cf_gears:response.data.done" => return from_tagged(&value).map(Self::DataDone),
                _ => {}
            }
        }
        Ok(Self::Other(Extension(value)))
    }
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// Lifecycle event carrying a full response snapshot (shared by the six
/// `response.*` lifecycle events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ResponseSnapshotEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// The response snapshot emitted with the event.
    pub response: ResponseResource,
}

/// An output item was added.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputItemAddedEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Index of the output item.
    pub output_index: u32,
    /// The output item that was added.
    pub item: OutputItem,
}

/// An output item was completed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputItemDoneEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Index of the output item.
    pub output_index: u32,
    /// The output item that was completed, if any.
    pub item: Option<OutputItem>,
}

/// A content part was added or completed (shared by both content-part events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ContentPartEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the content part.
    pub content_index: u32,
    /// The content part.
    pub part: OutputContentPart,
}

/// Output text delta.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputTextDeltaEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the content part.
    pub content_index: u32,
    /// The text delta appended.
    pub delta: String,
    /// Token log-probabilities emitted with the delta, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Vec<LogProb>>,
}

/// Output text completion.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputTextDoneEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the content part.
    pub content_index: u32,
    /// The final text emitted.
    pub text: String,
    /// Token log-probabilities emitted with the final text, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Vec<LogProb>>,
}

/// An output-text annotation was added.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputTextAnnotationAddedEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the output-text content.
    pub content_index: u32,
    /// Index of the annotation.
    pub annotation_index: u32,
    /// The annotation that was added, if any.
    pub annotation: Option<Annotation>,
}

/// Incremental text delta scoped to a content part (shared by the refusal and
/// reasoning delta events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ContentDeltaEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the content part.
    pub content_index: u32,
    /// The text delta appended.
    pub delta: String,
}

/// Refusal text completion.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RefusalDoneEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the refusal content.
    pub content_index: u32,
    /// The final refusal text emitted.
    pub refusal: String,
}

/// Function-call arguments delta.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FunctionCallArgumentsDeltaEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the tool-call item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// The arguments delta appended.
    pub delta: String,
}

/// Function-call arguments completion.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FunctionCallArgumentsDoneEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the tool-call item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// The final arguments string emitted.
    pub arguments: String,
}

/// Reasoning text completion.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReasoningDoneEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the reasoning content.
    pub content_index: u32,
    /// The final reasoning text emitted.
    pub text: String,
}

/// A reasoning-summary part was added or completed (shared by both summary-part
/// events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SummaryPartEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the summary part.
    pub summary_index: u32,
    /// The summary content part.
    pub part: ReasoningSummaryPart,
}

/// Reasoning-summary text delta.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReasoningSummaryTextDeltaEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the summary content.
    pub summary_index: u32,
    /// The summary text delta appended.
    pub delta: String,
}

/// Reasoning-summary text completion.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReasoningSummaryTextDoneEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the summary content.
    pub summary_index: u32,
    /// The final summary text emitted.
    pub text: String,
}

/// An error event.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ErrorEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// The error payload emitted.
    pub error: ResponseError,
}

/// Binary data output progress event (shared by the two Gears data events).
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DataEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Index of the output item in the response output array.
    pub output_index: u32,
    /// The data output item.
    pub item: DataOutput,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::content::OutputText;
    use crate::models::core::Role;
    use crate::models::items::{ItemStatus, MessageOutput};

    #[test]
    fn output_item_added_roundtrips() {
        let event = StreamingEvent::OutputItemAdded(OutputItemAddedEvent {
            sequence_number: 3,
            output_index: 0,
            item: OutputItem::Message(MessageOutput {
                id: "m1".into(),
                status: ItemStatus::InProgress,
                role: Role::Assistant,
                content: vec![OutputContentPart::OutputText(OutputText {
                    text: "hi".into(),
                    annotations: vec![],
                    logprobs: None,
                })],
            }),
        });
        let value = serde_json::to_value(&event).unwrap();
        let back: StreamingEvent = serde_json::from_value(value).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn refusal_and_reasoning_share_delta_shape() {
        let refusal = StreamingEvent::RefusalDelta(ContentDeltaEvent {
            sequence_number: 1,
            item_id: "i1".into(),
            output_index: 0,
            content_index: 0,
            delta: "no".into(),
        });
        let reasoning: StreamingEvent = serde_json::from_value(serde_json::json!({
            "type": "response.reasoning.delta",
            "sequence_number": 2,
            "item_id": "i1",
            "output_index": 0,
            "content_index": 0,
            "delta": "thinking"
        }))
        .unwrap();
        assert!(matches!(refusal, StreamingEvent::RefusalDelta(_)));
        assert!(matches!(reasoning, StreamingEvent::ReasoningDelta(_)));
    }

    #[test]
    fn data_done_roundtrips_with_gears_discriminator() {
        let event = StreamingEvent::DataDone(DataEvent {
            sequence_number: 7,
            output_index: 0,
            item: DataOutput {
                id: "d1".into(),
                status: ItemStatus::Completed,
                mime_type: "image/png".into(),
                base64: Some("AAAA".into()),
                url: None,
            },
        });
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "cf_gears:response.data.done");
        let back: StreamingEvent = serde_json::from_value(value).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn output_text_delta_accepts_logprobs_without_bytes() {
        let event: StreamingEvent = serde_json::from_value(serde_json::json!({
            "type": "response.output_text.delta",
            "sequence_number": 4,
            "item_id": "i1",
            "output_index": 0,
            "content_index": 0,
            "delta": "a",
            "logprobs": [
                { "token": "a", "logprob": -0.1, "top_logprobs": [{ "token": "b", "logprob": -1.0 }] }
            ]
        }))
        .unwrap();
        let StreamingEvent::OutputTextDelta(delta) = &event else {
            panic!("expected output_text.delta");
        };
        assert_eq!(delta.logprobs.as_ref().unwrap()[0].bytes, Vec::<u8>::new());
    }

    #[test]
    fn derived_schema_keeps_type_discriminator_and_skips_other() {
        // The schema is still derived even though serde is hand-written: it must
        // document the known `type` tags and omit the `Other` catch-all.
        let schema = serde_json::to_value(schemars::schema_for!(StreamingEvent)).unwrap();
        let text = schema.to_string();
        assert!(text.contains("response.created"), "known tag missing: {text}");
        assert!(
            text.contains("cf_gears:response.data.done"),
            "gears tag missing"
        );
        assert!(!text.contains("\"Other\""), "Other must be skipped: {text}");
    }

    #[test]
    fn unknown_event_type_preserved_as_other() {
        let wire = serde_json::json!({
            "type": "openai:web_search_call.searching",
            "sequence_number": 9,
            "output_index": 0,
            "extra": { "query": "rust" }
        });
        let event: StreamingEvent = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(event, StreamingEvent::Other(_)));
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }
}
