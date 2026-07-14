// Created: 2026-07-09 by Constructor Tech
//! Content-part models — from `schemas/content/`.
//!
//! Input and output content parts are `type`-discriminated unions. Leaf structs
//! carry only the non-discriminator fields; the enum tag owns `type`. Size caps
//! in the schemas are documented, not type-enforced.

use crate::models::extension::{Extension, from_tagged, serialize_tagged, tag_of};

// ---------------------------------------------------------------------------
// InputContentPart
// ---------------------------------------------------------------------------

/// Union of input content-part types, discriminated by `type`.
///
/// Variant names mirror the schema titles (`InputText`, `InputImage`, …). Any
/// `type` the core does not own — a provider or plugin extension — is preserved
/// verbatim in [`InputContentPart::Other`].
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
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
    /// A content-part `type` the core does not own, preserved verbatim.
    #[serde(skip)]
    Other(Extension),
}

impl serde::Serialize for InputContentPart {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::InputText(v) => serialize_tagged(serializer, "input_text", v),
            Self::InputImage(v) => serialize_tagged(serializer, "input_image", v),
            Self::InputAudio(v) => serialize_tagged(serializer, "input_audio", v),
            Self::InputVideo(v) => serialize_tagged(serializer, "input_video", v),
            Self::InputFile(v) => serialize_tagged(serializer, "input_file", v),
            Self::Other(ext) => serde::Serialize::serialize(&ext.0, serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for InputContentPart {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(tag) = tag_of(&value) {
            match tag {
                "input_text" => return from_tagged(&value).map(Self::InputText),
                "input_image" => return from_tagged(&value).map(Self::InputImage),
                "input_audio" => return from_tagged(&value).map(Self::InputAudio),
                "input_video" => return from_tagged(&value).map(Self::InputVideo),
                "input_file" => return from_tagged(&value).map(Self::InputFile),
                _ => {}
            }
        }
        Ok(Self::Other(Extension(value)))
    }
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
///
/// Any `type` the core does not own — a provider or plugin extension — is
/// preserved verbatim in [`OutputContentPart::Other`].
#[derive(Debug, Clone, PartialEq, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutputContentPart {
    /// Generated text with annotations.
    OutputText(OutputText),
    /// Refusal to generate content.
    Refusal(Refusal),
    /// A content-part `type` the core does not own, preserved verbatim.
    #[serde(skip)]
    Other(Extension),
}

impl serde::Serialize for OutputContentPart {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::OutputText(v) => serialize_tagged(serializer, "output_text", v),
            Self::Refusal(v) => serialize_tagged(serializer, "refusal", v),
            Self::Other(ext) => serde::Serialize::serialize(&ext.0, serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for OutputContentPart {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(tag) = tag_of(&value) {
            match tag {
                "output_text" => return from_tagged(&value).map(Self::OutputText),
                "refusal" => return from_tagged(&value).map(Self::Refusal),
                _ => {}
            }
        }
        Ok(Self::Other(Extension(value)))
    }
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
/// Any `type` the core does not own — a provider or plugin extension — is
/// preserved verbatim in [`Annotation::Other`].
#[derive(Debug, Clone, PartialEq, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Annotation {
    /// A cited URL with the character span it annotates.
    UrlCitation(UrlCitation),
    /// An annotation `type` the core does not own, preserved verbatim.
    #[serde(skip)]
    Other(Extension),
}

impl serde::Serialize for Annotation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::UrlCitation(v) => serialize_tagged(serializer, "url_citation", v),
            Self::Other(ext) => serde::Serialize::serialize(&ext.0, serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Annotation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some("url_citation") = tag_of(&value) {
            return from_tagged(&value).map(Self::UrlCitation);
        }
        Ok(Self::Other(Extension(value)))
    }
}

/// A cited URL with the character span it annotates.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct UrlCitation {
    /// Cited URL.
    pub url: String,
    /// Citation title.
    pub title: String,
    /// Start character index in the output text.
    pub start_index: u32,
    /// End character index in the output text.
    pub end_index: u32,
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
