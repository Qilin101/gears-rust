//! Binds the SDK's `models` `OData` filter allowlist to real `SeaORM` columns.
//!
//! The filterable wire surface itself — [`ModelFilterField`] — is public
//! contract and lives in [`model_registry_sdk::odata`]. What stays here is the
//! column binding, which names storage types the SDK does not expose and which
//! no derive can infer (`vision` → `Column::CapVision`), plus the cursor-value
//! extraction that keyset pagination needs. The provider-side binding lives in
//! [`super::provider_odata_mapper`].
//!
//! Every field maps to exactly one real column: the toolkit `OData` layer
//! (`toolkit-db` / `sea_orm_filter`) supports neither JSONB-path filtering nor
//! joins. That is why the filterable `ModelInfoV1` fields are promoted to
//! scalar columns rather than filtered inside a JSONB blob. Non-allowlisted
//! fields never reach this module — they are rejected by
//! [`toolkit_odata::filter::FilterField::from_name`] (returns `None`), which the
//! `OData` parser surfaces as an unknown-field validation error.

use model_registry_sdk::odata::ModelFilterField;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};

use super::entity::model;

// ===========================================================================
// Model filter fields
// ===========================================================================

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    // =======================================================================
    // Model column binding
    // =======================================================================

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
}
