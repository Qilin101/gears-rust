use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::{
    ApprovalStatus, CreateModelRequestV1, CreateProviderRequestV1, ModelV1, ProviderV1,
    UpdateModelRequestV1, UpdateProviderRequestV1,
};

use super::error::DomainError;

/// Repository trait for provider persistence.
///
/// Every method accepts a [`DBRunner`] connection and an [`AccessScope`] for
/// tenant-scoped access. The implementation uses `SecureConn` to enforce
/// tenant isolation at the query level.
#[async_trait]
pub trait ProviderRepository: Send + Sync {
    /// Find a provider by ID within the given access scope.
    async fn find_by_id<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<ProviderV1, DomainError>;

    /// List providers matching the `OData` query within the given access scope.
    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, DomainError>;

    /// Create a new provider.
    ///
    /// Returns [`DomainError::ProviderConflict`] when a provider with the same
    /// slug already exists within the tenant.
    async fn create<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        req: &CreateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError>;

    /// Update a provider (PATCH semantics).
    ///
    /// Only non-`None` fields in `req` are applied. Slug is immutable.
    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        req: &UpdateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError>;

    /// Delete a provider by ID.
    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<(), DomainError>;
}

/// Repository trait for model and approval persistence.
///
/// Every method accepts a [`DBRunner`] connection and an [`AccessScope`] for
/// tenant-scoped access. Approval operations are co-located here because they
/// share the same transaction boundary as model writes.
#[async_trait]
pub trait ModelRepository: Send + Sync {
    /// Find a model by canonical ID within the given access scope.
    async fn find_by_canonical<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        canonical_id: &str,
    ) -> Result<ModelV1, DomainError>;

    /// List models matching the `OData` query within the given access scope.
    ///
    /// Filtering operates entirely on `models` columns (including denormalized
    /// fields). Deprecated models are excluded by default unless the filter
    /// explicitly includes them.
    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, DomainError>;

    /// Create a new model.
    ///
    /// Derives `canonical_id` from `req.provider_slug` + `req.info.provider_model_id`.
    /// Returns [`DomainError::ModelNotFound`] when the provider is not found
    /// (pre-check should be done by the service layer).
    async fn create<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        req: &CreateModelRequestV1,
    ) -> Result<ModelV1, DomainError>;

    /// Update a model (PATCH semantics).
    ///
    /// Only non-`None` fields in `req` are applied. Identity fields
    /// (`canonical_id`, `provider_slug`, `info.provider_model_id`,
    /// `info.gts_type`) are immutable.
    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        canonical_id: &str,
        req: &UpdateModelRequestV1,
    ) -> Result<ModelV1, DomainError>;

    /// Soft-delete a model by setting `lifecycle_status` to `Deprecated`.
    async fn soft_delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        canonical_id: &str,
    ) -> Result<(), DomainError>;

    /// Get the approval status for a model.
    async fn get_approval<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        model_id: Uuid,
    ) -> Result<ApprovalStatus, DomainError>;

    /// Set (insert or update) the approval status for a model.
    ///
    /// This also keeps the denormalized `models.approval_status` column in sync
    /// within the same transaction.
    async fn set_approval<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        model_id: Uuid,
        status: ApprovalStatus,
    ) -> Result<(), DomainError>;

    /// Delete the approval record for a model (resets to default).
    async fn delete_approval<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        model_id: Uuid,
    ) -> Result<(), DomainError>;
}
