// Created: 2026-11-25 by Constructor Tech
//! Configuration of the Anthropic provider plugin gear.

use serde::Deserialize;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnthropicPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,
}

impl Default for AnthropicPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 100,
        }
    }
}
