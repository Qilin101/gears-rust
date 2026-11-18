// Created: 2026-11-09 by Constructor Tech
//! Anthropic Messages SSE events -> Open Responses streaming events.
//!
//! Anthropic streams one content block at a time. Text, tool arguments, and
//! thinking are forwarded as deltas; other blocks are emitted whole when they
//! stop. A web search item is done once its result block arrives. Every
//! stream ends with exactly one terminal event; a failed stream first closes
//! the open items as `incomplete` and keeps them in its final snapshot.

use std::collections::VecDeque;

use futures::{Stream, StreamExt};
use llm_gateway_sdk::{
    ContentDeltaEvent, ContentPartEvent, CreateResponseBody, FunctionCallArgumentsEvent,
    FunctionCallItem, ItemStatus, LlmGatewayError, MessageOutput, OutputContentPart, OutputItem,
    OutputItemEvent, OutputText, OutputTextAnnotationAddedEvent, OutputTextEvent,
    ProviderErrorCategory, ProviderFailure, ReasoningOutput, ResponseResource,
    ResponseSnapshotEvent, ResponseStatus, Role, StreamingEvent,
};
use oagw_sdk::error::StreamingError;
use serde_json::Value;

use crate::error;
use crate::response::{self, Envelope, OutputBuilder, Placement};
use crate::wire::{Block, BlockDelta, StreamEvent};

/// Translate an Anthropic event stream into Open Responses events.
pub fn translate<S>(
    events: S,
    body: &CreateResponseBody,
    envelope: Envelope,
) -> impl Stream<Item = StreamingEvent> + Send + 'static
where
    S: Stream<Item = Result<StreamEvent, StreamingError>> + Send + 'static,
{
    let translator = Translator::new(body, envelope);
    futures::stream::unfold(
        (Box::pin(events), translator),
        |(mut events, mut translator)| async move {
            loop {
                if let Some(event) = translator.pending.pop_front() {
                    return Some((event, (events, translator)));
                }
                if translator.finished {
                    return None;
                }
                match events.next().await {
                    Some(Ok(event)) => translator.on_event(event),
                    Some(Err(err)) => translator.interrupt(&err.to_string()),
                    None => translator.interrupt("stream ended before message_stop"),
                }
            }
        },
    )
}

/// The content block being streamed.
#[derive(Debug)]
struct OpenBlock {
    index: u32,
    placement: Placement,
    content: OpenContent,
}

#[derive(Debug)]
enum OpenContent {
    Text {
        text: String,
        citations: Vec<Value>,
    },
    ToolUse {
        id: String,
        name: String,
        arguments: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    /// Emitted whole at `content_block_stop`; `input_json` collects a
    /// streamed `input`.
    Whole {
        raw: Value,
        input_json: String,
    },
}

struct Translator {
    snapshot: ResponseResource,
    envelope: Envelope,
    sequence_number: u64,
    pending: VecDeque<StreamingEvent>,
    finished: bool,
    builder: OutputBuilder,
    open: Option<OpenBlock>,
    /// Output index of a message item not yet reported done.
    open_message: Option<u32>,
    /// Output indexes of web search items not yet reported done.
    open_searches: Vec<u32>,
    usage: Value,
    stop_reason: Option<String>,
    stop_details: Option<Value>,
}

impl Translator {
    fn new(body: &CreateResponseBody, envelope: Envelope) -> Self {
        let snapshot = response::base_resource(body, &envelope, ResponseStatus::InProgress);
        let builder = OutputBuilder::new(&envelope.request_id);
        Self {
            snapshot,
            envelope,
            sequence_number: 0,
            pending: VecDeque::new(),
            finished: false,
            builder,
            open: None,
            open_message: None,
            open_searches: Vec::new(),
            usage: Value::Null,
            stop_reason: None,
            stop_details: None,
        }
    }

    fn emit(&mut self, event: impl FnOnce(u64) -> StreamingEvent) {
        self.pending.push_back(event(self.sequence_number));
        self.sequence_number += 1;
    }

    fn on_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::MessageStart { message } => {
                self.usage = message.usage;
                let snapshot = self.snapshot.clone();
                self.emit(|n| StreamingEvent::Created(snapshot_event(n, snapshot.clone())));
                self.emit(|n| StreamingEvent::InProgress(snapshot_event(n, snapshot)));
            }
            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => self.on_block_start(index, content_block),
            StreamEvent::ContentBlockDelta { index, delta } => self.on_block_delta(index, delta),
            StreamEvent::ContentBlockStop { index } => self.on_block_stop(index),
            StreamEvent::MessageDelta { delta, usage } => {
                if delta.stop_reason.is_some() {
                    self.stop_reason = delta.stop_reason;
                }
                if delta.stop_details.is_some() {
                    self.stop_details = delta.stop_details;
                }
                merge_usage(&mut self.usage, usage);
            }
            StreamEvent::MessageStop => self.complete(),
            StreamEvent::Error { error: object } => {
                let err =
                    error::from_stream_error(&object, self.envelope.provider_request_id.as_deref());
                self.fail(&err);
            }
            StreamEvent::Ignored => {}
        }
    }

    /// The provider stream broke off or ended before `message_stop`.
    fn interrupt(&mut self, detail: &str) {
        let mut details = ProviderFailure::default();
        if let Some(id) = &self.envelope.provider_request_id {
            details = details.with_provider_request_id(id.clone());
        }
        let err = LlmGatewayError::provider(
            ProviderErrorCategory::ProviderUnavailable,
            format!("provider stream interrupted: {detail}"),
            details,
        );
        self.fail(&err);
    }

    fn on_block_start(&mut self, index: u32, raw: Value) {
        let block = Block::parse(raw.clone());
        if !matches!(block, Block::Text { .. }) {
            self.close_message();
        }
        let placement = self.builder.peek(block.kind());
        let content = match block {
            Block::Text { text, citations } => {
                self.start_text(&placement, &text);
                OpenContent::Text { text, citations }
            }
            Block::ToolUse { id, name, .. } => {
                let item = OutputItem::FunctionCall(FunctionCallItem {
                    id: Some(placement.item_id.clone()),
                    status: Some(ItemStatus::InProgress),
                    call_id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
                self.item_added(&placement, item);
                OpenContent::ToolUse {
                    id,
                    name,
                    arguments: String::new(),
                }
            }
            Block::Thinking {
                thinking,
                signature,
            } => {
                let item = OutputItem::Reasoning(ReasoningOutput {
                    id: placement.item_id.clone(),
                    summary: Some(Vec::new()),
                    content: Some(Vec::new()),
                    encrypted_content: None,
                });
                self.item_added(&placement, item);
                OpenContent::Thinking {
                    thinking,
                    signature,
                }
            }
            _ => OpenContent::Whole {
                raw,
                input_json: String::new(),
            },
        };
        self.open = Some(OpenBlock {
            index,
            placement,
            content,
        });
    }

    fn start_text(&mut self, placement: &Placement, text: &str) {
        if placement.new_item {
            let item = OutputItem::Message(MessageOutput {
                id: placement.item_id.clone(),
                status: ItemStatus::InProgress,
                role: Role::Assistant,
                content: Vec::new(),
            });
            self.item_added(placement, item);
            self.open_message = Some(placement.output_index);
        }
        let event = ContentPartEvent {
            sequence_number: 0,
            item_id: placement.item_id.clone(),
            output_index: placement.output_index,
            content_index: placement.content_index,
            part: OutputContentPart::OutputText(OutputText {
                text: text.to_owned(),
                annotations: Vec::new(),
                logprobs: None,
            }),
        };
        self.emit(|n| {
            StreamingEvent::ContentPartAdded(ContentPartEvent {
                sequence_number: n,
                ..event
            })
        });
    }

    fn item_added(&mut self, placement: &Placement, item: OutputItem) {
        let output_index = placement.output_index;
        self.emit(|n| {
            StreamingEvent::OutputItemAdded(OutputItemEvent {
                sequence_number: n,
                output_index,
                item: Some(item),
            })
        });
    }

    fn item_done(&mut self, output_index: u32) {
        let item = self.builder.items().get(output_index as usize).cloned();
        self.emit(|n| {
            StreamingEvent::OutputItemDone(OutputItemEvent {
                sequence_number: n,
                output_index,
                item,
            })
        });
    }

    fn close_message(&mut self) {
        if let Some(output_index) = self.open_message.take() {
            self.item_done(output_index);
        }
    }

    fn on_block_delta(&mut self, index: u32, delta: BlockDelta) {
        let Some(open) = self.open.as_mut().filter(|o| o.index == index) else {
            return;
        };
        let empty = match &delta {
            BlockDelta::TextDelta { text } => text.is_empty(),
            BlockDelta::InputJsonDelta { partial_json } => partial_json.is_empty(),
            BlockDelta::ThinkingDelta { thinking } => thinking.is_empty(),
            _ => false,
        };
        if empty {
            return;
        }
        let item_id = open.placement.item_id.clone();
        let output_index = open.placement.output_index;
        let content_index = open.placement.content_index;
        let event: Option<Box<dyn FnOnce(u64) -> StreamingEvent + Send>> =
            match (&mut open.content, delta) {
                (OpenContent::Text { text, .. }, BlockDelta::TextDelta { text: delta }) => {
                    text.push_str(&delta);
                    Some(Box::new(move |n| {
                        StreamingEvent::OutputTextDelta(OutputTextEvent {
                            sequence_number: n,
                            item_id,
                            output_index,
                            content_index,
                            text: delta,
                            logprobs: None,
                        })
                    }))
                }
                (OpenContent::Text { citations, .. }, BlockDelta::CitationsDelta { citation }) => {
                    citations.push(citation);
                    None
                }
                (
                    OpenContent::ToolUse { arguments, .. },
                    BlockDelta::InputJsonDelta { partial_json },
                ) => {
                    arguments.push_str(&partial_json);
                    Some(Box::new(move |n| {
                        StreamingEvent::FunctionCallArgumentsDelta(FunctionCallArgumentsEvent {
                            sequence_number: n,
                            item_id,
                            output_index,
                            arguments: partial_json,
                        })
                    }))
                }
                (
                    OpenContent::Thinking { thinking, .. },
                    BlockDelta::ThinkingDelta { thinking: delta },
                ) => {
                    thinking.push_str(&delta);
                    Some(Box::new(move |n| {
                        StreamingEvent::ReasoningDelta(ContentDeltaEvent {
                            sequence_number: n,
                            item_id,
                            output_index,
                            content_index: 0,
                            delta,
                        })
                    }))
                }
                (
                    OpenContent::Thinking { signature, .. },
                    BlockDelta::SignatureDelta { signature: delta },
                ) => {
                    signature.push_str(&delta);
                    None
                }
                (
                    OpenContent::Whole { input_json, .. },
                    BlockDelta::InputJsonDelta { partial_json },
                ) => {
                    input_json.push_str(&partial_json);
                    None
                }
                _ => None,
            };
        if let Some(event) = event {
            self.emit(event);
        }
    }

    fn on_block_stop(&mut self, index: u32) {
        let Some(open) = self.open.take_if(|o| o.index == index) else {
            return;
        };
        self.stop_block(open, false);
    }

    /// Finish a block; `cut` marks a block the stream broke off in.
    fn stop_block(&mut self, open: OpenBlock, cut: bool) {
        let placement = open.placement;
        match open.content {
            OpenContent::Text { text, citations } => {
                self.stop_text(&placement, text, citations);
                if cut {
                    self.builder.set_incomplete(placement.output_index);
                }
            }
            OpenContent::ToolUse {
                id,
                name,
                mut arguments,
            } => {
                let item_id = placement.item_id.clone();
                let output_index = placement.output_index;
                // A call without input streams no JSON; send `{}` as its only
                // delta so the deltas always add up to the arguments.
                if arguments.is_empty() && !cut {
                    "{}".clone_into(&mut arguments);
                    let delta = arguments.clone();
                    let item_id = item_id.clone();
                    self.emit(|n| {
                        StreamingEvent::FunctionCallArgumentsDelta(FunctionCallArgumentsEvent {
                            sequence_number: n,
                            item_id,
                            output_index,
                            arguments: delta,
                        })
                    });
                }
                self.builder.push_function_call(id, name, arguments.clone());
                if cut {
                    self.builder.set_incomplete(output_index);
                }
                self.emit(|n| {
                    StreamingEvent::FunctionCallArgumentsDone(FunctionCallArgumentsEvent {
                        sequence_number: n,
                        item_id,
                        output_index,
                        arguments,
                    })
                });
                self.item_done(output_index);
            }
            OpenContent::Thinking {
                thinking,
                signature,
            } => {
                self.builder.push(Block::Thinking {
                    thinking: thinking.clone(),
                    signature,
                });
                let item_id = placement.item_id.clone();
                let output_index = placement.output_index;
                self.emit(|n| {
                    StreamingEvent::ReasoningDone(ContentDeltaEvent {
                        sequence_number: n,
                        item_id,
                        output_index,
                        content_index: 0,
                        delta: thinking,
                    })
                });
                self.item_done(output_index);
            }
            OpenContent::Whole {
                mut raw,
                input_json,
            } => {
                if let Ok(input) = serde_json::from_str::<Value>(&input_json)
                    && let Some(object) = raw.as_object_mut()
                {
                    object.insert("input".to_owned(), input);
                }
                self.stop_whole(Block::parse(raw), cut);
            }
        }
    }

    /// A web search call is reported done when its result block arrives, or
    /// at the end of the stream.
    fn stop_whole(&mut self, block: Block, cut: bool) {
        let is_search_call = matches!(block, Block::WebSearchCall { .. });
        let placement = self.builder.push(block);
        let output_index = placement.output_index;
        if cut {
            self.builder.set_incomplete(output_index);
        }
        if placement.new_item {
            let item = self.builder.items().get(output_index as usize).cloned();
            if let Some(mut item) = item {
                if let (true, OutputItem::WebSearchCall(call)) = (is_search_call, &mut item) {
                    call.status = ItemStatus::InProgress;
                }
                self.item_added(&placement, item);
            }
        }
        if is_search_call {
            self.open_searches.push(output_index);
        } else {
            self.open_searches.retain(|index| *index != output_index);
            self.item_done(output_index);
        }
    }

    fn stop_text(&mut self, placement: &Placement, text: String, citations: Vec<Value>) {
        self.builder.push(Block::Text {
            text: text.clone(),
            citations,
        });
        let part = match self.builder.items().get(placement.output_index as usize) {
            Some(OutputItem::Message(message)) => message
                .content
                .get(placement.content_index as usize)
                .cloned(),
            _ => None,
        };
        let annotations = match &part {
            Some(OutputContentPart::OutputText(part)) => part.annotations.clone(),
            _ => Vec::new(),
        };
        for (annotation_index, annotation) in annotations.into_iter().enumerate() {
            let event = OutputTextAnnotationAddedEvent {
                sequence_number: 0,
                item_id: placement.item_id.clone(),
                output_index: placement.output_index,
                content_index: placement.content_index,
                annotation_index: u32::try_from(annotation_index).unwrap_or(u32::MAX),
                annotation: Some(annotation),
            };
            self.emit(|n| {
                StreamingEvent::OutputTextAnnotationAdded(OutputTextAnnotationAddedEvent {
                    sequence_number: n,
                    ..event
                })
            });
        }
        let done = OutputTextEvent {
            sequence_number: 0,
            item_id: placement.item_id.clone(),
            output_index: placement.output_index,
            content_index: placement.content_index,
            text,
            logprobs: None,
        };
        self.emit(|n| {
            StreamingEvent::OutputTextDone(OutputTextEvent {
                sequence_number: n,
                ..done
            })
        });
        if let Some(part) = part {
            let event = ContentPartEvent {
                sequence_number: 0,
                item_id: placement.item_id.clone(),
                output_index: placement.output_index,
                content_index: placement.content_index,
                part,
            };
            self.emit(|n| {
                StreamingEvent::ContentPartDone(ContentPartEvent {
                    sequence_number: n,
                    ..event
                })
            });
        }
    }

    fn complete(&mut self) {
        if let Some(err) = response::refused_without_output(
            self.stop_reason.as_deref(),
            self.builder.items(),
            self.stop_details.as_ref(),
            &self.envelope,
        ) {
            self.fail(&err);
            return;
        }
        let (status, _) = response::status_for(self.stop_reason.as_deref());
        if status == ResponseStatus::Incomplete {
            self.builder.set_last_incomplete();
        }
        self.close_items();
        let resource = response::finish(
            self.snapshot.clone(),
            &self.envelope,
            self.builder.items().to_vec(),
            self.stop_reason.as_deref(),
            &self.usage,
        );
        let incomplete = resource.status == ResponseStatus::Incomplete;
        self.emit(|n| {
            let event = snapshot_event(n, resource);
            if incomplete {
                StreamingEvent::Incomplete(event)
            } else {
                StreamingEvent::Completed(event)
            }
        });
        self.finished = true;
    }

    /// Report every item still open as done.
    fn close_items(&mut self) {
        self.close_message();
        for output_index in std::mem::take(&mut self.open_searches) {
            self.item_done(output_index);
        }
    }

    /// End the stream with `response.failed`, keeping the output so far: a
    /// block cut off mid-stream is closed as `incomplete`.
    fn fail(&mut self, err: &LlmGatewayError) {
        if let Some(open) = self.open.take() {
            self.stop_block(open, true);
        }
        self.close_items();
        let mut resource = self.snapshot.clone();
        resource.status = ResponseStatus::Failed;
        resource.error = Some(err.to_response_error());
        resource.output = self.builder.items().to_vec();
        resource.usage = Some(response::usage(&self.usage, false));
        resource.completed_at = Some(response::now_unix());
        resource.provider_details = Some(response::provider_details(
            self.stop_reason.as_deref(),
            &self.envelope,
        ));
        self.emit(|n| StreamingEvent::Failed(snapshot_event(n, resource)));
        self.finished = true;
    }
}

fn snapshot_event(sequence_number: u64, response: ResponseResource) -> ResponseSnapshotEvent {
    ResponseSnapshotEvent {
        sequence_number,
        response,
    }
}

/// Overlay the fields of a `message_delta` usage onto the accumulated usage.
pub fn merge_usage(usage: &mut Value, delta: Value) {
    match (usage.as_object_mut(), delta) {
        (Some(current), Value::Object(delta)) => {
            for (key, value) in delta {
                if !value.is_null() {
                    current.insert(key, value);
                }
            }
        }
        (None, delta @ Value::Object(_)) => *usage = delta,
        _ => {}
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "stream_tests.rs"]
mod stream_tests;
