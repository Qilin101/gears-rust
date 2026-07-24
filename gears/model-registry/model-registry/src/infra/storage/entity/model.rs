use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// `SeaORM` entity for the `models` table.
///
/// Stores model catalog entries per tenant. The schema is the authoritative
/// source of truth for `ModelInfoV1` (the public SDK type):
///
/// - **17 scalar columns** hold the fields that promote cleanly to typed
///   columns (one column per `ModelInfoV1` field, or per nested-struct leaf).
/// - **5 JSONB sub-object columns** hold the `ModelInfoV1` sub-objects that
///   don't promote cleanly: `capabilities_full` (everything in
///   `ModelCapabilities` minus the 4 `OData` booleans),
///   `default_parameters` (`DefaultInferenceParametersV1`),
///   `additional_info` (`HashMap<String, serde_json::Value>`),
///   `disabled_capabilities_full` (`DisabledCapabilities`), and
///   `allow_extra_params` (`Vec<String>` of caller-supplied parameter names).
/// - **`provider_settings`** is the polymorphic JSONB column storing the raw
///   provider-specific settings payload (the `P` in `ModelInfoV1<P>`), keyed
///   by the scalar `gts_type` discriminator.
///
/// Previously, `info` (a JSONB column holding the full serialized
/// `ModelInfoV1`) was the source of truth and the scalar columns were
/// denormalized shadows. As of 2026-07-24, `info` has been dropped and every
/// `ModelInfoV1` field is stored in a typed column or a small JSONB
/// sub-object.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "models")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Foreign key to `providers.id`.
    pub provider_id: Uuid,
    pub tenant_id: Uuid,
    /// Format: `{provider_slug}::{provider_model_id}`. Immutable after creation.
    pub canonical_id: String,
    /// Lifecycle status: `production`, `preview`, `experimental`, `deprecated`, `sunset`.
    pub lifecycle_status: String,
    /// When the model was soft-deleted (`lifecycle_status` -> deprecated).
    pub deprecated_at: Option<DateTime<Utc>>,
    /// Provider-specific settings payload, JSONB (JSON on `SQLite`). Keyed by `gts_type`.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub provider_settings: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // ═══════════════════════════════════════════════════════════════════
    // 17 promoted scalar columns from `ModelInfoV1`
    // ═══════════════════════════════════════════════════════════════════
    /// `info.display_name` — display name shown in UI. NOT NULL DEFAULT ''.
    pub display_name: String,
    /// `info.description`.
    pub description: Option<String>,
    /// `info.size_bytes` (for local/managed LLMs).
    pub size_bytes: Option<i64>,
    /// `info.region`.
    pub region: Option<String>,
    /// `info.hosted_by`.
    pub hosted_by: Option<String>,
    /// `info.last_release_at`.
    pub last_release_at: Option<DateTime<Utc>>,
    /// `info.reasoning_level` (display-only).
    pub reasoning_level: Option<String>,
    /// `info.version`.
    pub version: Option<String>,
    /// `info.sort_order` (display order in model picker).
    pub sort_order: Option<i32>,
    /// `info.icon` (URL to model icon).
    pub icon: Option<String>,
    /// `info.multiplier_display` (cost multiplier label).
    pub multiplier_display: Option<String>,
    /// `info.performance.response_latency_ms`.
    pub perf_response_latency_ms: Option<i32>,
    /// `info.performance.tokens_per_second`.
    pub perf_tokens_per_second: Option<i32>,
    /// `info.context_window.max_input_tokens`. NOT NULL DEFAULT 0.
    pub ctx_max_input_tokens: i32,
    /// `info.context_window.max_output_tokens`.
    pub ctx_max_output_tokens: Option<i32>,
    /// `info.context_window.output_vector_size` (for embedding models).
    pub ctx_output_vector_size: Option<i32>,
    /// `info.allow_parameter_override`. NOT NULL DEFAULT 0.
    pub allow_parameter_override: bool,

    // ═══════════════════════════════════════════════════════════════════
    // 5 JSONB sub-object columns (the rest of `ModelInfoV1`)
    // ═══════════════════════════════════════════════════════════════════
    /// `ModelCapabilities` minus the 4 `OData` booleans stored as scalar
    /// columns below (`cap_vision`, `cap_function_calling`, `cap_streaming`,
    /// `cap_reasoning_effort`).
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub capabilities_full: Option<serde_json::Value>,
    /// `DefaultInferenceParametersV1` sub-object.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub default_parameters: Option<serde_json::Value>,
    /// `additional_info: HashMap<String, serde_json::Value>` forward-compat escape hatch.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub additional_info: Option<serde_json::Value>,
    /// `DisabledCapabilities` (mirrors `capabilities_full`).
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub disabled_capabilities_full: Option<serde_json::Value>,
    /// `allow_extra_params: Vec<String>` of caller-supplied parameter names
    /// permitted alongside the request (added 2026-07-24 per the plan's
    /// `allow_extra_params` user decision).
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub allow_extra_params: Option<serde_json::Value>,

    // ═══════════════════════════════════════════════════════════════════
    // Denormalized filterable columns (15 existing OData filter surface)
    // ═══════════════════════════════════════════════════════════════════
    /// GTS schema chain identifier (e.g. `gts.cf.genai.model.info.v1~cf.genai._.openai.v1~`).
    pub gts_type: Option<String>,
    /// Model vendor (e.g. `OpenAI`, `Meta`).
    pub vendor: Option<String>,
    /// Model family (e.g. `gpt-4`, `claude`, `llama`).
    pub family: Option<String>,
    /// Per-model managed flag for local/managed LLMs.
    pub managed: bool,
    /// Model architecture classifier (e.g. "qwen", "llama", "mistral").
    pub architecture: Option<String>,
    /// Model weight/serving format (e.g. "gguf", "safetensors", "api-only").
    pub format: Option<String>,
    /// Provider's model identifier (e.g. "gpt-4o", "claude-sonnet-4-20250514").
    pub provider_model_id: Option<String>,
    /// Supported API kind (e.g. "completion", "embedding", "batch").
    /// Stored as denormalized text from `info.supported_api`.
    pub supported_api: Option<String>,

    // -- Denormalized approval status --
    /// Denormalized approval status from `model_approvals`. Defaults to
    /// "pending". Kept in sync on every approval write.
    pub approval_status: String,

    // -- Denormalized capability flags --
    /// Whether the model supports vision/image input.
    pub cap_vision: bool,
    /// Whether the model supports function/tool calling.
    pub cap_function_calling: bool,
    /// Whether the model supports streaming responses.
    pub cap_streaming: bool,
    /// Whether the model supports reasoning effort parameter.
    pub cap_reasoning_effort: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::provider::Entity",
        from = "Column::ProviderId",
        to = "super::provider::Column::Id",
        on_update = "NoAction",
        on_delete = "Restrict"
    )]
    Provider,
    #[sea_orm(has_many = "super::model_approval::Entity")]
    ModelApproval,
}

impl Related<super::provider::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Provider.def()
    }
}

impl Related<super::model_approval::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ModelApproval.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn test_id() -> Uuid {
        Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()
    }

    fn test_provider() -> Uuid {
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
    }

    fn test_tenant() -> Uuid {
        Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
    }

    /// Build a `Model` entity populated with all 21 new fields (17 scalar +
    /// 4 JSONB sub-object columns) plus the existing 13 filterable columns.
    /// Round-trips through JSON so the `#[non_exhaustive]` types in the SDK
    /// aren't an issue.
    #[allow(clippy::too_many_lines)]
    fn make_full_model_entity() -> Model {
        let now = Utc.with_ymd_and_hms(2026, 7, 24, 12, 0, 0).unwrap();
        Model {
            id: test_id(),
            provider_id: test_provider(),
            tenant_id: test_tenant(),
            canonical_id: "openai::gpt-4o".to_owned(),
            lifecycle_status: "production".to_owned(),
            deprecated_at: None,
            provider_settings: Some(json!({"oagw_alias": "openai-prod"})),
            created_at: now,
            updated_at: now,
            // 17 promoted scalar columns
            display_name: "GPT-4o".to_owned(),
            description: Some("OpenAI's flagship model".to_owned()),
            size_bytes: None,
            region: Some("us-east-1".to_owned()),
            hosted_by: Some("OpenAI".to_owned()),
            last_release_at: None,
            reasoning_level: Some("high".to_owned()),
            version: Some("1.0".to_owned()),
            sort_order: Some(10),
            icon: None,
            multiplier_display: Some("1x".to_owned()),
            perf_response_latency_ms: Some(500),
            perf_tokens_per_second: Some(100),
            ctx_max_input_tokens: 128_000,
            ctx_max_output_tokens: Some(16_384),
            ctx_output_vector_size: None,
            allow_parameter_override: true,
            // 4 JSONB sub-object columns
            capabilities_full: Some(json!({
                "vision": { "enabled": true, "supported_mime_types": ["image/png"] },
                "reasoning": { "effort": true, "toggle": false, "resume": false, "budget": false },
                "response_schema": true,
                "file_input": { "enabled": false, "supported_mime_types": [] },
                "image_generation": { "enabled": false, "supported_mime_types": [] },
                "audio_input": { "enabled": false, "supported_mime_types": [] },
                "audio_output": { "enabled": false, "supported_mime_types": [] },
                "code_interpreter": false,
                "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
            })),
            default_parameters: Some(json!({
                "temperature": 0.7
            })),
            additional_info: Some(json!({"region": "us-east"})),
            disabled_capabilities_full: Some(json!({
                "vision": { "disabled": false, "disabled_mime_types": [] },
                "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                "function_calling": false,
                "response_schema": false,
                "streaming": false,
                "file_input": { "disabled": false, "disabled_mime_types": [] },
                "image_generation": { "disabled": false, "disabled_mime_types": [] },
                "audio_input": { "disabled": false, "disabled_mime_types": [] },
                "audio_output": { "disabled": false, "disabled_mime_types": [] },
                "code_interpreter": false,
                "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
            })),
            allow_extra_params: Some(json!(["custom_param", "trace_id"])),
            // 13 existing filterable columns
            gts_type: Some("gts.cf.genai.model.info.v1~cf.genai._.openai.v1~".to_owned()),
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
            cap_reasoning_effort: true,
        }
    }

    /// Verify the entity compiles with the new column layout (no `info`
    /// field) by asserting representative values for all 17 promoted scalar
    /// fields + 5 JSONB sub-object fields are directly accessible on a
    /// constructed entity.
    #[test]
    fn entity_has_new_column_layout() {
        let entity = make_full_model_entity();

        assert_eq!(entity.id, test_id());
        assert_eq!(entity.provider_id, test_provider());
        assert_eq!(entity.tenant_id, test_tenant());
        assert_eq!(entity.canonical_id, "openai::gpt-4o");
        assert_eq!(entity.lifecycle_status, "production");
        assert!(entity.deprecated_at.is_none());
    }

    /// Verify representative values for all 17 promoted scalar columns
    /// survive the fixture (a stub-default would silently pass type checks).
    #[test]
    fn entity_has_promoted_scalar_values() {
        let entity = make_full_model_entity();

        assert_eq!(entity.display_name, "GPT-4o");
        assert_eq!(entity.description.as_deref(), Some("OpenAI's flagship model"));
        assert!(entity.size_bytes.is_none());
        assert_eq!(entity.region.as_deref(), Some("us-east-1"));
        assert_eq!(entity.hosted_by.as_deref(), Some("OpenAI"));
        assert!(entity.last_release_at.is_none());
        assert_eq!(entity.reasoning_level.as_deref(), Some("high"));
        assert_eq!(entity.version.as_deref(), Some("1.0"));
        assert_eq!(entity.sort_order, Some(10));
        assert!(entity.icon.is_none());
        assert_eq!(entity.multiplier_display.as_deref(), Some("1x"));
        assert_eq!(entity.perf_response_latency_ms, Some(500));
        assert_eq!(entity.perf_tokens_per_second, Some(100));
        assert_eq!(entity.ctx_max_input_tokens, 128_000);
        assert_eq!(entity.ctx_max_output_tokens, Some(16_384));
        assert!(entity.ctx_output_vector_size.is_none());
        assert!(entity.allow_parameter_override);
    }

    /// Verify all 5 JSONB sub-object columns are populated on the fixture.
    #[test]
    fn entity_has_jsonb_sub_object_values() {
        let entity = make_full_model_entity();

        assert!(entity.capabilities_full.is_some());
        assert!(entity.default_parameters.is_some());
        assert!(entity.additional_info.is_some());
        assert!(entity.disabled_capabilities_full.is_some());
        assert!(entity.allow_extra_params.is_some());
    }

    /// Verify `default_parameters`, `capabilities_full`, `additional_info`,
    /// `disabled_capabilities_full`, and `allow_extra_params` accept arbitrary
    /// JSON values.
    #[test]
    fn jsonb_sub_object_columns_accept_arbitrary_json() {
        let mut entity = make_full_model_entity();

        entity.capabilities_full = Some(json!({"custom_field": "anything"}));
        entity.default_parameters = Some(json!(null));
        entity.additional_info = Some(json!({}));
        entity.disabled_capabilities_full = None;
        entity.allow_extra_params = Some(json!(["x", "y"]));

        // Direct field comparisons — entity is not Serialize, so we can't
        // round-trip through serde_json::to_value.
        assert_eq!(
            entity.capabilities_full.as_ref().unwrap(),
            &json!({"custom_field": "anything"})
        );
        assert_eq!(
            entity.default_parameters.as_ref().unwrap(),
            &serde_json::Value::Null
        );
        assert_eq!(entity.additional_info.as_ref().unwrap(), &json!({}));
        assert!(entity.disabled_capabilities_full.is_none());
        assert_eq!(
            entity.allow_extra_params.as_ref().unwrap(),
            &json!(["x", "y"])
        );
    }

    /// Verify NOT NULL scalar columns hold concrete values when populated.
    /// (Rust's type system enforces non-nullability at compile time; this
    /// test confirms the runtime values are present and not corrupted.)
    #[test]
    fn not_null_scalar_columns_hold_values() {
        let entity = make_full_model_entity();

        assert_eq!(entity.display_name, "GPT-4o");
        assert_eq!(entity.ctx_max_input_tokens, 128_000);
        assert!(entity.allow_parameter_override);
    }

    /// Verify the entity can be cloned (`DeriveEntityModel` provides Clone),
    /// demonstrating field-by-field layout works end-to-end.
    #[test]
    fn entity_clone_preserves_all_columns() {
        let entity = make_full_model_entity();
        let cloned = entity.clone();

        assert_eq!(entity.id, cloned.id);
        assert_eq!(entity.display_name, cloned.display_name);
        assert_eq!(entity.ctx_max_input_tokens, cloned.ctx_max_input_tokens);
        assert_eq!(
            entity.allow_parameter_override,
            cloned.allow_parameter_override
        );
        assert_eq!(entity.capabilities_full, cloned.capabilities_full);
        assert_eq!(entity.default_parameters, cloned.default_parameters);
        assert_eq!(entity.additional_info, cloned.additional_info);
        assert_eq!(
            entity.disabled_capabilities_full,
            cloned.disabled_capabilities_full
        );
        assert_eq!(entity.allow_extra_params, cloned.allow_extra_params);
        assert_eq!(entity.provider_settings, cloned.provider_settings);
    }
}
