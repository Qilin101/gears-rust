//! REST DTOs for the Model Registry gear.
//!
//! These are the only types that derive `utoipa::ToSchema` — SDK domain types
//! in `model_registry_sdk::models` stay transport-agnostic. Each DTO mirrors
//! the wire shape defined by the SDK trait contract.
//!
//! ## Construction from SDK types
//!
//! SDK types (`ProviderV1`, `ModelV1`) are converted to their DTO counterparts
//! via `impl From<…> for …Dto` (see [`From<ProviderV1> for ProviderDto`] and
//! [`From<ModelV1> for ModelDto`]). Handlers call `.into()` (or
//! `Type::from(value)`) on the SDK return value to obtain the DTO — no
//! `from_value(to_value(...))` round-tripping is involved. The `ModelDto::info`
//! field does call `serde_json::to_value` once (see [`model_info_to_json`]) to
//! re-serialize the SDK's `ModelInfoV1<P>` into the wire-shape `JsonValue`.

use model_registry_sdk::models::{ModelInfoV1, ModelV1, ProviderV1};
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
/// while keeping the absent→`None` fallback. Pairs with
/// `#[serde(skip_serializing_if = "Option::is_none")]` so only the
/// "explicit null" and "value" cases reach the wire.
#[allow(clippy::option_option)]
pub(crate) mod nullable_opt {
    use serde::Deserialize;

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
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<Option<JsonValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_enabled: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
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

impl From<ProviderV1> for ProviderDto {
    fn from(source: ProviderV1) -> Self {
        Self {
            id: source.id,
            slug: source.slug,
            name: source.name,
            gts_type: source.gts_type.to_string(),
            status: source.status.as_str().to_owned(),
            managed: source.managed,
            metadata: source.metadata,
            discovery_enabled: source.discovery_enabled,
            discovery_interval_seconds: source.discovery_interval_seconds,
            created_at: source.created_at.to_rfc3339(),
            updated_at: source.updated_at.to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// Model DTOs
// ---------------------------------------------------------------------------

/// Wire projection of [`ModelV1`].
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct ModelDto {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub canonical_id: String,
    pub lifecycle_status: String,
    pub approval_status: String,
    pub info: JsonValue,
}

/// Re-serialize `ModelInfoV1<P>` into the `JsonValue` shape carried by
/// `ModelDto::info`.
///
/// The `Serialize` impl on `ModelInfoV1` is generated by the
/// `#[struct_to_gts_schema]` macro on the SDK side, so this is a pure
/// passthrough — the only failure mode is a serialization bug (programming
/// error). Falling back to `JsonValue::Null` keeps the `From` impl total
/// without surfacing an error path that cannot actually occur at runtime.
#[must_use]
pub fn model_info_to_json<P>(info: &ModelInfoV1<P>) -> JsonValue
where
    P: gts::GtsSchema + gts::GtsSerialize,
{
    serde_json::to_value(info).unwrap_or(JsonValue::Null)
}

impl<P> From<ModelV1<P>> for ModelDto
where
    P: gts::GtsSchema + gts::GtsSerialize,
{
    fn from(source: ModelV1<P>) -> Self {
        Self {
            id: source.id,
            provider_id: source.provider_id,
            canonical_id: source.canonical_id,
            lifecycle_status: source.lifecycle_status.as_str().to_owned(),
            approval_status: source.approval_status.as_str().to_owned(),
            info: model_info_to_json(&source.info),
        }
    }
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
    pub reasoning_level: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub version: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub sort_order: Option<Option<i32>>,
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
