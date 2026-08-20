use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::{
    CreateModelRequestV1, CreateProviderRequestV1, ModelV1, ProviderV1, UpdateModelRequestV1,
    UpdateProviderRequestV1,
};

use super::error::DomainError;

/// Controls which models are visible in a repository `list` query.
///
/// Separates the eval path (mandatory predicates: provider visibility + lifecycle
/// exclusion) from the management path (no mandatory predicates beyond an optional
/// deprecated/sunset exclusion).
///
/// The enum is deliberately a sum type rather than an options struct — "forgot
/// the allow-list" is not representable when the eval path requires one.
#[derive(Debug, Clone)]
#[domain_model]
pub enum ListVisibility<'a> {
    /// Eval visibility: ANDs `provider_id IN (allow_list)` plus two unconditional
    /// exclusions — terminal lifecycle and non-approved. Models whose
    /// `lifecycle_status` is `deprecated` / `sunset`, or whose `approval_status`
    /// is anything other than `approved`, are never returned through this path,
    /// regardless of the ``OData $filter`` in the query (DESIGN §3.3).
    Eval {
        /// Provider IDs allowed for this tenant. Must be a non-empty slice to
        /// produce any results — an empty slice yields an empty page.
        allow_list: &'a [Uuid],
    },
    /// Management visibility: no mandatory predicates. `include_deprecated`
    /// controls whether terminal-lifecycle models (`deprecated` / `sunset`) are
    /// returned; when `false`, they are excluded (the default for the management
    /// listing).
    Management {
        /// When `false`, models whose `lifecycle_status` is `deprecated` or
        /// `sunset` are excluded from results.
        include_deprecated: bool,
    },
}

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

    /// Return every provider for a tenant, **unpaginated**.
    ///
    /// Used by [`build_chain_providers`](crate::domain::inheritance::build_chain_providers)
    /// which needs each tenant's **complete** provider set. `list` is paginated
    /// (default 20 / max 100), and a truncated fetch silently corrupts the
    /// allow-list — the exact predicate this method exists to prevent.
    async fn list_all_for_tenant(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
    ) -> Result<Vec<ProviderV1>, DomainError>;

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
    ///
    /// Returns the provider as it was immediately before deletion. The service
    /// layer needs its `tenant_id` to invalidate the right cache prefix: the
    /// caller's `AccessScope` may permit writes outside the caller's own
    /// tenant, so `ctx.subject_tenant_id()` is not a safe substitute.
    async fn delete(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<ProviderV1, DomainError>;
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
    /// Filtering operates entirely on `models` columns. The `visibility` mode
    /// controls which mandatory predicates are applied:
    ///
    /// - [`ListVisibility::Eval`]: ANDs `provider_id IN (allow_list)` and an
    ///   unconditional lifecycle exclusion (deprecated/sunset models are never
    ///   returned, regardless of the `$filter`).
    /// - [`ListVisibility::Management`]: no mandatory predicates beyond the
    ///   optional `include_deprecated` exclusion.
    async fn list(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        query: &ODataQuery,
        visibility: ListVisibility<'_>,
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
    ///
    /// Returns the model in its post-deprecation state. The service layer needs
    /// its `tenant_id` to invalidate the right cache prefix — see
    /// [`ProviderRepository::delete`] for why the caller's tenant is not enough.
    async fn soft_delete(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        canonical_id: &str,
    ) -> Result<ModelV1, DomainError>;
}
