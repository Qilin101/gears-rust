// Created: 2026-10-28 by Constructor Tech
// Updated: 2026-10-28 by Constructor Tech
//! [`AvailableModelV1`] — the model as listed for evaluation (DESIGN §3.1
//! Available Model): every [`ModelV1`] field except `info.provider_settings`,
//! `info.base_url`, and `info.secret_ref`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::models::{
    ApprovalV1, ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities,
    ModelCapabilities, ModelInfoV1, ModelPerformance, ModelV1, SharingMode, SupportedApi,
};

/// [`ModelInfoV1`] without `base_url`, `secret_ref`, and `provider_settings`.
/// Field semantics are those of `ModelInfoV1`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AvailableModelInfoV1 {
    pub gts_type: gts::GtsTypeId,
    pub provider_model_id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub family: Option<String>,
    pub vendor: Option<String>,
    pub managed: bool,
    pub architecture: Option<String>,
    pub size_bytes: Option<u64>,
    pub format: Option<String>,
    pub region: Option<String>,
    pub hosted_by: Option<String>,
    pub last_release_at: Option<DateTime<Utc>>,
    pub reasoning_level: Option<String>,
    pub version: Option<String>,
    pub sort_order: i32,
    pub icon: Option<String>,
    pub multiplier_display: Option<String>,
    pub performance: ModelPerformance,
    pub additional_info: HashMap<String, serde_json::Value>,
    pub supported_api: HashSet<SupportedApi>,
    pub capabilities: ModelCapabilities,
    pub disabled_capabilities: DisabledCapabilities,
    pub context_window: ContextWindow,
    pub default_parameters: DefaultInferenceParametersV1,
    pub allow_parameter_override: bool,
    pub allow_extra_params: Vec<String>,
}

impl From<ModelInfoV1> for AvailableModelInfoV1 {
    fn from(i: ModelInfoV1) -> Self {
        let ModelInfoV1 {
            gts_type,
            provider_model_id,
            base_url: _,
            secret_ref: _,
            display_name,
            description,
            family,
            vendor,
            managed,
            architecture,
            size_bytes,
            format,
            region,
            hosted_by,
            last_release_at,
            reasoning_level,
            version,
            sort_order,
            icon,
            multiplier_display,
            performance,
            additional_info,
            supported_api,
            capabilities,
            disabled_capabilities,
            context_window,
            default_parameters,
            allow_parameter_override,
            allow_extra_params,
            provider_settings: _,
        } = i;
        Self {
            gts_type,
            provider_model_id,
            display_name,
            description,
            family,
            vendor,
            managed,
            architecture,
            size_bytes,
            format,
            region,
            hosted_by,
            last_release_at,
            reasoning_level,
            version,
            sort_order,
            icon,
            multiplier_display,
            performance,
            additional_info,
            supported_api,
            capabilities,
            disabled_capabilities,
            context_window,
            default_parameters,
            allow_parameter_override,
            allow_extra_params,
        }
    }
}

/// A model as returned by `list_available_models` and the
/// `/available-models` REST routes. Same fields as [`ModelV1`] with
/// [`AvailableModelInfoV1`] as `info`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AvailableModelV1 {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider_id: Uuid,
    pub canonical_id: String,
    pub sharing_mode: SharingMode,
    pub approval: ApprovalV1,
    pub info: AvailableModelInfoV1,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
}

impl From<ModelV1> for AvailableModelV1 {
    fn from(m: ModelV1) -> Self {
        Self {
            id: m.id,
            tenant_id: m.tenant_id,
            provider_id: m.provider_id,
            canonical_id: m.canonical_id,
            sharing_mode: m.sharing_mode,
            approval: m.approval,
            info: m.info.into(),
            created_at: m.created_at,
            created_by: m.created_by,
            updated_at: m.updated_at,
            updated_by: m.updated_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REDACTED: [&str; 3] = ["base_url", "secret_ref", "provider_settings"];

    #[test]
    fn available_model_drops_exactly_the_redacted_fields() {
        let model = crate::models::entity::test_support::sample_model();
        let full = serde_json::to_value(&model).expect("serializes");
        let available = AvailableModelV1::from(model);
        let json = serde_json::to_value(&available).expect("serializes");

        let full_info = full["info"].as_object().expect("object");
        let info = json["info"].as_object().expect("object");
        for key in REDACTED {
            assert!(full_info.contains_key(key), "{key}");
            assert!(!info.contains_key(key), "{key}");
        }
        assert_eq!(info.len(), full_info.len() - REDACTED.len());
        for (key, value) in info {
            assert_eq!(&full_info[key], value, "{key}");
        }
        for key in [
            "id",
            "canonical_id",
            "sharing_mode",
            "approval",
            "created_at",
        ] {
            assert_eq!(json[key], full[key], "{key}");
        }

        let back: AvailableModelV1 = serde_json::from_value(json).expect("deserializes");
        assert_eq!(back, available);
    }
}
