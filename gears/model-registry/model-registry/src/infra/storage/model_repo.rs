//! `models` repository. The mandatory visibility predicates of DESIGN §3.6
//! are built here as SQL conditions under the scope the service passes; a
//! caller `$filter` narrows further through the paginator.

use model_registry_sdk::odata::ModelFilterField;
use model_registry_sdk::{ApprovalState, ModelV1, SharingMode};
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::odata::sea_orm_filter::{PaginateOdataTryError, paginate_odata_try};
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert, secure_update_with_scope,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::entity::model;
use super::error_mapping::{InsertFailure, classify_insert, map_scope_error};
use super::mapper;
use super::odata_mapper::{ModelODataMapper, with_default_order};
use crate::config::PageLimits;
use crate::domain::error::DomainError;

#[derive(Debug, Clone, Copy)]
pub struct ModelRepo {
    limits: PageLimits,
}

/// `tenant_id = t0 OR sharing_mode = shared`.
fn own_or_shared(t0: Uuid) -> Condition {
    Condition::any()
        .add(model::Column::TenantId.eq(t0))
        .add(model::Column::SharingMode.eq(SharingMode::Shared.as_str()))
}

impl ModelRepo {
    #[must_use]
    pub fn new(limits: PageLimits) -> Self {
        Self { limits }
    }

    /// Predicate of the available list (DESIGN §3.6 step 5): provider in the
    /// allow-list, own or shared, approved. `allow_list` must be non-empty.
    #[must_use]
    pub fn available_condition(t0: Uuid, allow_list: &[Uuid]) -> Condition {
        Condition::all()
            .add(model::Column::ProviderId.is_in(allow_list.to_vec()))
            .add(own_or_shared(t0))
            .add(model::Column::ApprovalState.eq(ApprovalState::Approved.as_str()))
    }

    /// Predicate of the management list: own models in any state, plus shared
    /// models of the inherited shared candidate providers.
    #[must_use]
    pub fn management_condition(t0: Uuid, inherited_provider_ids: &[Uuid]) -> Condition {
        let mut any = Condition::any().add(model::Column::TenantId.eq(t0));
        if !inherited_provider_ids.is_empty() {
            any = any.add(
                Condition::all()
                    .add(model::Column::ProviderId.is_in(inherited_provider_ids.to_vec()))
                    .add(model::Column::SharingMode.eq(SharingMode::Shared.as_str())),
            );
        }
        any
    }

    /// The model of the winning provider by its provider model id
    /// (DESIGN §3.6 resolve step 4), under the chain scope.
    pub async fn find_for_resolution(
        &self,
        conn: &impl DBRunner,
        chain: &[Uuid],
        provider_id: Uuid,
        provider_model_id: &str,
    ) -> Result<Option<ModelV1>, DomainError> {
        model::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenants(chain.to_vec()))
            .filter(
                Condition::all()
                    .add(model::Column::ProviderId.eq(provider_id))
                    .add(model::Column::ProviderModelId.eq(provider_model_id)),
            )
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .map(mapper::model_from_row)
            .transpose()
    }

    pub async fn find_by_id(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ModelV1>, DomainError> {
        model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::Id.eq(id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .map(mapper::model_from_row)
            .transpose()
    }

    /// Keyset page in `canonical_id asc, id asc` unless the caller ordered
    /// otherwise.
    pub async fn list(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        predicate: Condition,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, DomainError> {
        let base = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(predicate);
        let query = with_default_order(query, "canonical_id");
        paginate_odata_try::<ModelFilterField, ModelODataMapper, model::Entity, ModelV1, _, _, _>(
            base,
            conn,
            &query,
            ("id", SortDir::Asc),
            self.limits.limit_cfg(),
            mapper::model_from_row,
        )
        .await
        .map_err(|e| match e {
            PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
                DomainError::from_db_message(&message)
            }
            PaginateOdataTryError::OData(e) => DomainError::OData(e),
            PaginateOdataTryError::MapError(e) => e,
        })
    }

    /// Inserts under `tenant_id = model.tenant_id`. A duplicate canonical id
    /// is `ModelAlreadyExists`; a missing provider is `ProviderNotFound`.
    pub async fn insert(&self, tx: &impl DBRunner, model: &ModelV1) -> Result<(), DomainError> {
        let scope = AccessScope::for_tenant(model.tenant_id);
        let am = mapper::model_to_active(model)?;
        match secure_insert::<model::Entity>(am, &scope, tx).await {
            Ok(_) => Ok(()),
            Err(e) => Err(match classify_insert(&e) {
                InsertFailure::UniqueViolation => DomainError::ModelAlreadyExists {
                    canonical_id: model.canonical_id.clone(),
                },
                InsertFailure::ForeignKeyViolation => DomainError::ProviderNotFound {
                    id: model.provider_id,
                },
                InsertFailure::Other => map_scope_error(e),
            }),
        }
    }

    /// Loads the row under `tenant_id = t0` with an exclusive row lock; `None`
    /// when T0 owns no such model.
    pub async fn find_for_update(
        &self,
        tx: &impl DBRunner,
        t0: Uuid,
        id: Uuid,
    ) -> Result<Option<ModelV1>, DomainError> {
        model::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(t0))
            .filter(Condition::all().add(model::Column::Id.eq(id)))
            .lock_exclusive()
            .one(tx)
            .await
            .map_err(map_scope_error)?
            .map(mapper::model_from_row)
            .transpose()
    }

    /// Rewrites every column, promoted ones included, under
    /// `tenant_id = model.tenant_id`.
    pub async fn save(&self, tx: &impl DBRunner, model: &ModelV1) -> Result<ModelV1, DomainError> {
        let scope = AccessScope::for_tenant(model.tenant_id);
        let am = mapper::model_to_active(model)?;
        secure_update_with_scope::<model::Entity>(am, &scope, model.id, tx)
            .await
            .map_err(map_scope_error)
            .and_then(mapper::model_from_row)
    }

    /// Hard delete under `tenant_id = t0`. Returns whether the row existed.
    pub async fn delete(
        &self,
        tx: &impl DBRunner,
        t0: Uuid,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let result = model::Entity::delete_many()
            .secure()
            .scope_with(&AccessScope::for_tenant(t0))
            .filter(Condition::all().add(model::Column::Id.eq(id)))
            .exec(tx)
            .await
            .map_err(map_scope_error)?;
        Ok(result.rows_affected > 0)
    }
}
