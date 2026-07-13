// Created: 2026-07-09 by Constructor Tech
//! Content-part models — from `schemas/content/`.
//!
//! Input and output content parts are `type`-discriminated unions. Leaf structs
//! carry only the non-discriminator fields; the enum tag owns `type`. Size caps
//! in the schemas are documented, not type-enforced.

// ---------------------------------------------------------------------------
// InputContentPart
// ---------------------------------------------------------------------------

/// Union of input content-part types, discriminated by `type`.
///
/// Variant names mirror the schema titles (`InputText`, `InputImage`, …).
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContentPart {
    /// Text content.
    InputText(InputText),
    /// Image content.
    InputImage(InputImage),
    /// Audio content.
    InputAudio(InputAudio),
    /// Video content.
    InputVideo(InputVideo),
    /// File/document content.
    InputFile(InputFile),
}

/// Input text content.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct InputText {
    /// Text content (schema maximum 10 MiB).
    pub text: String,
}

/// Input image content.
///
/// At least one of `image_url` or `file_id` should be set (not type-enforced).
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct InputImage {
    /// Image URL or base64 data URI (schema maximum 20 MiB).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// File-storage identifier (Gears extension).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    /// Detail level for vision models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageDetail>,
}

/// Input audio content.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct InputAudio {
    /// Audio URL (file-storage or external).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Base64-encoded audio data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Audio format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<AudioFormat>,
}

/// Input video content.
///
/// Exactly one of `url` or `file_id` should be set (not type-enforced).
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct InputVideo {
    /// Video URL (file-storage or external).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// File-storage identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

/// Input file/document content.
///
/// At least one of `file_data`, `file_url`, or `file_id` should be set (not
/// type-enforced).
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct InputFile {
    /// Original filename for display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Base64-encoded file data (schema maximum 32 MiB).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_data: Option<String>,
    /// File URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_url: Option<String>,
    /// File-storage identifier (Gears extension).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

/// Image detail level for vision models.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Low,
    High,
    Auto,
}

/// Input audio format.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    Wav,
    Mp3,
    Flac,
    Opus,
    Pcm16,
}

// ---------------------------------------------------------------------------
// OutputContentPart
// ---------------------------------------------------------------------------

/// Union of output content-part types, discriminated by `type`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContentPart {
    /// Generated text with annotations.
    OutputText(OutputText),
    /// Refusal to generate content.
    Refusal(Refusal),
}

/// Output text content.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputText {
    /// Generated text content.
    pub text: String,
    /// Text annotations (citations, links).
    pub annotations: Vec<Annotation>,
    /// Token log-probabilities, when requested via `include`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Vec<LogProb>>,
}

/// Refusal content.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Refusal {
    /// Refusal explanation (schema maximum 10 MiB).
    pub refusal: String,
}

/// Annotation within output text, discriminated by `type`.
///
/// Currently the sole variant is a URL citation; modeled as a tagged enum so
/// future annotation kinds slot in without a wire change.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Annotation {
    /// A cited URL with the character span it annotates.
    UrlCitation {
        /// Cited URL.
        url: String,
        /// Citation title.
        title: String,
        /// Start character index in the output text.
        start_index: u32,
        /// End character index in the output text.
        end_index: u32,
    },
}

/// Log-probability for a single output token.
///
/// Reused by the streaming text events, where `bytes` may be omitted, hence the
/// `#[serde(default)]` tolerance.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LogProb {
    /// The token.
    pub token: String,
    /// Log-probability of the token.
    pub logprob: f64,
    /// UTF-8 bytes of the token.
    #[serde(default)]
    pub bytes: Vec<u8>,
    /// Alternative tokens and their log-probabilities.
    pub top_logprobs: Vec<TopLogProb>,
}

/// An alternative token considered at a position.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TopLogProb {
    /// The token.
    pub token: String,
    /// Log-probability of the token.
    pub logprob: f64,
    /// UTF-8 bytes of the token.
    #[serde(default)]
    pub bytes: Vec<u8>,
}
