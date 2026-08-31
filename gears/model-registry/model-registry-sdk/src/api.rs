// Created: 2026-04-17 by Constructor Tech
//! Public API trait for the `model-registry` module.
//!
//! [`ModelRegistryClientV1`] is registered in `ClientHub` by the module:
//! ```ignore
//! let mr = hub.get::<dyn ModelRegistryClientV1>()?;
//! ```

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use toolkit_odata::{ODataQuery, Page};

use crate::errors::ModelRegistryError;
use crate::models::{
    CreateModelRequestV1, CreateProviderRequestV1, ModelManagementV1, ModelV1, ProviderV1,
    UpdateModelRequestV1, UpdateProviderRequestV1,
};

/// Public API trait for the Model Registry (Version 1).
///
/// This trait is registered in `ClientHub` by the model-registry module:
/// ```ignore
/// let mr = hub.get::<dyn ModelRegistryClientV1>()?;
///
/// // eval read — keyed by canonical id, resolved through the tenant chain
/// let model = mr.get_tenant_model(ctx, "openai::gpt-4o").await?;
///
/// // management read/write — keyed by uuid
/// let same = mr.get_model(ctx, model.id).await?;
/// ```
///
/// All methods require `SecurityContext` for tenant scoping and authorization.
///
/// **Identifiers**: the eval read path (`get_tenant_model`) is keyed by
/// `canonical_id` because that is the only handle an inference caller has.
/// Everything else — the management read and all CRUD — is keyed by `Uuid`.
#[async_trait]
pub trait ModelRegistryClientV1: Send + Sync {
    // ==================== Models — read (P1) ====================

    /// Get a model by canonical ID within the caller's tenant context.
    ///
    /// **Eval-facing and fail-closed**: a successful return means the model is
    /// approved, live, and on an active provider that won its slug in the
    /// caller's tenant chain. For the ungated management read see
    /// [`Self::get_model`].
    ///
    /// `canonical_id` is parsed on its first `::` to recover the provider slug,
    /// which is then resolved closest-first across the ancestor chain — this is
    /// the one place in the API where a slug is interpreted.
    async fn get_tenant_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, ModelRegistryError>;

    /// List models available to the caller's tenant with `OData` filtering.
    ///
    /// Build `query` with [`QueryBuilder`](crate::odata::QueryBuilder) over
    /// [`ModelSchema`](crate::odata::ModelSchema) and the `MODEL_*` field
    /// references in [`crate::odata`] — that route needs no `$filter` text and
    /// computes the cursor `filter_hash` for you.
    ///
    /// `query.filter` and `query.order` accept exactly the fields enumerated
    /// by [`ModelFilterField`](crate::odata::ModelFilterField); anything
    /// outside that allowlist is rejected as an unknown-field validation
    /// error. Field names are flat (`gts_type`, `vision`), not nested under
    /// `info.`. Per-provider parameter and cost fields are not filterable in
    /// v1 — see `docs/DESIGN.md` §3.3.
    ///
    /// `query.select` is **not supported**: this method always returns whole
    /// [`ModelV1`] values, and a query carrying one is rejected with
    /// [`ModelRegistryError::Validation`] rather than silently ignored.
    ///
    /// Returns `ModelV1` (the default `P = serde_json::Value` for
    /// heterogeneous lists). Consumers narrowed to a specific provider (e.g.
    /// when they've already filtered on
    /// `gts_type eq 'gts.cf.genai.model.info.v1~cf.genai._.openai.v1~'`)
    /// can call [`ModelV1::try_into_typed`] on each result.
    async fn list_tenant_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, ModelRegistryError>;

    /// List models with management flags (admin endpoint).
    ///
    /// Returns every model in the caller's access scope, with no eval gates.
    /// Rows are [`ModelManagementV1`], carrying `provider_disabled` and
    /// `available_for_eval`. `include_deprecated` adds terminal-lifecycle
    /// models.
    async fn list_tenant_models_management(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        include_deprecated: bool,
    ) -> Result<Page<ModelManagementV1>, ModelRegistryError>;

    // ==================== Models — manual management (P1) ====================
    //
    // P1 admin catalog management without auto-discovery
    // (`cpt-cf-model-registry-fr-manual-model-management`). Status changes
    // (`approve` / `reject` / `revoke`) flow through `update_model` with
    // `UpdateModelRequestV1::approval_status` — no dedicated action endpoints.
    //
    // Every method here addresses entities by `Uuid`, never by slug or
    // `canonical_id`. A `canonical_id` is chain-relative — the same string
    // resolves to different rows for a parent and a child once a provider slug
    // is shadowed — so it cannot key a write, and the PDP evaluates resource
    // constraints on `id` alone.
    //
    // The same SDK methods continue to work in P2; only the implementation
    // shifts so approval status writes route through the Approval Service
    // (DESIGN §1.2 driver `fr-model-approval`).

    /// Get a model by its `Uuid` with **management** semantics.
    ///
    /// The management counterpart of [`Self::get_tenant_model`], and the read
    /// that pairs with the id-keyed CRUD methods below:
    ///
    /// - **No eval gates.** Lifecycle, provider status, and approval status are
    ///   *not* checked. A returned model may be `pending`, `rejected`,
    ///   `deprecated`, or sit on a disabled or shadowed provider — that is the
    ///   point, since an admin has to read a row before acting on it. Callers
    ///   needing eval availability must use [`Self::get_tenant_model`] or read
    ///   `available_for_eval` from [`Self::list_tenant_models_management`].
    /// - **Scope-bound.** Reads within the caller's access scope; the tenant
    ///   ancestor chain is not consulted.
    ///
    /// Returns [`ModelRegistryError::ModelNotFoundById`] when no in-scope model
    /// carries `id`.
    async fn get_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ModelV1, ModelRegistryError>;

    /// Manually register a new model in the catalog.
    ///
    /// `req.provider_id` is resolved within the caller's access scope; a
    /// provider outside it is refused with
    /// [`ModelRegistryError::ProviderNotFound`]. The created model takes the
    /// resolved provider's tenant, since a model's `tenant_id` must equal its
    /// provider's. The server derives `canonical_id` as
    /// `{provider.slug}::{req.info.provider_model_id}` from that provider.
    ///
    /// The optional `req.approval_status` is written to `models.approval_status`;
    /// defaults to [`crate::models::ApprovalStatus::Pending`] when `None`.
    async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError>;

    /// Update an existing model's mutable fields by `Uuid` (PATCH semantics).
    ///
    /// `canonical_id`, `provider_id`, `info.provider_model_id`, and
    /// `info.gts_type` are immutable — to change them, soft-delete and
    /// recreate.
    ///
    /// Scoped to the caller's access scope, the same one [`Self::get_model`]
    /// reads under.
    ///
    /// Setting `req.approval_status` updates `models.approval_status`.
    async fn update_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError>;

    /// Soft-delete a model by `Uuid` (sets `lifecycle_status` to
    /// [`crate::models::LifecycleStatus::Deprecated`]).
    ///
    /// Record is retained but hidden from default `list_tenant_models`
    /// responses. Resurrection requires recreate via [`Self::create_model`]
    /// after the previous record is purged.
    ///
    /// Scoped to the caller's access scope, like [`Self::update_model`].
    async fn delete_model(&self, ctx: &SecurityContext, id: Uuid)
    -> Result<(), ModelRegistryError>;

    // ==================== Providers (P1) ====================

    /// Get a provider by ID within the caller's access scope.
    async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderV1, ModelRegistryError>;

    /// List the providers in the caller's access scope, with `OData`
    /// filtering.
    ///
    /// `query.filter` and `query.order` accept exactly the fields enumerated
    /// by [`ProviderFilterField`](crate::odata::ProviderFilterField); build it
    /// with [`QueryBuilder`](crate::odata::QueryBuilder) over
    /// [`ProviderSchema`](crate::odata::ProviderSchema) and the `PROVIDER_*`
    /// field references. `query.select` is not supported and is rejected with
    /// [`ModelRegistryError::Validation`].
    async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, ModelRegistryError>;

    /// Register a new provider for the caller's tenant.
    async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError>;

    /// Update a provider (PATCH semantics).
    async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError>;

    /// Delete a provider by ID.
    async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), ModelRegistryError>;
}
