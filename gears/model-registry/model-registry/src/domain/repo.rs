use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::{
    CreateModelRequestV1, CreateProviderRequestV1, ModelV1, ProviderV1, UpdateModelRequestV1,
    UpdateProviderRequestV1,
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
    async fn find_by_id(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<ProviderV1, DomainError>;

    /// Find a provider by slug within the given access scope.
    ///
    /// Used by the service layer to resolve provider identity when creating
    /// models. Returns [`DomainError::ProviderNotFound`] when the slug does
    /// not exist within the scope.
    async fn find_by_slug(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        slug: &str,
    ) -> Result<ProviderV1, DomainError>;

    /// List providers matching the `OData` query within the given access scope.
    async fn list(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, DomainError>;

    /// Create a new provider.
    ///
    /// Returns [`DomainError::ProviderConflict`] when a provider with the same
    /// slug already exists within the tenant.
    async fn create(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        req: &CreateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError>;

    /// Update a provider (PATCH semantics).
    ///
    /// Only non-`None` fields in `req` are applied. Slug is immutable.
    async fn update(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
        req: &UpdateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError>;

    /// Delete a provider by ID.
    async fn delete(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<(), DomainError>;
}

/// Repository trait for model persistence.
///
/// Every method accepts a [`DBRunner`] connection and an [`AccessScope`] for
/// tenant-scoped access. Approval status is patched via the same `update`
/// flow as other model fields.
#[async_trait]
pub trait ModelRepository: Send + Sync {
    /// Find a model by canonical ID within the given access scope.
    async fn find_by_canonical(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        canonical_id: &str,
    ) -> Result<ModelV1, DomainError>;

    /// List models matching the `OData` query within the given access scope.
    ///
    /// Filtering operates entirely on `models` columns. Deprecated models are
    /// excluded by default unless the filter explicitly includes them.
    async fn list(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, DomainError>;

    /// Create a new model.
    ///
    /// Derives `canonical_id` from `req.provider_slug` + `req.info.provider_model_id`.
    /// Returns [`DomainError::ModelNotFound`] when the provider is not found
    /// (pre-check should be done by the service layer).
    async fn create(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        req: &CreateModelRequestV1,
    ) -> Result<ModelV1, DomainError>;

    /// Update a model (PATCH semantics).
    ///
    /// Only non-`None` fields in `req` are applied. Identity fields
    /// (`canonical_id`, `provider_slug`, `info.provider_model_id`,
    /// `info.gts_type`) are immutable. Approval status is patched in place
    /// alongside other fields when `req.approval_status` is `Some(...)`.
    async fn update(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        canonical_id: &str,
        req: &UpdateModelRequestV1,
    ) -> Result<ModelV1, DomainError>;

    /// Soft-delete a model by setting `lifecycle_status` to `Deprecated`.
    async fn soft_delete(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        canonical_id: &str,
    ) -> Result<(), DomainError>;
}
