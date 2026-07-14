// Created: 2026-07-09 by Constructor Tech
//! Tool models — from `schemas/tools/`.
//!
//! Tools form a `type`-discriminated family, mapped here to a tagged enum.
//! `parameters` and inline schemas stay as `serde_json::Value` since they are
//! arbitrary JSON Schema.

use crate::models::extension::{Extension, from_tagged, serialize_tagged, tag_of};

// ---------------------------------------------------------------------------
// Tool
// ---------------------------------------------------------------------------

/// A tool definition, discriminated by `type`.
///
/// Core-owned tool types have named variants; any other `type` — a provider or
/// third-party plugin extension the core does not own — is preserved verbatim
/// in [`Tool::Other`] and forwarded without interpretation.
#[derive(Debug, Clone, PartialEq, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Tool {
    /// A caller-defined function tool.
    Function(FunctionTool),
    /// A reference to a tool schema in the Type Registry.
    Reference(ToolReference),
    /// An inline GTS schema definition.
    InlineGts(ToolInlineGts),
    /// The built-in image-generation tool (Gears extension).
    #[serde(rename = "cf_gears:image_generation")]
    ImageGeneration(ImageGenerationTool),
    /// A tool `type` the core does not own, preserved verbatim.
    #[serde(skip)]
    Other(Extension),
}

impl serde::Serialize for Tool {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Function(v) => serialize_tagged(serializer, "function", v),
            Self::Reference(v) => serialize_tagged(serializer, "reference", v),
            Self::InlineGts(v) => serialize_tagged(serializer, "inline_gts", v),
            Self::ImageGeneration(v) => {
                serialize_tagged(serializer, "cf_gears:image_generation", v)
            }
            Self::Other(ext) => serde::Serialize::serialize(&ext.0, serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Tool {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(tag) = tag_of(&value) {
            match tag {
                "function" => return from_tagged(&value).map(Self::Function),
                "reference" => return from_tagged(&value).map(Self::Reference),
                "inline_gts" => return from_tagged(&value).map(Self::InlineGts),
                "cf_gears:image_generation" => {
                    return from_tagged(&value).map(Self::ImageGeneration);
                }
                _ => {}
            }
        }
        Ok(Self::Other(Extension(value)))
    }
}

/// Function tool.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FunctionTool {
    /// Function name (schema pattern `^[a-zA-Z0-9_-]+$`, 1..=64 chars).
    pub name: String,
    /// Function description for the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the function parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    /// Strict schema enforcement for function arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Tool reference.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ToolReference {
    /// GTS identifier of the tool schema in the Type Registry.
    pub schema_id: String,
}

/// Inline GTS tool schema.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ToolInlineGts {
    /// Inline schema definition.
    pub schema: Schema,
}

/// JSON Schema wrapper.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Schema {
    /// JSON Schema definition.
    pub json_schema: serde_json::Value,
}

/// Image-generation tool (Gears extension).
///
/// All fields default to null, deferring to the provider/gateway default.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ImageGenerationTool {
    /// Output image aspect ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<AspectRatio>,
    /// Resolution in megapixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// Generation quality level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<ImageQuality>,
    /// Image file format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<ImageOutputFormat>,
    /// Compression level 0..=100 (applies to jpeg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_compression: Option<u8>,
    /// How the generated image is returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ImageResponseFormat>,
}

/// Output image aspect ratio.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum AspectRatio {
    #[serde(rename = "1:1")]
    Square,
    #[serde(rename = "2:3")]
    TwoByThree,
    #[serde(rename = "3:2")]
    ThreeByTwo,
    #[serde(rename = "3:4")]
    ThreeByFour,
    #[serde(rename = "4:3")]
    FourByThree,
    #[serde(rename = "9:16")]
    NineBySixteen,
    #[serde(rename = "16:9")]
    SixteenByNine,
}

/// Output resolution in megapixels.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum Resolution {
    #[serde(rename = "0.5")]
    Half,
    #[serde(rename = "1")]
    One,
    #[serde(rename = "2")]
    Two,
    #[serde(rename = "4")]
    Four,
}

/// Image generation quality level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ImageQuality {
    Low,
    Medium,
    High,
    Auto,
}

/// Generated image file format.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ImageOutputFormat {
    Png,
    Jpeg,
    Webp,
}

/// How a generated image is returned.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ImageResponseFormat {
    Base64,
    Url,
}
