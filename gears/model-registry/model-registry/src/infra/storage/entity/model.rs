use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// `SeaORM` entity for the `models` table.
///
/// Stores model catalog entries per tenant. The `info` column is the full JSONB
/// source of truth (serialized `ModelInfoV1`). The denormalized columns
/// (`gts_type`, `vendor`, `family`, `managed`, `architecture`, `format`,
/// `provider_model_id`, `supported_api`, `approval_status`, and capability
/// flags) are kept in sync and exist solely to serve `OData` filtering (the
/// toolkit `OData` layer maps filter fields to real columns, not JSONB paths).
///
/// `provider_settings` is a separate JSONB column storing the raw
/// provider-specific settings payload (the `P` in `ModelInfoV1<P>`), extracted
/// from `info` on write for direct access without full deserialization.
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
    /// Full model info envelope, JSONB (JSON on `SQLite`). Authoritative source of truth.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub info: Option<serde_json::Value>,
    /// Provider-specific settings payload, JSONB (JSON on `SQLite`).
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub provider_settings: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // ═══════════════════════════════════════════════════════════════════
    // Denormalized filterable columns (promoted from `info`)
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
        on_delete = "NoAction"
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
