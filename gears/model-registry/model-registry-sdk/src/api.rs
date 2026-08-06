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
    CreateModelRequestV1, CreateProviderRequestV1, ModelV1, ProviderV1, UpdateModelRequestV1,
    UpdateProviderRequestV1,
};

/// Public API trait for the Model Registry (Version 1).
///
/// This trait is registered in `ClientHub` by the model-registry module:
/// ```ignore
/// let mr = hub.get::<dyn ModelRegistryClientV1>()?;
/// let model = mr.get_tenant_model(ctx, "openai::gpt-4o").await?;
/// ```
///
/// All methods require `SecurityContext` for tenant scoping and authorization.
#[async_trait]
pub trait ModelRegistryClientV1: Send + Sync {
    // ==================== Models — read (P1) ====================

    /// Get a model by canonical ID within the caller's tenant context.
    ///
    /// Returns the model with its approval status. Uses cache-first lookup
    /// with DB fallback.
    async fn get_tenant_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, ModelRegistryError>;

    /// List models available to the caller's tenant with `OData` filtering.
    ///
    /// `query.filter` and `query.order` accept exactly the fields enumerated
    /// by [`ModelFilterField`](crate::odata::ModelFilterField); anything
    /// outside that allowlist is rejected as an unknown-field validation
    /// error. Field names are flat (`gts_type`, `vision`), not nested under
    /// `info.`. Per-provider parameter and cost fields are not filterable in
    /// v1 — see `docs/DESIGN.md` §3.3. `query.select` is not supported and is
    /// ignored: this method always returns whole [`ModelV1`] values.
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

    // ==================== Models — manual management (P1) ====================
    //
    // P1 admin catalog management without auto-discovery
    // (`cpt-cf-model-registry-fr-manual-model-management`). Status changes
    // (`approve` / `reject` / `revoke`) flow through `update_model` with
    // `UpdateModelRequestV1::approval_status` — no dedicated action endpoints.
    //
    // The same SDK methods continue to work in P2; only the implementation
    // shifts so approval status writes route through the Approval Service
    // (DESIGN §1.2 driver `fr-model-approval`).

    /// Manually register a new model in the catalog.
    ///
    /// Provider must already exist (registered via [`Self::create_provider`]
    /// or inherited from an ancestor tenant). The `canonical_id` is derived
    /// from `req.provider_slug` + `req.info.provider_model_id`.
    ///
    /// The optional `req.approval_status` is written to `models.approval_status`;
    /// defaults to [`crate::models::ApprovalStatus::Pending`] when `None`.
    async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError>;

    /// Update an existing model's mutable fields (PATCH semantics).
    ///
    /// `canonical_id`, `provider_slug`, `info.provider_model_id`, and
    /// `info.gts_type` are immutable — to change them, soft-delete and
    /// recreate.
    ///
    /// Setting `req.approval_status` updates `models.approval_status`.
    async fn update_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
        req: UpdateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError>;

    /// Soft-delete a model by canonical ID (sets `lifecycle_status` to
    /// [`crate::models::LifecycleStatus::Deprecated`]).
    ///
    /// Record is retained but hidden from default `list_tenant_models`
    /// responses. Resurrection requires recreate via [`Self::create_model`]
    /// after the previous record is purged.
    async fn delete_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<(), ModelRegistryError>;

    // ==================== Providers (P1) ====================

    /// Get a provider by ID.
    async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderV1, ModelRegistryError>;

    /// List providers for the caller's tenant with `OData` filtering.
    ///
    /// `query.filter` and `query.order` accept exactly the fields enumerated
    /// by [`ProviderFilterField`](crate::odata::ProviderFilterField).
    /// `query.select` is not supported and is ignored.
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
