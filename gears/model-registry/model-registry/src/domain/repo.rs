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
/// exclusion) from the admin path (no mandatory predicates beyond an optional
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
    /// returned; when `false`, they are excluded (the default for the admin
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

    /// Find every provider carrying `slug` within the given access scope,
    /// **unpaginated**.
    ///
    /// Serves [`get_tenant_model`](crate::domain::service::Service::get_tenant_model)'s
    /// slug resolution: one query over the whole tenant chain replaces a
    /// per-hop fan-out. Bounded by the scope and by `UNIQUE (tenant_id, slug)`,
    /// so it returns at most one row per chain tenant. An unresolved slug is an
    /// empty vector, not an error — the caller owns the
    /// [`DomainError::ProviderNotFoundBySlug`] decision.
    async fn find_all_by_slug(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        slug: &str,
    ) -> Result<Vec<ProviderV1>, DomainError>;

    /// List providers matching the `OData` query within the given access scope.
    async fn list(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, DomainError>;

    /// Fetch the providers carrying any of `ids` that fall within the scope,
    /// **unpaginated**.
    ///
    /// Bounded by the caller's id list. An empty `ids` slice returns an empty
    /// vector without querying. Ids with no in-scope row are simply absent from
    /// the result.
    async fn find_by_ids(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<ProviderV1>, DomainError>;

    /// Return every provider in the scope, **unpaginated**.
    ///
    /// Serves [`build_chain_providers`](crate::domain::inheritance::build_chain_providers)
    /// on the eval path, called once with a scope spanning the whole tenant
    /// chain: that path needs every chain tenant's **complete** provider set,
    /// and `list` is paginated (default 20 / max 100), so a truncated fetch
    /// corrupts the allow-list.
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
    /// Fails with `ProviderNotFound` when no in-scope provider carries `id`,
    /// and with `ProviderHasModels` when models still reference it.
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
///
/// Writes are keyed by `Uuid`. `find_by_canonical` exists for one caller —
/// [`get_tenant_model`](crate::domain::service::Service::get_tenant_model),
/// whose input is a `canonical_id` — and is never a write key: a `canonical_id`
/// is chain-relative, so it does not identify a row independently of the
/// requester.
#[async_trait]
pub trait ModelRepository: Send + Sync {
    /// Find a model by `Uuid` within the given access scope.
    ///
    /// Returns [`DomainError::ModelNotFoundById`] when no in-scope model
    /// carries `id`.
    async fn find_by_id(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<ModelV1, DomainError>;

    /// Find a model by canonical ID within the given access scope.
    ///
    /// Read-only helper for the eval path; see the trait-level note.
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

    /// Create a new model against an already-resolved provider.
    ///
    /// `provider` is resolved by the service layer; the repository does not
    /// look it up again. `canonical_id` is derived as
    /// `{provider.slug}::{req.info.provider_model_id}` and `provider_id` is
    /// taken from `provider.id`. `tenant_id` is the provider's tenant.
    async fn create(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        provider: &ProviderV1,
        req: &CreateModelRequestV1,
    ) -> Result<ModelV1, DomainError>;

    /// Update a model by `Uuid` (PATCH semantics).
    ///
    /// Only non-`None` fields in `req` are applied. Identity fields
    /// (`canonical_id`, `provider_id`, `info.provider_model_id`,
    /// `info.gts_type`) are immutable. Approval status is patched in place
    /// alongside other fields when `req.approval_status` is `Some(...)`.
    async fn update(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
        req: &UpdateModelRequestV1,
    ) -> Result<ModelV1, DomainError>;

    /// Soft-delete a model by `Uuid`, setting `lifecycle_status` to
    /// `Deprecated`.
    ///
    /// Fails with `ModelNotFoundById` when no in-scope model carries `id`.
    async fn soft_delete(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<(), DomainError>;
}
