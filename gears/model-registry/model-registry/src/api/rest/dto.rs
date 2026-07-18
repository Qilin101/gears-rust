//! REST DTOs for the Model Registry gear.
//!
//! These are the only types that derive `utoipa::ToSchema` — SDK domain types
//! in `model_registry_sdk::models` stay transport-agnostic. Each DTO mirrors
//! the wire shape defined by the SDK trait contract.
//!
//! ## Construction from SDK types
//!
//! SDK types (`ProviderV1`, `ModelV1`) are `#[non_exhaustive]` — they cannot
//! be constructed with struct literals outside the SDK crate. For DTO
//! conversions we use `serde_json` roundtripping:
//!
//! ```rust,ignore
//! let dto: ProviderDto = serde_json::from_value(serde_json::to_value(provider)?)?;
//! ```
//!
//! This is safe because the DTO fields are a strict subset of the SDK fields,
//! and both sides derive `serde::Serialize` + `serde::Deserialize`.

use serde_json::Value as JsonValue;
use toolkit_macros::api_dto;
use uuid::Uuid;

/// Serde deserialization helper for `Option<Option<T>>` PATCH fields.
///
/// Distinguishes:
/// - **Absent** → `None` (field not provided — leave unchanged)
/// - **`null`** → `Some(None)` (explicitly clear a nullable field)
/// - **value** → `Some(Some(v))` (set to value)
///
/// `#[serde(default)]` on its own cannot distinguish absent from `null` for
/// `Option<Option<T>>`, so this helper replaces `default` on nullable fields
/// while keeping the absent→`None` fallback.
#[allow(clippy::option_option)]
pub(crate) mod nullable_opt {
    use serde::Deserialize;

    /// Used via `#[serde(skip_serializing_if = "nullable_opt::is_absent")]`.
    #[allow(dead_code, clippy::ref_option)]
    #[must_use]
    pub fn is_absent<T>(v: &Option<Option<T>>) -> bool {
        v.is_none()
    }

    pub fn deserialize<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        match Option::<T>::deserialize(d) {
            Ok(Some(v)) => Ok(Some(Some(v))),
            Ok(None) => Ok(Some(None)),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider DTOs
// ---------------------------------------------------------------------------

/// Wire projection of [`ProviderV1`].
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct ProviderDto {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub gts_type: String,
    pub status: String,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
    pub discovery_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_interval_seconds: Option<u32>,
    pub created_at: String,
    pub updated_at: String,
}

/// Request body for `POST /model-registry/v1/providers`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct CreateProviderRequestDto {
    pub slug: String,
    pub name: String,
    pub gts_type: String,
    #[serde(default)]
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
    #[serde(default)]
    pub discovery_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_interval_seconds: Option<u32>,
}

/// Request body for `PATCH /model-registry/v1/providers/{id}`.
#[api_dto(request)]
#[derive(Debug, Clone, Default)]
#[allow(clippy::option_option)]
pub struct UpdateProviderRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub metadata: Option<Option<JsonValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_enabled: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub discovery_interval_seconds: Option<Option<u32>>,
}

/// Cursor-paginated list envelope for `GET /model-registry/v1/providers`.
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct ProviderListDto {
    pub items: Vec<ProviderDto>,
    pub page_info: PageInfoDto,
}

// ---------------------------------------------------------------------------
// Model DTOs
// ---------------------------------------------------------------------------

/// Wire projection of [`ModelV1`].
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct ModelDto {
    pub id: Uuid,
    pub canonical_id: String,
    pub lifecycle_status: String,
    pub approval_status: String,
    pub info: JsonValue,
}

/// Request body for `POST /model-registry/v1/models`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct CreateModelRequestDto {
    pub provider_slug: String,
    pub lifecycle_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_status: Option<String>,
    pub info: JsonValue,
}

/// Request body for `PATCH /model-registry/v1/models/{canonical_id}`.
#[api_dto(request)]
#[derive(Debug, Clone, Default)]
#[allow(clippy::option_option)]
pub struct UpdateModelRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub family: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub vendor: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub architecture: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub size_bytes: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub format: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub region: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub hosted_by: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub reasoning_level: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub version: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub sort_order: Option<Option<i32>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub icon: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "nullable_opt::is_absent"
    )]
    pub multiplier_display: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<JsonValue>,
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

/// Cursor-paginated list envelope for `GET /model-registry/v1/models`.
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct ModelListDto {
    pub items: Vec<ModelDto>,
    pub page_info: PageInfoDto,
}

// ---------------------------------------------------------------------------
// Shared DTOs
// ---------------------------------------------------------------------------

/// Pagination metadata, mirroring [`toolkit_odata::PageInfo`].
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct PageInfoDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_cursor: Option<String>,
    pub limit: u32,
}
