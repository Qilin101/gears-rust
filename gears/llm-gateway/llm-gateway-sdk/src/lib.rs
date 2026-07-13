// Created: 2026-07-09 by Constructor Tech
//! LLM Gateway SDK
//!
//! Rust models for the LLM Gateway's Open Responses–aligned domain, translated
//! from the JSON Schemas under `llm-gateway-sdk/schemas/`. Covers the core
//! request/response, item, content, tool, async (job/batch), and streaming
//! (server-sent event) families.
//!
//! Schema polymorphism (an `allOf` chain with a `const` `type` discriminator)
//! maps to plain serde enums (`#[serde(tag = "type")]`).

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod models;

pub use models::async_ops::{AsyncError, Batch, BatchRequest, BatchStatus, Job, JobStatus};
pub use models::content::{
    Annotation, AudioFormat, ImageDetail, InputAudio, InputContentPart, InputFile, InputImage,
    InputText, InputVideo, LogProb, OutputContentPart, OutputText, Refusal, TopLogProb,
};
pub use models::core::{
    CreateResponseBody, EmbeddingData, EmbeddingInput, EmbeddingRequest, EmbeddingResponse,
    EmbeddingVector, EncodingFormat, FallbackConfig, FallbackStrategy, IncludeField,
    IncompleteDetails, InputTokensDetails, NamedToolChoice, OutputTokensDetails, ReasoningConfig,
    ReasoningEffort, ReasoningSummary, ResponseError, ResponseInput, ResponseResource,
    ResponseStatus, Role, ServiceTier, StreamOptions, TextFormat, TextFormatKind, TextVerbosity,
    ToolChoice, ToolChoiceMode, TruncationStrategy, Usage,
};
pub use models::items::{
    DataOutput, FunctionCallItem, FunctionCallOutputItem, InputContent, InputItem, ItemReference,
    ItemStatus, MessageItem, MessageOutput, OutputItem, ReasoningContentPart, ReasoningItem,
    ReasoningOutput, ReasoningSummaryPart,
};
pub use models::streaming::{
    ContentDeltaEvent, ContentPartEvent, DataEvent, ErrorEvent, FunctionCallArgumentsDeltaEvent,
    FunctionCallArgumentsDoneEvent, OutputItemAddedEvent, OutputItemDoneEvent,
    OutputTextAnnotationAddedEvent, OutputTextDeltaEvent, OutputTextDoneEvent, ReasoningDoneEvent,
    ReasoningSummaryTextDeltaEvent, ReasoningSummaryTextDoneEvent, RefusalDoneEvent,
    ResponseSnapshotEvent, StreamingEvent, SummaryPartEvent,
};
pub use models::tools::{
    AspectRatio, FunctionTool, ImageGenerationTool, ImageOutputFormat, ImageQuality,
    ImageResponseFormat, Resolution, Schema, Tool, ToolInlineGts, ToolReference,
};
