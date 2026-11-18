// Created: 2026-11-09 by Constructor Tech
//! Anthropic provider type: the shape of `provider_settings` for models whose
//! provider has this `gts_type`.
//!
//! Holds the API headers and the Messages API defaults the
//! request leaves unset. Pricing is the generic `ModelInfoV1.cost`.

use model_registry_sdk::ProviderTypeV1;
use toolkit_gts::gts_type_schema;

/// Service tier. `StandardOnly` opts out of priority capacity.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicServiceTier {
    Auto,
    StandardOnly,
}

/// `output_config.effort` level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicOutputEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// How thinking content appears in the response.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicThinkingDisplay {
    Summarized,
    Omitted,
}

/// Default `thinking` configuration.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnthropicThinking {
    Enabled {
        budget_tokens: u32,
        display: Option<AnthropicThinkingDisplay>,
    },
    Disabled,
    Adaptive {
        display: Option<AnthropicThinkingDisplay>,
    },
}

/// Default `tool_choice`.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnthropicToolChoice {
    Auto {
        disable_parallel_tool_use: Option<bool>,
    },
    Any {
        disable_parallel_tool_use: Option<bool>,
    },
    Tool {
        name: String,
        disable_parallel_tool_use: Option<bool>,
    },
    None,
}

/// `output_config.format`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicJsonOutputFormat {
    JsonSchema { schema: serde_json::Value },
}

/// Default `output_config`.
#[derive(
    Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct AnthropicOutputConfig {
    pub effort: Option<AnthropicOutputEffort>,
    pub format: Option<AnthropicJsonOutputFormat>,
}

/// Anthropic provider settings (Messages API).
#[derive(Debug, Clone, PartialEq, Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = ProviderTypeV1,
    type_id = gts_id!("cf.genai.model.provider.v1~cf.genai._.anthropic.v1~"),
    description = "Anthropic provider settings (Messages API)",
    properties = "anthropic_version,anthropic_beta,max_tokens,stop_sequences,system,inference_geo,service_tier,thinking,tool_choice,output_config"
)]
pub struct AnthropicProviderSettingsV1 {
    /// `anthropic-version` header; empty uses the plugin default.
    pub anthropic_version: String,
    /// `anthropic-beta` flags.
    pub anthropic_beta: Vec<String>,
    /// Default `max_tokens`; Anthropic requires it on every request.
    pub max_tokens: Option<u32>,
    pub stop_sequences: Option<Vec<String>>,
    /// System prompt placed before the request instructions.
    pub system: Option<String>,
    pub inference_geo: Option<String>,
    pub service_tier: Option<AnthropicServiceTier>,
    pub thinking: Option<AnthropicThinking>,
    pub tool_choice: Option<AnthropicToolChoice>,
    pub output_config: Option<AnthropicOutputConfig>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "settings_tests.rs"]
mod settings_tests;
