use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// Record of model approval status per tenant.
///
/// This table is the authoritative write path for approval status in P1. The
/// denormalized `models.approval_status` column is kept in sync on every write
/// so that `OData` filtering and hot reads operate without a join.
///
/// In P2+, the write path is swapped to the Approval Service; this table
/// remains as a cache/read seam.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "model_approvals")]
#[secure(tenant_col = "tenant_id", resource_col = "model_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub model_id: Uuid,
    /// Approval status: `pending`, `approved`, `rejected`, `revoked`.
    pub approval_status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::model::Entity",
        from = "Column::ModelId",
        to = "super::model::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Model,
}

impl Related<super::model::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Model.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
