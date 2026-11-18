// Created: 2026-11-09 by Constructor Tech
use gts::GtsSchema;
use serde_json::json;

use super::*;

const TYPE_ID: &str = "gts.cf.genai.model.provider.v1~cf.genai._.anthropic.v1~";

fn document() -> serde_json::Value {
    json!({
        "anthropic_version": "2026-10-21",
        "anthropic_beta": ["files-api-2025-04-14"],
        "max_tokens": 4096,
        "stop_sequences": null,
        "system": null,
        "inference_geo": null,
        "service_tier": "standard_only",
        "thinking": { "type": "adaptive", "display": "summarized" },
        "tool_choice": { "type": "auto", "disable_parallel_tool_use": true },
        "output_config": { "effort": "high", "format": null }
    })
}

#[test]
fn type_derives_from_provider_base_type() {
    assert_eq!(AnthropicProviderSettingsV1::TYPE_ID, TYPE_ID);
    assert!(TYPE_ID.starts_with(model_registry_sdk::PROVIDER_BASE_TYPE));
}

#[test]
fn schema_is_registered_with_settings_properties() {
    let entry = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
        .into_iter()
        .find(|e| e.type_id == TYPE_ID)
        .expect("Anthropic provider type registered in the GTS inventory");
    let schema = (entry.schema_fn)();
    for property in [
        "anthropic_version",
        "anthropic_beta",
        "thinking",
        "output_config",
    ] {
        assert!(schema.contains(property), "{property} missing: {schema}");
    }
}

#[test]
fn narrows_settings_document() {
    let settings = gts::try_narrow::<AnthropicProviderSettingsV1>(TYPE_ID, document()).unwrap();
    assert_eq!(settings.anthropic_version, "2026-10-21");
    assert_eq!(settings.max_tokens, Some(4096));
    assert_eq!(
        settings.service_tier,
        Some(AnthropicServiceTier::StandardOnly)
    );
    assert_eq!(
        settings.thinking,
        Some(AnthropicThinking::Adaptive {
            display: Some(AnthropicThinkingDisplay::Summarized)
        })
    );
}

#[test]
fn rejects_another_provider_type() {
    let err = gts::try_narrow::<AnthropicProviderSettingsV1>(
        "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~",
        document(),
    )
    .unwrap_err();
    assert!(matches!(err, gts::NarrowError::SchemaId { .. }));
}
