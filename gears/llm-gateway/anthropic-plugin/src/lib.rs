// Created: 2026-11-09 by Constructor Tech
//! LLM Gateway provider plugin for the Anthropic Messages API.
//!
//! [`AnthropicPlugin`] implements
//! [`llm_gateway_sdk::LlmGatewayProviderPluginClientV1`]: it translates Open
//! Responses requests to `POST /v1/messages`, sends them through OAGW to the
//! model's upstream alias, and translates responses and SSE streams back.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod client;
mod error;
mod request;
mod response;
mod schema;
pub mod settings;
mod stream;
mod wire;

pub use client::AnthropicPlugin;
pub use settings::AnthropicProviderSettingsV1;
