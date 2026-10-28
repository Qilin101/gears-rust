// Created: 2026-04-17 by Constructor Tech
// Updated: 2026-10-26 by Constructor Tech
//! Public API trait of the `model-registry` gear (DESIGN §3.3 SDK Client
//! Trait).

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::models::{
    AvailableModelV1, CreateModelRequestV1, CreateProviderRequestV1, ModelEntryV1, ModelV1,
    ProviderEntryV1, UpdateModelRequestV1, UpdateProviderRequestV1,
};

/// Model Registry client, version 1. Registered in `ClientHub` by the gear:
///
/// ```ignore
/// let registry = hub.get::<dyn ModelRegistryClientV1>()?;
/// let model = registry.resolve_model(&ctx, "openai::gpt-4o").await?;
/// ```
///
/// Every method takes the caller's [`SecurityContext`]; the acting tenant is
/// its subject tenant. Every method returns [`CanonicalError`]; the opt-in
/// typed view is [`ModelRegistryError`](crate::ModelRegistryError), and the
/// reason and field codes are in [`crate::reason`] and [`crate::field`].
///
/// `resolve_model` returns the full [`ModelV1`]; `list_available_models`
/// returns [`AvailableModelV1`], which omits `provider_settings`, `base_url`,
/// and `secret_ref`. The management methods return [`ProviderEntryV1`] /
/// [`ModelEntryV1`], which add the `inherited`, `shadowed`, and
/// eval-availability flags. Management operations address entities by id;
/// writes succeed only on entries the acting tenant owns (inherited entries
/// fail with `INHERITED_READ_ONLY`).
///
/// Lists take an [`ODataQuery`] whose `$filter` and `$orderby` fields are
/// the [`ModelFilterField`](crate::odata::ModelFilterField) and
/// [`ProviderFilterField`](crate::odata::ProviderFilterField) allowlists;
/// `$orderby` additionally rejects the filter-only model fields
/// ([`MODEL_FILTER_ONLY_FIELDS`](crate::odata::MODEL_FILTER_ONLY_FIELDS)).
/// The SDK always returns whole records; `$select` is applied by the REST
/// layer only. Pages are keyset pages; a caller filter only narrows the
/// mandatory visibility predicates.
#[async_trait]
pub trait ModelRegistryClientV1: Send + Sync {
    // ==================== Resolution ====================

    /// Resolve a canonical id (`provider_slug::provider_model_id`) in the
    /// caller's effective catalog for evaluation.
    ///
    /// Succeeds only when the model is visible to the acting tenant, its
    /// provider is enabled, it is approved, and the PDP permits `evaluate`.
    /// Failures, in this order: model not found (`NotFound`),
    /// `PROVIDER_DISABLED`, `MODEL_NOT_APPROVED`, `NOT_PERMITTED` (all
    /// `PermissionDenied`), or dependency unavailable (`ServiceUnavailable`)
    /// when Tenant Resolver or the PDP cannot answer. Returns the full
    /// record, including `provider_settings`, `base_url`, and `secret_ref`.
    async fn resolve_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, CanonicalError>;

    /// List exactly the models `resolve_model` would return for the caller,
    /// without `provider_settings`, `base_url`, and `secret_ref`.
    ///
    /// Requires the model `list_available` permission (`NOT_PERMITTED`
    /// otherwise); an `evaluate` denial yields an empty page.
    async fn list_available_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<AvailableModelV1>, CanonicalError>;

    // ==================== Providers ====================

    /// Create a provider in the acting tenant. A duplicate slug yields
    /// `AlreadyExists`; a `gts_type` that is not a registered provider type
    /// yields `InvalidArgument` with `INVALID_GTS_TYPE`,
    /// `NOT_A_PROVIDER_TYPE`, or `GTS_TYPE_NOT_REGISTERED`.
    async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequestV1,
    ) -> Result<ProviderEntryV1, CanonicalError>;

    /// Get an own or inherited provider by id.
    async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderEntryV1, CanonicalError>;

    /// List own providers plus inherited candidates, shadowed ones included.
    async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderEntryV1>, CanonicalError>;

    /// Update an own provider. Present fields replace stored values; a
    /// change of `enabled` or `sharing_mode` applies to the next request.
    async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
    ) -> Result<ProviderEntryV1, CanonicalError>;

    /// Delete an own provider and all of its models in one transaction. A
    /// provider it shadowed becomes visible again on the next request.
    async fn delete_provider(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), CanonicalError>;

    // ==================== Models ====================

    /// Create a model under an own provider. The gear builds
    /// `canonical_id`, copies `gts_type` from the provider, validates
    /// `provider_settings` against the provider type schema
    /// (`SCHEMA_MISMATCH`), and records the approval decision (default
    /// `approved`). A duplicate canonical id yields `AlreadyExists`.
    async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelEntryV1, CanonicalError>;

    /// Get an own or inherited model by id, in any state.
    async fn get_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ModelEntryV1, CanonicalError>;

    /// Admin list under the model `list` permission: own models plus
    /// inherited candidates in every state, including models of disabled
    /// providers and models that are not approved, with flags. For the
    /// evaluable set use [`Self::list_available_models`].
    async fn list_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelEntryV1>, CanonicalError>;

    /// Update an own model. Present fields replace stored values; a present
    /// `approval_state` records a new decision; a present
    /// `provider_settings` is validated against the provider type schema.
    async fn update_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateModelRequestV1,
    ) -> Result<ModelEntryV1, CanonicalError>;

    /// Hard-delete an own model.
    async fn delete_model(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), CanonicalError>;
}
