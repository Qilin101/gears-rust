//! `OData` filter / order surface for the `model-registry` listing endpoints.
//!
//! Defines filter-field enums and mappers for both the `models` and
//! `providers` listing endpoints. Each filter field maps to a **real
//! database column** — the toolkit `OData` layer (`toolkit-db` / `sea_orm_filter`)
//! does not support JSONB-path filtering or joins.
//!
//! Non-allowlisted fields are rejected via [`FilterField::from_name`] (returns
//! `None`), which the `OData` parser surfaces as an unknown-field validation
//! error.

use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};
use toolkit_odata::filter::{FieldKind, FilterField};

use super::entity::{model, provider};

// ===========================================================================
// Model filter fields
// ===========================================================================

/// `OData` filter / order field enum for `GET /model-registry/v1/models`.
///
/// Every field maps to a real `models` column (including the denormalized
/// filterable columns promoted from `info` JSONB and capability flags, plus
/// the denormalized `approval_status`).
///
/// Non-allowlisted fields (`provider_settings.*`, `default_parameters.*`,
/// `info.additional_info.*`, per-MIME array fields) are rejected at the
/// parser level — [`FilterField::from_name`] returns `None` for them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ModelFilterField {
    CanonicalId,
    LifecycleStatus,
    ApprovalStatus,
    GtsType,
    SupportedApi,
    ProviderModelId,
    Vendor,
    Family,
    Managed,
    Architecture,
    Format,
    Vision,
    FunctionCalling,
    Streaming,
    ReasoningEffort,
}

impl FilterField for ModelFilterField {
    const FIELDS: &'static [Self] = &[
        Self::CanonicalId,
        Self::LifecycleStatus,
        Self::ApprovalStatus,
        Self::GtsType,
        Self::SupportedApi,
        Self::ProviderModelId,
        Self::Vendor,
        Self::Family,
        Self::Managed,
        Self::Architecture,
        Self::Format,
        Self::Vision,
        Self::FunctionCalling,
        Self::Streaming,
        Self::ReasoningEffort,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::CanonicalId => "canonical_id",
            Self::LifecycleStatus => "lifecycle_status",
            Self::ApprovalStatus => "approval_status",
            Self::GtsType => "gts_type",
            Self::SupportedApi => "supported_api",
            Self::ProviderModelId => "provider_model_id",
            Self::Vendor => "vendor",
            Self::Family => "family",
            Self::Managed => "managed",
            Self::Architecture => "architecture",
            Self::Format => "format",
            Self::Vision => "vision",
            Self::FunctionCalling => "function_calling",
            Self::Streaming => "streaming",
            Self::ReasoningEffort => "reasoning_effort",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::CanonicalId
            | Self::LifecycleStatus
            | Self::ApprovalStatus
            | Self::GtsType
            | Self::SupportedApi
            | Self::ProviderModelId
            | Self::Vendor
            | Self::Family
            | Self::Architecture
            | Self::Format => FieldKind::String,
            Self::Managed
            | Self::Vision
            | Self::FunctionCalling
            | Self::Streaming
            | Self::ReasoningEffort => FieldKind::Bool,
        }
    }
}

/// Maps [`ModelFilterField`] to `models` columns and extracts cursor values.
pub struct ModelODataMapper;

impl FieldToColumn<ModelFilterField> for ModelODataMapper {
    type Column = model::Column;

    fn map_field(field: ModelFilterField) -> model::Column {
        match field {
            ModelFilterField::CanonicalId => model::Column::CanonicalId,
            ModelFilterField::LifecycleStatus => model::Column::LifecycleStatus,
            ModelFilterField::ApprovalStatus => model::Column::ApprovalStatus,
            ModelFilterField::GtsType => model::Column::GtsType,
            ModelFilterField::SupportedApi => model::Column::SupportedApi,
            ModelFilterField::ProviderModelId => model::Column::ProviderModelId,
            ModelFilterField::Vendor => model::Column::Vendor,
            ModelFilterField::Family => model::Column::Family,
            ModelFilterField::Managed => model::Column::Managed,
            ModelFilterField::Architecture => model::Column::Architecture,
            ModelFilterField::Format => model::Column::Format,
            ModelFilterField::Vision => model::Column::CapVision,
            ModelFilterField::FunctionCalling => model::Column::CapFunctionCalling,
            ModelFilterField::Streaming => model::Column::CapStreaming,
            ModelFilterField::ReasoningEffort => model::Column::CapReasoningEffort,
        }
    }
}

impl ODataFieldMapping<ModelFilterField> for ModelODataMapper {
    type Entity = model::Entity;

    fn extract_cursor_value(
        m: &<Self::Entity as sea_orm::EntityTrait>::Model,
        field: ModelFilterField,
    ) -> sea_orm::Value {
        match field {
            ModelFilterField::CanonicalId => {
                sea_orm::Value::String(Some(Box::new(m.canonical_id.clone())))
            }
            ModelFilterField::LifecycleStatus => {
                sea_orm::Value::String(Some(Box::new(m.lifecycle_status.clone())))
            }
            ModelFilterField::ApprovalStatus => {
                sea_orm::Value::String(Some(Box::new(m.approval_status.clone())))
            }
            ModelFilterField::GtsType => {
                sea_orm::Value::String(m.gts_type.as_ref().map(|s| Box::new(s.clone())))
            }
            ModelFilterField::SupportedApi => {
                sea_orm::Value::String(m.supported_api.as_ref().map(|s| Box::new(s.clone())))
            }
            ModelFilterField::ProviderModelId => {
                sea_orm::Value::String(m.provider_model_id.as_ref().map(|s| Box::new(s.clone())))
            }
            ModelFilterField::Vendor => {
                sea_orm::Value::String(m.vendor.as_ref().map(|s| Box::new(s.clone())))
            }
            ModelFilterField::Family => {
                sea_orm::Value::String(m.family.as_ref().map(|s| Box::new(s.clone())))
            }
            ModelFilterField::Managed => sea_orm::Value::Bool(Some(m.managed)),
            ModelFilterField::Architecture => {
                sea_orm::Value::String(m.architecture.as_ref().map(|s| Box::new(s.clone())))
            }
            ModelFilterField::Format => {
                sea_orm::Value::String(m.format.as_ref().map(|s| Box::new(s.clone())))
            }
            ModelFilterField::Vision => sea_orm::Value::Bool(Some(m.cap_vision)),
            ModelFilterField::FunctionCalling => sea_orm::Value::Bool(Some(m.cap_function_calling)),
            ModelFilterField::Streaming => sea_orm::Value::Bool(Some(m.cap_streaming)),
            ModelFilterField::ReasoningEffort => sea_orm::Value::Bool(Some(m.cap_reasoning_effort)),
        }
    }
}

// ===========================================================================
// Provider filter fields
// ===========================================================================

/// `OData` filter / order field enum for `GET /model-registry/v1/providers`.
///
/// Every field maps to a real `providers` column.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ProviderFilterField {
    Slug,
    Name,
    Status,
    GtsType,
    Managed,
    DiscoveryEnabled,
}

impl FilterField for ProviderFilterField {
    const FIELDS: &'static [Self] = &[
        Self::Slug,
        Self::Name,
        Self::Status,
        Self::GtsType,
        Self::Managed,
        Self::DiscoveryEnabled,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::Slug => "slug",
            Self::Name => "name",
            Self::Status => "status",
            Self::GtsType => "gts_type",
            Self::Managed => "managed",
            Self::DiscoveryEnabled => "discovery_enabled",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Slug | Self::Name | Self::Status | Self::GtsType => FieldKind::String,
            Self::Managed | Self::DiscoveryEnabled => FieldKind::Bool,
        }
    }
}

/// Maps [`ProviderFilterField`] to `providers` columns and extracts cursor values.
pub struct ProviderODataMapper;

impl FieldToColumn<ProviderFilterField> for ProviderODataMapper {
    type Column = provider::Column;

    fn map_field(field: ProviderFilterField) -> provider::Column {
        match field {
            ProviderFilterField::Slug => provider::Column::Slug,
            ProviderFilterField::Name => provider::Column::Name,
            ProviderFilterField::Status => provider::Column::Status,
            ProviderFilterField::GtsType => provider::Column::GtsType,
            ProviderFilterField::Managed => provider::Column::Managed,
            ProviderFilterField::DiscoveryEnabled => provider::Column::DiscoveryEnabled,
        }
    }
}

impl ODataFieldMapping<ProviderFilterField> for ProviderODataMapper {
    type Entity = provider::Entity;

    fn extract_cursor_value(
        m: &<Self::Entity as sea_orm::EntityTrait>::Model,
        field: ProviderFilterField,
    ) -> sea_orm::Value {
        match field {
            ProviderFilterField::Slug => sea_orm::Value::String(Some(Box::new(m.slug.clone()))),
            ProviderFilterField::Name => sea_orm::Value::String(Some(Box::new(m.name.clone()))),
            ProviderFilterField::Status => sea_orm::Value::String(Some(Box::new(m.status.clone()))),
            ProviderFilterField::GtsType => {
                sea_orm::Value::String(Some(Box::new(m.gts_type.clone())))
            }
            ProviderFilterField::Managed => sea_orm::Value::Bool(Some(m.managed)),
            ProviderFilterField::DiscoveryEnabled => {
                sea_orm::Value::Bool(Some(m.discovery_enabled))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    // =======================================================================
    // Model filter field tests
    // =======================================================================

    /// Returns all filterable field names as expected by `OData` API consumers.
    fn expected_model_field_names() -> Vec<&'static str> {
        vec![
            "canonical_id",
            "lifecycle_status",
            "approval_status",
            "gts_type",
            "supported_api",
            "provider_model_id",
            "vendor",
            "family",
            "managed",
            "architecture",
            "format",
            "vision",
            "function_calling",
            "streaming",
            "reasoning_effort",
        ]
    }

    #[test]
    fn model_field_names_match_expected() {
        let expected = expected_model_field_names();
        let actual: Vec<&str> = ModelFilterField::FIELDS
            .iter()
            .map(FilterField::name)
            .collect();
        assert_eq!(
            actual, expected,
            "model filter field names must match the API contract"
        );
    }

    #[test]
    fn model_field_kinds_are_correct() {
        for field in ModelFilterField::FIELDS {
            match field {
                ModelFilterField::Managed
                | ModelFilterField::Vision
                | ModelFilterField::FunctionCalling
                | ModelFilterField::Streaming
                | ModelFilterField::ReasoningEffort => {
                    assert_eq!(
                        field.kind(),
                        FieldKind::Bool,
                        "field {field:?} should be Bool"
                    );
                }
                _ => {
                    assert_eq!(
                        field.kind(),
                        FieldKind::String,
                        "field {field:?} should be String"
                    );
                }
            }
        }
    }

    #[test]
    fn model_field_map_field_returns_correct_column() {
        use sea_orm::Iden;
        fn assert_col(f: ModelFilterField, expected: &str) {
            let actual = ModelODataMapper::map_field(f);
            assert_eq!(
                actual.to_string(),
                expected,
                "field {f:?} should map to column {expected}"
            );
        }

        assert_col(ModelFilterField::CanonicalId, "canonical_id");
        assert_col(ModelFilterField::LifecycleStatus, "lifecycle_status");
        assert_col(ModelFilterField::ApprovalStatus, "approval_status");
        assert_col(ModelFilterField::GtsType, "gts_type");
        assert_col(ModelFilterField::SupportedApi, "supported_api");
        assert_col(ModelFilterField::ProviderModelId, "provider_model_id");
        assert_col(ModelFilterField::Vendor, "vendor");
        assert_col(ModelFilterField::Family, "family");
        assert_col(ModelFilterField::Managed, "managed");
        assert_col(ModelFilterField::Architecture, "architecture");
        assert_col(ModelFilterField::Format, "format");
        assert_col(ModelFilterField::Vision, "cap_vision");
        assert_col(ModelFilterField::FunctionCalling, "cap_function_calling");
        assert_col(ModelFilterField::Streaming, "cap_streaming");
        assert_col(ModelFilterField::ReasoningEffort, "cap_reasoning_effort");
    }

    #[test]
    fn model_from_name_resolves_exact_match() {
        assert_eq!(
            ModelFilterField::from_name("lifecycle_status"),
            Some(ModelFilterField::LifecycleStatus)
        );
        assert_eq!(
            ModelFilterField::from_name("canonical_id"),
            Some(ModelFilterField::CanonicalId)
        );
        assert_eq!(
            ModelFilterField::from_name("approval_status"),
            Some(ModelFilterField::ApprovalStatus)
        );
        assert_eq!(
            ModelFilterField::from_name("vision"),
            Some(ModelFilterField::Vision)
        );
        assert_eq!(
            ModelFilterField::from_name("reasoning_effort"),
            Some(ModelFilterField::ReasoningEffort)
        );
    }

    #[test]
    fn model_from_name_is_case_insensitive() {
        assert_eq!(
            ModelFilterField::from_name("LIFECYCLE_STATUS"),
            Some(ModelFilterField::LifecycleStatus)
        );
        assert_eq!(
            ModelFilterField::from_name("Gts_Type"),
            Some(ModelFilterField::GtsType)
        );
    }

    #[test]
    fn model_rejects_non_allowlisted_fields() {
        // These fields should NOT be filterable per DESIGN §3.3
        assert_eq!(ModelFilterField::from_name("provider_settings"), None);
        assert_eq!(ModelFilterField::from_name("default_parameters"), None);
        assert_eq!(ModelFilterField::from_name("additional_info"), None);
        assert_eq!(ModelFilterField::from_name("context_window"), None);
        assert_eq!(ModelFilterField::from_name("cost"), None);
        assert_eq!(ModelFilterField::from_name("nonexistent_field"), None);
    }

    #[test]
    fn model_rejects_jsonb_path_style_fields() {
        // `OData` layer maps to real columns, not JSONB paths
        assert_eq!(ModelFilterField::from_name("info.gts_type"), None);
        assert_eq!(ModelFilterField::from_name("info.supported_api"), None);
        assert_eq!(
            ModelFilterField::from_name("info.capabilities.vision"),
            None
        );
        assert_eq!(ModelFilterField::from_name("info.vendor"), None);
    }

    #[test]
    fn model_extract_cursor_value_round_trip() {
        let now = Utc::now();
        let m = model::Model {
            id: Uuid::nil(),
            provider_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            canonical_id: "test::model".to_owned(),
            lifecycle_status: "production".to_owned(),
            deprecated_at: None,
            provider_settings: None,
            created_at: now,
            updated_at: now,
            display_name: String::new(),
            description: None,
            size_bytes: None,
            region: None,
            hosted_by: None,
            last_release_at: None,
            reasoning_level: None,
            version: None,
            sort_order: None,
            icon: None,
            multiplier_display: None,
            perf_response_latency_ms: None,
            perf_tokens_per_second: None,
            ctx_max_input_tokens: 0,
            ctx_max_output_tokens: None,
            ctx_output_vector_size: None,
            allow_parameter_override: false,
            capabilities_full: None,
            default_parameters: None,
            additional_info: None,
            disabled_capabilities_full: None,
            allow_extra_params: None,
            gts_type: Some("gts.cf.genai.model.info.v1~".to_owned()),
            vendor: Some("OpenAI".to_owned()),
            family: Some("gpt-4".to_owned()),
            managed: false,
            architecture: Some("transformer".to_owned()),
            format: Some("api-only".to_owned()),
            provider_model_id: Some("gpt-4o".to_owned()),
            supported_api: Some("completion".to_owned()),
            approval_status: "approved".to_owned(),
            cap_vision: true,
            cap_function_calling: true,
            cap_streaming: true,
            cap_reasoning_effort: false,
        };

        // String fields
        assert_eq!(
            ModelODataMapper::extract_cursor_value(&m, ModelFilterField::LifecycleStatus),
            sea_orm::Value::String(Some(Box::new("production".to_owned())))
        );
        assert_eq!(
            ModelODataMapper::extract_cursor_value(&m, ModelFilterField::ApprovalStatus),
            sea_orm::Value::String(Some(Box::new("approved".to_owned())))
        );
        assert_eq!(
            ModelODataMapper::extract_cursor_value(&m, ModelFilterField::GtsType),
            sea_orm::Value::String(Some(Box::new("gts.cf.genai.model.info.v1~".to_owned())))
        );

        // Bool fields
        assert_eq!(
            ModelODataMapper::extract_cursor_value(&m, ModelFilterField::Vision),
            sea_orm::Value::Bool(Some(true))
        );
        assert_eq!(
            ModelODataMapper::extract_cursor_value(&m, ModelFilterField::FunctionCalling),
            sea_orm::Value::Bool(Some(true))
        );
        assert_eq!(
            ModelODataMapper::extract_cursor_value(&m, ModelFilterField::ReasoningEffort),
            sea_orm::Value::Bool(Some(false))
        );

        // Optional string fields (None case)
        let empty = model::Model {
            id: Uuid::nil(),
            provider_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            canonical_id: "test::empty".to_owned(),
            lifecycle_status: "production".to_owned(),
            deprecated_at: None,
            provider_settings: None,
            created_at: now,
            updated_at: now,
            display_name: String::new(),
            description: None,
            size_bytes: None,
            region: None,
            hosted_by: None,
            last_release_at: None,
            reasoning_level: None,
            version: None,
            sort_order: None,
            icon: None,
            multiplier_display: None,
            perf_response_latency_ms: None,
            perf_tokens_per_second: None,
            ctx_max_input_tokens: 0,
            ctx_max_output_tokens: None,
            ctx_output_vector_size: None,
            allow_parameter_override: false,
            capabilities_full: None,
            default_parameters: None,
            additional_info: None,
            disabled_capabilities_full: None,
            allow_extra_params: None,
            gts_type: None,
            vendor: None,
            family: None,
            managed: false,
            architecture: None,
            format: None,
            provider_model_id: None,
            supported_api: None,
            approval_status: "pending".to_owned(),
            cap_vision: false,
            cap_function_calling: false,
            cap_streaming: false,
            cap_reasoning_effort: false,
        };
        assert_eq!(
            ModelODataMapper::extract_cursor_value(&empty, ModelFilterField::GtsType),
            sea_orm::Value::String(None)
        );
        assert_eq!(
            ModelODataMapper::extract_cursor_value(&empty, ModelFilterField::Vendor),
            sea_orm::Value::String(None)
        );
    }

    // =======================================================================
    // Provider filter field tests
    // =======================================================================

    #[test]
    fn provider_field_names_match_expected() {
        let expected: Vec<&str> = vec![
            "slug",
            "name",
            "status",
            "gts_type",
            "managed",
            "discovery_enabled",
        ];
        let actual: Vec<&str> = ProviderFilterField::FIELDS
            .iter()
            .map(FilterField::name)
            .collect();
        assert_eq!(
            actual, expected,
            "provider filter field names must match the API contract"
        );
    }

    #[test]
    fn provider_field_kinds_are_correct() {
        for field in ProviderFilterField::FIELDS {
            match field {
                ProviderFilterField::Managed | ProviderFilterField::DiscoveryEnabled => {
                    assert_eq!(
                        field.kind(),
                        FieldKind::Bool,
                        "field {field:?} should be Bool"
                    );
                }
                _ => {
                    assert_eq!(
                        field.kind(),
                        FieldKind::String,
                        "field {field:?} should be String"
                    );
                }
            }
        }
    }

    #[test]
    fn provider_field_map_field_returns_correct_column() {
        use sea_orm::Iden;
        fn assert_col(f: ProviderFilterField, expected: &str) {
            let actual = ProviderODataMapper::map_field(f);
            assert_eq!(
                actual.to_string(),
                expected,
                "field {f:?} should map to column {expected}"
            );
        }

        assert_col(ProviderFilterField::Slug, "slug");
        assert_col(ProviderFilterField::Name, "name");
        assert_col(ProviderFilterField::Status, "status");
        assert_col(ProviderFilterField::GtsType, "gts_type");
        assert_col(ProviderFilterField::Managed, "managed");
        assert_col(ProviderFilterField::DiscoveryEnabled, "discovery_enabled");
    }

    #[test]
    fn provider_from_name_resolves() {
        assert_eq!(
            ProviderFilterField::from_name("slug"),
            Some(ProviderFilterField::Slug)
        );
        assert_eq!(
            ProviderFilterField::from_name("discovery_enabled"),
            Some(ProviderFilterField::DiscoveryEnabled)
        );
    }

    #[test]
    fn provider_rejects_non_allowlisted_fields() {
        assert_eq!(ProviderFilterField::from_name("metadata"), None);
        assert_eq!(
            ProviderFilterField::from_name("discovery_interval_seconds"),
            None
        );
        assert_eq!(ProviderFilterField::from_name("unknown_field"), None);
    }

    #[test]
    fn provider_extract_cursor_value_round_trip() {
        let now = Utc::now();
        let m = provider::Model {
            id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            slug: "openai".to_owned(),
            name: "OpenAI".to_owned(),
            gts_type: "gts.cf.genai._.openai.v1~".to_owned(),
            status: "active".to_owned(),
            managed: true,
            metadata: None,
            discovery_enabled: false,
            discovery_interval_seconds: None,
            created_at: now,
            updated_at: now,
        };

        assert_eq!(
            ProviderODataMapper::extract_cursor_value(&m, ProviderFilterField::Slug),
            sea_orm::Value::String(Some(Box::new("openai".to_owned())))
        );
        assert_eq!(
            ProviderODataMapper::extract_cursor_value(&m, ProviderFilterField::Managed),
            sea_orm::Value::Bool(Some(true))
        );
        assert_eq!(
            ProviderODataMapper::extract_cursor_value(&m, ProviderFilterField::DiscoveryEnabled),
            sea_orm::Value::Bool(Some(false))
        );
    }
}
