use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// `SeaORM` entity for the `providers` table.
///
/// Stores provider configuration per tenant. Each provider has a unique slug
/// within the tenant namespace.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "providers")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Human-readable identifier (immutable after creation).
    /// Format: 1-64 chars, lowercase alphanumeric + hyphen.
    pub slug: String,
    pub name: String,
    /// GTS type identifier string (e.g. `gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~`).
    pub gts_type: String,
    /// Operational status: `active` or `disabled`.
    pub status: String,
    /// Whether the platform can manage this provider (e.g. install/unload models).
    pub managed: bool,
    /// Provider-specific metadata, JSONB (JSON on `SQLite`).
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,
    pub discovery_enabled: bool,
    pub discovery_interval_seconds: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::model::Entity")]
    Model,
}

impl Related<super::model::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Model.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
