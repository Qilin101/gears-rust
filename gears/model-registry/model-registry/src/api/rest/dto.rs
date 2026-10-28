//! REST DTOs (DESIGN §3.3 REST Endpoints). Entry DTOs flatten the SDK
//! entries: the provider or model fields plus the flags at the top level.
//! The nested Model Info objects travel as JSON and are parsed into the SDK
//! types by `parse`.

use chrono::{DateTime, Utc};
use model_registry_sdk::{
    ApprovalV1, AvailableModelInfoV1, AvailableModelV1, ModelEntryV1, ModelInfoV1, ModelV1,
    ProviderEntryV1, ProviderV1,
};
use serde_json::Value as JsonValue;
use toolkit_macros::api_dto;
use uuid::Uuid;

/// Deserializes `Option<Option<T>>` so that an absent key is `None`, an
/// explicit `null` is `Some(None)`, and a value is `Some(Some(v))`.
#[allow(clippy::option_option)]
pub(crate) mod nullable_opt {
    use serde::Deserialize;

    pub fn deserialize<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

/// `ProviderEntryV1`, flattened.
#[allow(clippy::struct_excessive_bools)]
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ProviderEntryDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub slug: String,
    pub name: String,
    pub gts_type: String,
    pub enabled: bool,
    pub sharing_mode: String,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
    pub inherited: bool,
    pub shadowed: bool,
    pub available_for_eval: bool,
}

impl From<ProviderEntryV1> for ProviderEntryDto {
    fn from(e: ProviderEntryV1) -> Self {
        let ProviderV1 {
            id,
            tenant_id,
            slug,
            name,
            gts_type,
            enabled,
            sharing_mode,
            managed,
            metadata,
            created_at,
            created_by,
            updated_at,
            updated_by,
        } = e.provider;
        Self {
            id,
            tenant_id,
            slug,
            name,
            gts_type: gts_type.to_string(),
            enabled,
            sharing_mode: sharing_mode.as_str().to_owned(),
            managed,
            metadata,
            created_at,
            created_by,
            updated_at,
            updated_by,
            inherited: e.inherited,
            shadowed: e.shadowed,
            available_for_eval: e.available_for_eval,
        }
    }
}

#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct CreateProviderRequestDto {
    pub slug: String,
    pub name: String,
    pub gts_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
}

#[api_dto(request)]
#[derive(Debug, Clone, Default)]
#[allow(clippy::option_option)]
pub struct UpdateProviderRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    /// `null` clears the metadata.
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<Option<JsonValue>>,
}

#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ProviderEntryPageDto {
    pub items: Vec<ProviderEntryDto>,
    pub page_info: PageInfoDto,
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ApprovalDto {
    pub state: String,
    pub decided_by: Uuid,
    pub decided_at: DateTime<Utc>,
}

impl From<ApprovalV1> for ApprovalDto {
    fn from(a: ApprovalV1) -> Self {
        Self {
            state: a.state.as_str().to_owned(),
            decided_by: a.decided_by,
            decided_at: a.decided_at,
        }
    }
}

/// `ModelInfoV1` on the wire. Nested objects keep the SDK JSON shape.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ModelInfoDto {
    pub gts_type: String,
    pub provider_model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosted_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_release_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub sort_order: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier_display: Option<String>,
    pub performance: JsonValue,
    pub additional_info: JsonValue,
    pub supported_api: Vec<String>,
    pub capabilities: JsonValue,
    pub disabled_capabilities: JsonValue,
    pub context_window: JsonValue,
    pub default_parameters: JsonValue,
    pub allow_parameter_override: bool,
    pub allow_extra_params: Vec<String>,
    pub provider_settings: JsonValue,
}

fn to_json<T: serde::Serialize>(value: &T) -> JsonValue {
    serde_json::to_value(value).unwrap_or(JsonValue::Null)
}

impl From<ModelInfoV1> for ModelInfoDto {
    fn from(i: ModelInfoV1) -> Self {
        let mut supported_api: Vec<String> = i
            .supported_api
            .iter()
            .map(|a| a.as_str().to_owned())
            .collect();
        supported_api.sort();
        Self {
            gts_type: i.gts_type.to_string(),
            provider_model_id: i.provider_model_id,
            base_url: i.base_url,
            secret_ref: i.secret_ref,
            display_name: i.display_name,
            description: i.description,
            family: i.family,
            vendor: i.vendor,
            managed: i.managed,
            architecture: i.architecture,
            size_bytes: i.size_bytes,
            format: i.format,
            region: i.region,
            hosted_by: i.hosted_by,
            last_release_at: i.last_release_at,
            reasoning_level: i.reasoning_level,
            version: i.version,
            sort_order: i.sort_order,
            icon: i.icon,
            multiplier_display: i.multiplier_display,
            performance: to_json(&i.performance),
            additional_info: to_json(&i.additional_info),
            supported_api,
            capabilities: to_json(&i.capabilities),
            disabled_capabilities: to_json(&i.disabled_capabilities),
            context_window: to_json(&i.context_window),
            default_parameters: to_json(&i.default_parameters),
            allow_parameter_override: i.allow_parameter_override,
            allow_extra_params: i.allow_extra_params,
            provider_settings: i.provider_settings,
        }
    }
}

/// `AvailableModelInfoV1` on the wire: `ModelInfoDto` without `base_url`,
/// `secret_ref`, and `provider_settings`.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct AvailableModelInfoDto {
    pub gts_type: String,
    pub provider_model_id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosted_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_release_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub sort_order: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier_display: Option<String>,
    pub performance: JsonValue,
    pub additional_info: JsonValue,
    pub supported_api: Vec<String>,
    pub capabilities: JsonValue,
    pub disabled_capabilities: JsonValue,
    pub context_window: JsonValue,
    pub default_parameters: JsonValue,
    pub allow_parameter_override: bool,
    pub allow_extra_params: Vec<String>,
}

impl From<AvailableModelInfoV1> for AvailableModelInfoDto {
    fn from(i: AvailableModelInfoV1) -> Self {
        let mut supported_api: Vec<String> = i
            .supported_api
            .iter()
            .map(|a| a.as_str().to_owned())
            .collect();
        supported_api.sort();
        Self {
            gts_type: i.gts_type.to_string(),
            provider_model_id: i.provider_model_id,
            display_name: i.display_name,
            description: i.description,
            family: i.family,
            vendor: i.vendor,
            managed: i.managed,
            architecture: i.architecture,
            size_bytes: i.size_bytes,
            format: i.format,
            region: i.region,
            hosted_by: i.hosted_by,
            last_release_at: i.last_release_at,
            reasoning_level: i.reasoning_level,
            version: i.version,
            sort_order: i.sort_order,
            icon: i.icon,
            multiplier_display: i.multiplier_display,
            performance: to_json(&i.performance),
            additional_info: to_json(&i.additional_info),
            supported_api,
            capabilities: to_json(&i.capabilities),
            disabled_capabilities: to_json(&i.disabled_capabilities),
            context_window: to_json(&i.context_window),
            default_parameters: to_json(&i.default_parameters),
            allow_parameter_override: i.allow_parameter_override,
            allow_extra_params: i.allow_extra_params,
        }
    }
}

/// `AvailableModelV1` on the wire (resolution responses).
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct AvailableModelDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider_id: Uuid,
    pub canonical_id: String,
    pub sharing_mode: String,
    pub approval: ApprovalDto,
    pub info: AvailableModelInfoDto,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
}

impl From<AvailableModelV1> for AvailableModelDto {
    fn from(m: AvailableModelV1) -> Self {
        Self {
            id: m.id,
            tenant_id: m.tenant_id,
            provider_id: m.provider_id,
            canonical_id: m.canonical_id,
            sharing_mode: m.sharing_mode.as_str().to_owned(),
            approval: m.approval.into(),
            info: m.info.into(),
            created_at: m.created_at,
            created_by: m.created_by,
            updated_at: m.updated_at,
            updated_by: m.updated_by,
        }
    }
}

/// `ModelEntryV1`, flattened.
#[allow(clippy::struct_excessive_bools)]
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ModelEntryDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider_id: Uuid,
    pub canonical_id: String,
    pub sharing_mode: String,
    pub approval: ApprovalDto,
    pub info: ModelInfoDto,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
    pub inherited: bool,
    pub shadowed: bool,
    pub provider_enabled: bool,
    pub available_for_eval: bool,
}

impl From<ModelEntryV1> for ModelEntryDto {
    fn from(e: ModelEntryV1) -> Self {
        let m: ModelV1 = e.model;
        Self {
            id: m.id,
            tenant_id: m.tenant_id,
            provider_id: m.provider_id,
            canonical_id: m.canonical_id,
            sharing_mode: m.sharing_mode.as_str().to_owned(),
            approval: m.approval.into(),
            info: m.info.into(),
            created_at: m.created_at,
            created_by: m.created_by,
            updated_at: m.updated_at,
            updated_by: m.updated_by,
            inherited: e.inherited,
            shadowed: e.shadowed,
            provider_enabled: e.provider_enabled,
            available_for_eval: e.available_for_eval,
        }
    }
}

/// `CreateModelRequestV1` on the wire: every Model Info field except
/// `gts_type`, plus `provider_id`, `sharing_mode`, and `approval_state`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct CreateModelRequestDto {
    pub provider_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_state: Option<String>,
    pub provider_model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_release_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplier_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_api: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_capabilities: Option<JsonValue>,
    pub context_window: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_parameters: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_parameter_override: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_extra_params: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_settings: Option<JsonValue>,
}

/// `UpdateModelRequestV1` on the wire. Nullable fields accept `null` to
/// clear.
#[api_dto(request)]
#[derive(Debug, Clone, Default)]
#[allow(clippy::option_option)]
pub struct UpdateModelRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_state: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub base_url: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub secret_ref: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub family: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub vendor: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub architecture: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub size_bytes: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub format: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub region: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub hosted_by: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_release_at: Option<Option<DateTime<Utc>>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_level: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub version: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub icon: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub multiplier_display: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_api: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_capabilities: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_parameters: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_parameter_override: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_extra_params: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_settings: Option<JsonValue>,
}

#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct AvailableModelPageDto {
    pub items: Vec<AvailableModelDto>,
    pub page_info: PageInfoDto,
}

#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ModelEntryPageDto {
    pub items: Vec<ModelEntryDto>,
    pub page_info: PageInfoDto,
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/// Mirrors `toolkit_odata::PageInfo`.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct PageInfoDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_cursor: Option<String>,
    pub limit: u64,
}

impl From<toolkit_odata::PageInfo> for PageInfoDto {
    fn from(p: toolkit_odata::PageInfo) -> Self {
        Self {
            next_cursor: p.next_cursor,
            prev_cursor: p.prev_cursor,
            limit: p.limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use model_registry_sdk::{ApprovalState, SharingMode};

    use super::*;

    fn sample_model() -> ModelV1 {
        let now = DateTime::parse_from_rfc3339("2026-10-28T00:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        ModelV1 {
            id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            provider_id: Uuid::nil(),
            canonical_id: "openai::gpt-4o".into(),
            sharing_mode: SharingMode::Shared,
            approval: ApprovalV1 {
                state: ApprovalState::Approved,
                decided_by: Uuid::nil(),
                decided_at: now,
            },
            info: ModelInfoV1 {
                gts_type: gts::GtsTypeId::new(
                    "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~",
                ),
                provider_model_id: "gpt-4o".into(),
                base_url: Some("https://api.openai.com/v1".into()),
                secret_ref: Some("openai-key".into()),
                display_name: "GPT-4o".into(),
                description: None,
                family: None,
                vendor: None,
                managed: false,
                architecture: None,
                size_bytes: None,
                format: None,
                region: None,
                hosted_by: None,
                last_release_at: None,
                reasoning_level: None,
                version: None,
                sort_order: 0,
                icon: None,
                multiplier_display: None,
                performance: model_registry_sdk::ModelPerformance::default(),
                additional_info: HashMap::new(),
                supported_api: HashSet::new(),
                capabilities: model_registry_sdk::ModelCapabilities::default(),
                disabled_capabilities: model_registry_sdk::DisabledCapabilities::none(),
                context_window: model_registry_sdk::ContextWindow::default(),
                default_parameters: model_registry_sdk::DefaultInferenceParametersV1::default(),
                allow_parameter_override: false,
                allow_extra_params: Vec::new(),
                provider_settings: serde_json::json!({ "oagw_alias": "openai" }),
            },
            created_at: now,
            created_by: Uuid::nil(),
            updated_at: now,
            updated_by: Uuid::nil(),
        }
    }

    /// The `/available-models` routes omit `provider_settings`, `base_url`,
    /// and `secret_ref`; the management routes keep them (DESIGN §4
    /// Security).
    #[test]
    fn available_model_dto_is_redacted_and_entry_dto_is_not() {
        let model = sample_model();
        let entry = serde_json::to_value(ModelEntryDto::from(ModelEntryV1 {
            model: model.clone(),
            inherited: false,
            shadowed: false,
            provider_enabled: true,
            available_for_eval: true,
        }))
        .expect("serializes");
        let available =
            serde_json::to_value(AvailableModelDto::from(AvailableModelV1::from(model)))
                .expect("serializes");

        for key in ["base_url", "secret_ref", "provider_settings"] {
            assert!(entry["info"].get(key).is_some(), "{key}");
            assert!(available["info"].get(key).is_none(), "{key}");
        }
        assert_eq!(available["info"]["sort_order"], 0);
        assert_eq!(available["canonical_id"], entry["canonical_id"]);
        assert_eq!(
            available["info"].as_object().map(serde_json::Map::len),
            entry["info"].as_object().map(|o| o.len() - 3)
        );
    }
}
