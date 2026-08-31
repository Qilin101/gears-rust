//! Application service for the Model Registry gear.
//!
//! Orchestrates authorization and persistence for providers and models.
//! Generic over the two repository traits so unit tests can inject mocks; the
//! cache is a trait object, since which implementation is installed is a
//! runtime decision.
//!
//! Every method derives an [`AccessScope`] from the PDP via [`PolicyEnforcer`]
//! and applies it to each query.
//!
//! The two eval reads — [`Service::get_tenant_model`] and
//! [`Service::list_tenant_models`] — additionally resolve the caller's tenant
//! ancestor chain (served from the [`ResolutionCache`]) to apply additive
//! inheritance and provider shadowing. The admin surface does not: it reads and
//! writes exactly what the PDP scope covers.

use std::collections::HashMap;
use std::sync::Arc;

use authz_resolver_sdk::pep::{PolicyEnforcer, ResourceType};
use tenant_resolver_sdk::TenantResolverClient;
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::cache::ResolutionCache;
use super::error::DomainError;
use super::inheritance::{build_chain_providers, chain_read_scope, resolve_ancestors};
use super::repo::{ListVisibility, ModelRepository, ProviderRepository};

use crate::config::ModelRegistryConfig;
use crate::{
    ApprovalStatus, CreateProviderRequestV1, LifecycleStatus, ModelManagementV1, ProviderStatus,
    ProviderV1, UpdateProviderRequestV1,
};

// ---------------------------------------------------------------------------
// Authorization resource type constants
// ---------------------------------------------------------------------------

/// Authorization resource type for provider operations.
pub(crate) const PROVIDER_RESOURCE: ResourceType = ResourceType::from_static(
    "model_registry.provider",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type for model operations.
pub(crate) const MODEL_RESOURCE: ResourceType = ResourceType::from_static(
    "model_registry.model",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Reject a listing query that carries `$select`.
///
/// The listing methods return whole `ProviderV1` / `ModelV1` values and there
/// is no projection stage, so `select` cannot be honoured and ignoring it
/// would hand back more than the caller asked for. The REST layer rejects the
/// clause earlier with a `$select` field violation (a better shape over HTTP);
/// this guard covers the in-process SDK path, including a query built by
/// `QueryBuilder::select`.
fn reject_select(query: &ODataQuery) -> Result<(), DomainError> {
    if query.select.is_some() {
        return Err(DomainError::validation(
            "$select is not supported by this endpoint; responses always carry every field",
        ));
    }
    Ok(())
}

/// Well-known action names for PDP evaluation.
pub(crate) mod actions {
    /// Get / read a single resource.
    pub const GET: &str = "get";
    /// List / search resources.
    pub const LIST: &str = "list";
    /// Create a new resource.
    pub const CREATE: &str = "create";
    /// Update an existing resource.
    pub const UPDATE: &str = "update";
    /// Delete a resource.
    pub const DELETE: &str = "delete";
    /// List / search resources with management flags (admin endpoint).
    pub const LIST_MANAGEMENT: &str = "list_management";
    /// Read a single resource without the eval gates (admin endpoint).
    ///
    /// Distinct from [`GET`] for the same reason [`LIST_MANAGEMENT`] is
    /// distinct from [`LIST`]: the management read returns rows the eval read
    /// refuses (pending, rejected, deprecated, shadowed, disabled-provider), so
    /// it must be grantable separately.
    pub const GET_MANAGEMENT: &str = "get_management";
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Application service orchestrating authorization, chain resolution,
/// inheritance resolution, and persistence.
///
/// Generic over:
/// - `R`: repository implementing [`ProviderRepository`]
/// - `M`: repository implementing [`ModelRepository`]
///
/// The cache is held as `Arc<dyn ResolutionCache>` rather than a type
/// parameter: its concrete type is a runtime choice between
/// `ClusterResolutionCache` and `NoopResolutionCache`, so a parameter would
/// force two `Service` instantiations in the gear for no benefit.
#[domain_model]
pub struct Service<R, M> {
    db: Arc<DBProvider<toolkit_db::DbError>>,
    provider_repo: Arc<R>,
    model_repo: Arc<M>,
    cache: Arc<dyn ResolutionCache>,
    tenant_resolver: Arc<dyn TenantResolverClient>,
    policy_enforcer: PolicyEnforcer,
    config: ModelRegistryConfig,
}

impl<R: ProviderRepository, M: ModelRepository> Service<R, M> {
    /// Validate that `discovery_interval_seconds` fits within `i32` range.
    ///
    /// The DB column is `i32`, so values exceeding `i32::MAX` must be rejected
    /// at the application layer rather than silently truncated.
    fn validate_discovery_interval(interval: Option<u32>) -> Result<(), DomainError> {
        if let Some(v) = interval
            && v > i32::MAX as u32
        {
            return Err(DomainError::validation(format!(
                "discovery_interval_seconds must not exceed {} (i32::MAX), got {v}",
                i32::MAX,
            )));
        }
        Ok(())
    }

    /// Create a new service instance.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DBProvider<toolkit_db::DbError>>,
        provider_repo: Arc<R>,
        model_repo: Arc<M>,
        cache: Arc<dyn ResolutionCache>,
        tenant_resolver: Arc<dyn TenantResolverClient>,
        policy_enforcer: PolicyEnforcer,
        config: ModelRegistryConfig,
    ) -> Self {
        Self {
            db,
            provider_repo,
            model_repo,
            cache,
            tenant_resolver,
            policy_enforcer,
            config,
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    /// Derive a tenant-scoped `AccessScope` via the `PolicyEnforcer`.
    ///
    /// Calls the PDP to evaluate the given action against the resource type,
    /// returning the compiled scope. The caller must have been authenticated.
    ///
    /// `resource_id` is `Some(id)` for every point operation (get / update /
    /// delete) and `None` for collection operations (list, create), where no
    /// single resource is addressed yet. Both resource types declare
    /// `pep_properties::RESOURCE_ID`, so a policy may return `id`-scoped
    /// constraints — which is only useful if the id is actually supplied.
    async fn derive_access_scope(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, resource, action, resource_id)
            .await?)
    }

    // ── Provider operations ──────────────────────────────────────────────

    /// Get a provider by ID within the caller's PDP access scope.
    ///
    /// A provider outside that scope yields
    /// [`DomainError::ProviderNotFound`].
    pub async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderV1, DomainError> {
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::GET, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;
        self.provider_repo.find_by_id(&conn, &scope, id).await
    }

    /// List the providers in the caller's PDP access scope, with `OData`
    /// filtering.
    pub async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, DomainError> {
        reject_select(query)?;

        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::LIST, None)
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;
        self.provider_repo.list(&conn, &scope, query).await
    }

    /// Create a new provider.
    ///
    /// Validates slug and display-name format, checks authorization, and
    /// delegates to the repository.
    pub async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: &CreateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError> {
        // 1. Validate slug format, display name and discovery interval
        Self::validate_slug(req.slug())?;
        Self::validate_name(req.name())?;
        Self::validate_discovery_interval(req.discovery_interval_seconds())?;

        // 2. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::CREATE, None)
            .await?;

        // 3. Create via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let tenant_id = ctx.subject_tenant_id();
        let provider = self
            .provider_repo
            .create(&conn, &scope, tenant_id, req)
            .await?;

        Ok(provider)
    }

    /// Update a provider (PATCH semantics).
    ///
    /// Slug is immutable — the repository rejects changes to it.
    pub async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: &UpdateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError> {
        // 1. Validate the fields being updated
        if let Some(name) = req.name.as_deref() {
            Self::validate_name(name)?;
        }
        Self::validate_discovery_interval(req.discovery_interval_seconds.flatten())?;

        // 2. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::UPDATE, Some(id))
            .await?;

        // 3. Update via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let provider = self.provider_repo.update(&conn, &scope, id, req).await?;

        Ok(provider)
    }

    /// Delete a provider.
    pub async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::DELETE, Some(id))
            .await?;

        // 2. Delete via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.provider_repo.delete(&conn, &scope, id).await?;

        Ok(())
    }
}

// ── Validation (no trait bounds) ─────────────────────────────────────

impl<R, M> Service<R, M> {
    /// Validate provider slug format.
    ///
    /// Rules: 1-64 characters, lowercase alphanumeric + hyphens only.
    fn validate_slug(slug: &str) -> Result<(), DomainError> {
        if slug.is_empty() {
            return Err(DomainError::validation("provider slug cannot be empty"));
        }
        if slug.len() > 64 {
            return Err(DomainError::validation(
                "provider slug must be at most 64 characters",
            ));
        }
        if !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(DomainError::validation(
                "provider slug must be lowercase alphanumeric with hyphens",
            ));
        }
        Ok(())
    }

    /// Validate provider display name.
    ///
    /// Free-form text, bounded by the stored column: 1-255 characters. Counted
    /// in characters rather than bytes because that is what the column bounds.
    fn validate_name(name: &str) -> Result<(), DomainError> {
        if name.is_empty() {
            return Err(DomainError::validation("provider name cannot be empty"));
        }
        if name.chars().count() > 255 {
            return Err(DomainError::validation(
                "provider name must be at most 255 characters",
            ));
        }
        Ok(())
    }

    /// Validate lifecycle state transitions.
    ///
    /// Rules:
    /// - `Deprecated` and `Sunset` are terminal — no transitions out.
    /// - Any status can transition to `Deprecated`.
    /// - All other transitions are permitted (promotion, demotion).
    fn validate_lifecycle_transition(
        from: LifecycleStatus,
        to: LifecycleStatus,
    ) -> Result<(), DomainError> {
        // Terminal states: cannot transition OUT of Deprecated or Sunset.
        if matches!(from, LifecycleStatus::Deprecated | LifecycleStatus::Sunset) {
            return Err(DomainError::invalid_transition(format!(
                "cannot transition from terminal status {from:?} to {to:?}"
            )));
        }
        Ok(())
    }
}

// ── Model read operations ──────────────────────────────────────────────

impl<R: ProviderRepository, M: ModelRepository> Service<R, M> {
    /// Get a model by canonical ID via slug resolution (DESIGN §3.5).
    ///
    /// Resolves the provider slug from `canonical_id` (the segment before `::`)
    /// closest-first across the tenant chain, then reads the model only from
    /// the winning tenant. Applies gates in order (C2, C4):
    ///
    /// 1. `provider_id` mismatch → `ModelNotFound` (inconsistent data)
    /// 2. Terminal lifecycle → `ModelDeprecated`
    /// 3. Disabled provider → `ProviderDisabled`
    /// 4. Not `approved` → `ModelNotApproved`
    ///
    /// The read is a **fail-closed access gate**: a successful return means
    /// approved, live, and on an active winning provider. Approval is checked
    /// last so a 403 is only ever returned for a model the caller could
    /// otherwise observe (DESIGN §3.5).
    ///
    /// A malformed `canonical_id` (no `::` separator) yields `ModelNotFound`.
    /// An unresolved slug yields `ProviderNotFoundBySlug`.
    ///
    /// Slug resolution is **fail-closed**: any non-not-found query error at any
    /// chain hop propagates as `Internal`.
    pub async fn get_tenant_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<crate::ModelV1, DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let own_scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::GET, None)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance =
            resolve_ancestors(self.tenant_resolver.as_ref(), self.cache.as_ref(), ctx).await?;

        // 3. Split canonical_id on the first `::` to get slug and model id.
        let slug = canonical_id.split_once("::").map(|(s, _)| s);
        let Some(slug) = slug else {
            return Err(DomainError::model_not_found(canonical_id));
        };

        // 4. Resolve the slug against the database with one chain-wide query,
        //    then rank the rows by chain distance. Fail-closed: the `?` covers
        //    the whole chain, so no hop can be silently skipped and un-shadow
        //    an ancestor.
        let conn = self.db.conn().map_err(DomainError::from)?;
        let conn = &conn;

        // Provider resolution carries no PDP scope on this path — the per-hop
        // fan-out this replaces used a plain `for_tenant` at every hop, own
        // tenant included, so the chain-wide scope is exactly their union.
        let chain_ids: Vec<Uuid> = inheritance.chain_ids().copied().collect();
        let rows = self
            .provider_repo
            .find_all_by_slug(conn, &AccessScope::for_tenants(chain_ids), slug)
            .await?;

        // The same primitive the listing path builds, over at most one row per
        // chain tenant. `winner_for_slug` returns a disabled winner too, so the
        // gate order below can report `ProviderDisabled` only after the model
        // is found (DESIGN §3.5).
        let chain = build_chain_providers(&inheritance, rows);
        let Some(winner) = chain.winner_for_slug(slug) else {
            return Err(DomainError::provider_not_found_by_slug(slug));
        };
        let winner_tenant_id = winner.owner_tenant;

        // 5. Scope the model read:
        //    - Winner is own tenant → use the PDP-derived own_scope (preserves
        //      compiled constraints).
        //    - Winner is an ancestor → construct a scoped scope.
        let model_scope = if winner_tenant_id == inheritance.tenant_id() {
            own_scope
        } else {
            AccessScope::for_tenant(winner_tenant_id)
        };

        // 6. Read the model from the winner's tenant.
        let model = match self
            .model_repo
            .find_by_canonical(conn, &model_scope, canonical_id)
            .await
        {
            Ok(model) => model,
            Err(DomainError::ModelNotFound { .. }) => {
                return Err(DomainError::model_not_found(canonical_id));
            }
            Err(e) => return Err(e),
        };

        // 7. Apply gates in order (C2, C4).
        //    Gate 1: provider_id must match the winning provider. A defensive
        //    fail-closed guard — unreachable in a consistent database.
        if model.provider_id != winner.id {
            return Err(DomainError::model_not_found(canonical_id));
        }

        //    Gate 2: terminal lifecycle → ModelDeprecated.
        if matches!(
            model.lifecycle_status,
            crate::LifecycleStatus::Deprecated | crate::LifecycleStatus::Sunset
        ) {
            return Err(DomainError::model_deprecated(canonical_id));
        }

        //    Gate 3: disabled winning provider → ProviderDisabled. Ordered
        //    after the model lookup so `ProviderDisabled` cannot leak for a
        //    `canonical_id` that names no model of that provider.
        if !matches!(winner.status, crate::ProviderStatus::Active) {
            return Err(DomainError::provider_disabled(winner.id));
        }

        //    Gate 4: not approved → ModelNotApproved.
        if !matches!(model.approval_status, ApprovalStatus::Approved) {
            return Err(DomainError::model_not_approved(canonical_id));
        }

        Ok(model)
    }

    /// List models with `OData` filtering and inheritance.
    ///
    /// Returns models from the own tenant (with full `OData` support) merged with
    /// models inherited from ancestor tenants. Child-tenant models shadow
    /// ancestor models with the same `canonical_id`. Deprecated models are
    /// excluded from the eval list (filtered by the repository layer).
    ///
    /// Builds `ChainProviders(T0)` from one chain-wide provider query, then
    /// issues **one** model query scoped to the whole chain with
    /// `ListVisibility::Eval` carrying the chain-wide allow-list. The database
    /// therefore applies `$filter`, `$orderby` and cursor pagination across
    /// own and inherited rows alike — there is no in-memory merge.
    ///
    /// No dedupe step is needed: `allow_list` admits at most one provider per
    /// slug across the chain, and `canonical_id` is `{slug}::{provider_model_id}`
    /// over a `UNIQUE (tenant_id, canonical_id)` table, so two admitted rows
    /// cannot share one (DESIGN §3.5 Corollary). That uniqueness is also what
    /// makes `canonical_id` a valid keyset tiebreaker over the merged set.
    pub async fn list_tenant_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<crate::ModelV1>, DomainError> {
        reject_select(query)?;

        // 1. Derive access scope (authorization check + DB scope)
        let own_scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::LIST, None)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance =
            resolve_ancestors(self.tenant_resolver.as_ref(), self.cache.as_ref(), ctx).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;

        // 3. Build ChainProviders(T0) from one chain-wide provider query.
        //    Fail closed: a provider query that failed would un-shadow an
        //    ancestor, so the error propagates rather than narrowing the view.
        let chain_ids: Vec<Uuid> = inheritance.chain_ids().copied().collect();
        let providers = self
            .provider_repo
            .list_all_for_tenant(&conn, &AccessScope::for_tenants(chain_ids))
            .await?;
        let chain = build_chain_providers(&inheritance, providers);

        // 4. No active winning provider anywhere in the chain: nothing can
        //    match. Synthesize the page rather than issuing a query whose
        //    `IN ()` predicate has no portable rendering.
        let allow_list = chain.allow_list();
        if allow_list.is_empty() {
            return Ok(Page {
                items: vec![],
                page_info: toolkit_odata::PageInfo {
                    // The effective limit the repository would have resolved.
                    limit: self.config.page_limits().clamp(query.limit),
                    next_cursor: None,
                    prev_cursor: None,
                },
            });
        }

        // 5. One chain-wide model query: the database applies the caller's
        //    filter, ordering and cursor over own and inherited rows together.
        let scope = chain_read_scope(&own_scope, &inheritance);
        self.model_repo
            .list(&conn, &scope, query, ListVisibility::Eval { allow_list })
            .await
    }

    /// List models with management flags for the admin endpoint.
    ///
    /// Returns every model in the caller's PDP access scope, with no eval
    /// gates: rows that are `pending`, `rejected`, or sit on a disabled
    /// provider all come back. `include_deprecated` controls whether
    /// terminal-lifecycle rows are included.
    ///
    /// Each row carries `provider_disabled` and `available_for_eval`, computed
    /// from the model's own provider. A provider that cannot be read leaves
    /// both flags `false`.
    pub async fn list_tenant_models_management(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        include_deprecated: bool,
    ) -> Result<Page<crate::ModelManagementV1>, DomainError> {
        reject_select(query)?;

        let scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::LIST_MANAGEMENT, None)
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;
        let page = self
            .model_repo
            .list(
                &conn,
                &scope,
                query,
                ListVisibility::Management { include_deprecated },
            )
            .await?;

        // A model's tenant always equals its provider's (§3.1 Invariants), so
        // the providers backing this page are reachable under a scope built
        // from the tenants of the rows the caller was already authorized to
        // read. The model scope itself cannot be reused: its constraints bind
        // to `models` columns.
        let mut tenant_ids: Vec<Uuid> = page.items.iter().map(|m| m.tenant_id).collect();
        tenant_ids.sort_unstable();
        tenant_ids.dedup();

        let mut provider_ids: Vec<Uuid> = page.items.iter().map(|m| m.provider_id).collect();
        provider_ids.sort_unstable();
        provider_ids.dedup();

        let provider_scope = AccessScope::for_tenants(tenant_ids);
        let providers: HashMap<Uuid, ProviderStatus> = self
            .provider_repo
            .find_by_ids(&conn, &provider_scope, &provider_ids)
            .await?
            .into_iter()
            .map(|p| (p.id, p.status))
            .collect();

        let items: Vec<crate::ModelManagementV1> = page
            .items
            .into_iter()
            .map(|model| {
                let status = providers.get(&model.provider_id);
                let provider_disabled = status.is_some_and(|s| *s != ProviderStatus::Active);
                let available_for_eval = status.is_some_and(|s| *s == ProviderStatus::Active)
                    && !matches!(
                        model.lifecycle_status,
                        LifecycleStatus::Deprecated | LifecycleStatus::Sunset
                    )
                    && matches!(model.approval_status, ApprovalStatus::Approved);

                ModelManagementV1 {
                    model,
                    provider_disabled,
                    available_for_eval,
                }
            })
            .collect();

        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }

    /// Get a model by `Uuid` with management semantics.
    ///
    /// The management counterpart of [`Self::get_tenant_model`]: it applies
    /// **no** eval gates — lifecycle, provider status, and approval status are
    /// not consulted — and reads within the caller's PDP access scope. A model
    /// outside that scope yields [`DomainError::ModelNotFoundById`].
    ///
    /// Authorized under [`actions::GET_MANAGEMENT`] rather than
    /// [`actions::GET`], so a plain tenant member holding only the eval read
    /// grant cannot use it to observe rows the eval read hides.
    pub async fn get_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<crate::ModelV1, DomainError> {
        let scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::GET_MANAGEMENT, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;
        self.model_repo.find_by_id(&conn, &scope, id).await
    }

    /// Create a new model against a provider addressed by `Uuid`.
    ///
    /// The provider is resolved under its own PDP decision
    /// (`model_registry.provider` / [`actions::GET`]); the model is written
    /// under the `model_registry.model` [`actions::CREATE`] scope and takes the
    /// resolved provider's `tenant_id`, since a model's tenant MUST equal its
    /// provider's (§3.1 Invariants). `canonical_id` is derived from the
    /// resolved provider's slug.
    ///
    /// A `provider_id` outside the provider read scope yields
    /// [`DomainError::ProviderNotFound`]. A provider whose tenant lies outside
    /// the model create scope is refused by the repository as `Forbidden`.
    pub async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: &crate::CreateModelRequestV1,
    ) -> Result<crate::ModelV1, DomainError> {
        // 1. Reject models created directly in a terminal lifecycle state.
        if matches!(
            req.lifecycle_status,
            crate::LifecycleStatus::Deprecated | crate::LifecycleStatus::Sunset
        ) {
            return Err(DomainError::validation(format!(
                "cannot create a model with terminal lifecycle status `{:?}`",
                req.lifecycle_status,
            )));
        }

        // 2. Two decisions: one to write the model, one to read the provider it
        //    attaches to. The model scope's constraints bind to `models`
        //    columns, so it cannot scope the provider lookup.
        let model_scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::CREATE, None)
            .await?;
        let provider_scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::GET, Some(req.provider_id))
            .await?;

        // 3. Resolve the provider; it carries the slug `canonical_id` is built
        //    from, so the repository does not look it up again.
        let conn = self.db.conn().map_err(DomainError::from)?;
        let provider = self
            .provider_repo
            .find_by_id(&conn, &provider_scope, req.provider_id)
            .await?;

        if !matches!(provider.status, crate::ProviderStatus::Active) {
            return Err(DomainError::ProviderDisabled { id: provider.id });
        }

        // 4. Create under the model scope, owned by the provider's tenant.
        let model = self
            .model_repo
            .create(&conn, &model_scope, provider.tenant_id, &provider, req)
            .await?;

        Ok(model)
    }

    /// Update a model by `Uuid` (PATCH semantics) including approval status.
    ///
    /// Applies non-status field patches and `approval_status` transitions in a
    /// single repository call. Validates lifecycle state transitions.
    ///
    /// Own-tenant-only: the caller's PDP-derived scope is the only scope used,
    /// so an inherited model — readable via [`Self::get_model`] — is not
    /// writable here.
    pub async fn update_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: &crate::UpdateModelRequestV1,
    ) -> Result<crate::ModelV1, DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::UPDATE, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // 2. Fetch existing model to validate state transitions.
        let existing = self.model_repo.find_by_id(&conn, &scope, id).await?;

        // 3. Validate lifecycle state transitions
        if let Some(new_lifecycle) = &req.lifecycle_status {
            Self::validate_lifecycle_transition(existing.lifecycle_status, *new_lifecycle)?;
        }

        // 3b. Prevent approval changes on terminal-lifecycle models (deprecated
        //     or sunset) since those states are effectively read-only.
        if req.approval_status.is_some()
            && matches!(
                existing.lifecycle_status,
                crate::LifecycleStatus::Deprecated | crate::LifecycleStatus::Sunset
            )
        {
            return Err(DomainError::invalid_transition(
                "cannot modify approval status on a deprecated or sunset model",
            ));
        }

        // 4. Update via repo (mapper projects approval_status alongside other
        //    fields).
        let model = self.model_repo.update(&conn, &scope, id, req).await?;

        Ok(model)
    }

    /// Soft-delete a model by `Uuid`.
    ///
    /// Sets `lifecycle_status` to `Deprecated` and records the deprecation
    /// timestamp. Own-tenant-only, like [`Self::update_model`].
    pub async fn delete_model(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::DELETE, Some(id))
            .await?;

        // 2. Soft-delete via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.model_repo.soft_delete(&conn, &scope, id).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use async_trait::async_trait;
    use authz_resolver_sdk::pep::PolicyEnforcer;
    use authz_resolver_sdk::{
        AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
    };
    use model_registry_sdk::models::{
        ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities, DisabledMediaCapability,
        DisabledReasoningCapability, DisabledWebSearchCapability, MediaCapability,
        ModelCapabilities, ModelPerformance, ReasoningCapability, SupportedApi,
        WebSearchCapability,
    };
    use sea_orm_migration::MigratorTrait;
    use tenant_resolver_sdk::{
        GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
        GetTenantsOptions, IsAncestorOptions, TenantId, TenantRef, TenantResolverClient,
        TenantResolverError, TenantStatus,
    };
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::secure::DBRunner;
    use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
    use toolkit_odata::ODataQuery;
    use toolkit_security::{AccessScope, SecurityContext};
    use uuid::Uuid;

    use super::*;
    use crate::domain::cache::NoopResolutionCache;
    use crate::infra::storage::migrations::Migrator;
    use crate::infra::storage::model_repo::ModelRepositoryImpl;
    use crate::infra::storage::provider_repo::ProviderRepositoryImpl;

    // ═════════════════════════════════════════════════════════════════════════
    // Mock AuthZResolverClient — always returns permissive responses
    // ═════════════════════════════════════════════════════════════════════════

    #[domain_model]
    struct MockAuthZ;

    #[async_trait]
    impl AuthZResolverClient for MockAuthZ {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            // Extract the caller's tenant ID from the subject properties (set by
            // PolicyEnforcer). Default to a nil UUID if somehow absent.
            let tenant_id = request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .unwrap_or_else(Uuid::nil);

            // Return a permissive response with a simple `Eq` constraint on
            // `owner_tenant_id`. We avoid InTenantSubtree because that requires
            // a `tenant_closure` table that our test DB doesn't have.
            Ok(EvaluationResponse {
                decision: true,
                context: authz_resolver_sdk::EvaluationResponseContext {
                    constraints: vec![authz_resolver_sdk::constraints::Constraint {
                        predicates: vec![authz_resolver_sdk::constraints::Predicate::Eq(
                            authz_resolver_sdk::constraints::EqPredicate {
                                property: "owner_tenant_id".to_owned(),
                                value: serde_json::json!(tenant_id.to_string()),
                            },
                        )],
                    }],
                    deny_reason: None,
                },
            })
        }
    }

    /// PDP that grants the caller write access across a fixed set of tenants,
    /// not just its own — the shape a real policy produces for a role like
    /// `platform-admin` ("own + descendants", DESIGN §4 Authorization).
    ///
    /// [`MockAuthZ`] pins every scope to the caller's own tenant, so it cannot
    /// exercise the case where the row a write lands on is owned by someone
    /// other than the caller.
    #[domain_model]
    struct MockAuthZTenants(Vec<Uuid>);

    #[async_trait]
    impl AuthZResolverClient for MockAuthZTenants {
        async fn evaluate(
            &self,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            Ok(EvaluationResponse {
                decision: true,
                context: authz_resolver_sdk::EvaluationResponseContext {
                    constraints: vec![authz_resolver_sdk::constraints::Constraint {
                        predicates: vec![authz_resolver_sdk::constraints::Predicate::In(
                            authz_resolver_sdk::constraints::InPredicate::new(
                                "owner_tenant_id",
                                self.0.iter().map(ToString::to_string),
                            ),
                        )],
                    }],
                    deny_reason: None,
                },
            })
        }
    }

    /// PDP that pins the caller to its own tenant **and** a single row id — the
    /// shape a row-level policy produces. Used to prove the eval listing keeps
    /// enforcing PDP constraints on own-tenant rows after the chain-wide query
    /// collapsed the per-tenant fan-out.
    #[domain_model]
    struct MockAuthZRowScoped {
        tenant_id: Uuid,
        resource_id: Uuid,
    }

    #[async_trait]
    impl AuthZResolverClient for MockAuthZRowScoped {
        async fn evaluate(
            &self,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            Ok(EvaluationResponse {
                decision: true,
                context: authz_resolver_sdk::EvaluationResponseContext {
                    constraints: vec![authz_resolver_sdk::constraints::Constraint {
                        predicates: vec![
                            authz_resolver_sdk::constraints::Predicate::Eq(
                                authz_resolver_sdk::constraints::EqPredicate {
                                    property: "owner_tenant_id".to_owned(),
                                    value: serde_json::json!(self.tenant_id.to_string()),
                                },
                            ),
                            authz_resolver_sdk::constraints::Predicate::In(
                                authz_resolver_sdk::constraints::InPredicate::new(
                                    "id",
                                    [self.resource_id.to_string()],
                                ),
                            ),
                        ],
                    }],
                    deny_reason: None,
                },
            })
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Mock TenantResolverClient variants
    // ═════════════════════════════════════════════════════════════════════════

    /// Returns no ancestors (single-tenant scenario).
    #[domain_model]
    struct NoAncestorsResolver;

    #[async_trait]
    impl TenantResolverClient for NoAncestorsResolver {
        async fn get_ancestors(
            &self,
            _ctx: &SecurityContext,
            id: TenantId,
            _options: &GetAncestorsOptions,
        ) -> Result<GetAncestorsResponse, TenantResolverError> {
            Ok(GetAncestorsResponse {
                tenant: TenantRef {
                    id,
                    status: TenantStatus::Active,
                    tenant_type: None,
                    parent_id: None,
                    self_managed: false,
                },
                ancestors: vec![],
            })
        }

        async fn get_tenant(
            &self,
            _: &SecurityContext,
            _: TenantId,
        ) -> Result<tenant_resolver_sdk::TenantInfo, TenantResolverError> {
            unimplemented!()
        }
        async fn get_root_tenant(
            &self,
            _: &SecurityContext,
        ) -> Result<tenant_resolver_sdk::TenantInfo, TenantResolverError> {
            unimplemented!()
        }
        async fn get_tenants(
            &self,
            _: &SecurityContext,
            _: &[TenantId],
            _: &GetTenantsOptions,
        ) -> Result<Vec<tenant_resolver_sdk::TenantInfo>, TenantResolverError> {
            unimplemented!()
        }
        async fn get_descendants(
            &self,
            _: &SecurityContext,
            _: TenantId,
            _: &GetDescendantsOptions,
        ) -> Result<GetDescendantsResponse, TenantResolverError> {
            unimplemented!()
        }
        async fn is_ancestor(
            &self,
            _: &SecurityContext,
            _: TenantId,
            _: TenantId,
            _: &IsAncestorOptions,
        ) -> Result<bool, TenantResolverError> {
            unimplemented!()
        }
    }

    /// Returns a fixed ancestor chain: child → parent → grandparent.
    #[domain_model]
    struct TwoAncestorsResolver;

    fn parent_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }

    fn grandparent_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()
    }

    #[async_trait]
    impl TenantResolverClient for TwoAncestorsResolver {
        async fn get_ancestors(
            &self,
            _ctx: &SecurityContext,
            id: TenantId,
            _options: &GetAncestorsOptions,
        ) -> Result<GetAncestorsResponse, TenantResolverError> {
            Ok(GetAncestorsResponse {
                tenant: TenantRef {
                    id,
                    status: TenantStatus::Active,
                    tenant_type: None,
                    parent_id: Some(TenantId(parent_id())),
                    self_managed: false,
                },
                ancestors: vec![
                    TenantRef {
                        id: TenantId(parent_id()),
                        status: TenantStatus::Active,
                        tenant_type: None,
                        parent_id: Some(TenantId(grandparent_id())),
                        self_managed: false,
                    },
                    TenantRef {
                        id: TenantId(grandparent_id()),
                        status: TenantStatus::Active,
                        tenant_type: None,
                        parent_id: None,
                        self_managed: false,
                    },
                ],
            })
        }

        async fn get_tenant(
            &self,
            _: &SecurityContext,
            _: TenantId,
        ) -> Result<tenant_resolver_sdk::TenantInfo, TenantResolverError> {
            unimplemented!()
        }
        async fn get_root_tenant(
            &self,
            _: &SecurityContext,
        ) -> Result<tenant_resolver_sdk::TenantInfo, TenantResolverError> {
            unimplemented!()
        }
        async fn get_tenants(
            &self,
            _: &SecurityContext,
            _: &[TenantId],
            _: &GetTenantsOptions,
        ) -> Result<Vec<tenant_resolver_sdk::TenantInfo>, TenantResolverError> {
            unimplemented!()
        }
        async fn get_descendants(
            &self,
            _: &SecurityContext,
            _: TenantId,
            _: &GetDescendantsOptions,
        ) -> Result<GetDescendantsResponse, TenantResolverError> {
            unimplemented!()
        }
        async fn is_ancestor(
            &self,
            _: &SecurityContext,
            _: TenantId,
            _: TenantId,
            _: &IsAncestorOptions,
        ) -> Result<bool, TenantResolverError> {
            unimplemented!()
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // CountingProviderRepo — mock ProviderRepository whose first `list` call
    // succeeds and every later one fails, so a fan-out is observable
    // ═════════════════════════════════════════════════════════════════════════

    use std::sync::atomic::{AtomicUsize, Ordering};
    use toolkit_odata::PageInfo as OdataPageInfo;

    #[domain_model]
    struct CountingProviderRepo {
        call_count: AtomicUsize,
        list_all_count: AtomicUsize,
        /// Rows the chain-wide `list_all_for_tenant` hands back. Empty by
        /// default, which drives the eval listing down its empty-allow-list
        /// short-circuit.
        chain_providers: Vec<ProviderV1>,
    }

    impl CountingProviderRepo {
        fn new() -> Self {
            Self {
                call_count: AtomicUsize::new(0),
                list_all_count: AtomicUsize::new(0),
                chain_providers: Vec::new(),
            }
        }

        fn with_chain_providers(providers: Vec<ProviderV1>) -> Self {
            Self {
                chain_providers: providers,
                ..Self::new()
            }
        }
    }

    /// Build a `ProviderV1` in memory, for mocks that never touch the database.
    fn provider_v1(id: Uuid, tenant_id: Uuid, slug: &str) -> ProviderV1 {
        let now = chrono::Utc::now();
        ProviderV1 {
            id,
            tenant_id,
            slug: slug.to_owned(),
            name: slug.to_owned(),
            gts_type: gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~"),
            status: ProviderStatus::Active,
            managed: false,
            metadata: None,
            discovery_enabled: false,
            discovery_interval_seconds: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[async_trait]
    impl ProviderRepository for CountingProviderRepo {
        async fn find_by_id(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!("not used in list_providers test")
        }

        async fn find_all_by_slug(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _slug: &str,
        ) -> Result<Vec<ProviderV1>, DomainError> {
            unimplemented!("not used in list_providers test")
        }

        async fn list(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _query: &ODataQuery,
        ) -> Result<Page<ProviderV1>, DomainError> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                // First call (own tenant) succeeds with empty page
                Ok(Page {
                    items: vec![],
                    page_info: OdataPageInfo {
                        next_cursor: None,
                        prev_cursor: None,
                        limit: 20,
                    },
                })
            } else {
                Err(DomainError::internal("ancestor provider query failed"))
            }
        }

        async fn list_all_for_tenant(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
        ) -> Result<Vec<ProviderV1>, DomainError> {
            self.list_all_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.chain_providers.clone())
        }

        async fn find_by_ids(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _ids: &[Uuid],
        ) -> Result<Vec<ProviderV1>, DomainError> {
            Ok(vec![])
        }

        async fn create(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _tenant_id: Uuid,
            _req: &CreateProviderRequestV1,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!("not used in list_providers test")
        }

        async fn update(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
            _req: &UpdateProviderRequestV1,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!("not used in list_providers test")
        }

        async fn delete(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<(), DomainError> {
            unimplemented!("not used in list_providers test")
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Test helpers
    // ═════════════════════════════════════════════════════════════════════════

    fn child_tenant() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000010").unwrap()
    }

    fn test_tenant() -> Uuid {
        Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
    }

    fn other_tenant() -> Uuid {
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
    }

    fn scope_for(tenant_id: Uuid) -> AccessScope {
        AccessScope::for_tenants(vec![tenant_id])
    }

    fn make_create_req(slug: &str, name: &str) -> crate::CreateProviderRequestV1 {
        let gts = gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~");
        crate::CreateProviderRequestV1::builder(slug, name, gts).build()
    }

    fn make_create_model_req(
        provider_id: Uuid,
        provider_model_id: &str,
    ) -> crate::CreateModelRequestV1 {
        let gts_leaf = "cf.genai._.openai.v1~";
        let gts_type = format!("gts.cf.genai.model.info.v1~{gts_leaf}");

        let info = model_registry_sdk::ModelInfoV1 {
            gts_type: gts::GtsTypeId::new(&gts_type),
            display_name: format!("Test {provider_model_id}"),
            description: None,
            family: Some("test-family".to_owned()),
            vendor: Some("TestVendor".to_owned()),
            managed: false,
            architecture: Some("transformer".to_owned()),
            size_bytes: None,
            format: Some("api-only".to_owned()),
            region: None,
            hosted_by: None,
            last_release_at: None,
            reasoning_level: None,
            version: None,
            sort_order: None,
            icon: None,
            multiplier_display: None,
            performance: ModelPerformance {
                response_latency_ms: None,
                tokens_per_second: None,
            },
            additional_info: HashMap::new(),
            supported_api: HashSet::from([SupportedApi::Completion]),
            provider_model_id: provider_model_id.to_owned(),
            capabilities: ModelCapabilities {
                vision: MediaCapability {
                    enabled: true,
                    supported_mime_types: vec!["image/jpeg".to_owned()],
                },
                reasoning: ReasoningCapability {
                    effort: false,
                    toggle: false,
                    resume: false,
                    budget: false,
                },
                function_calling: true,
                response_schema: false,
                streaming: true,
                file_input: MediaCapability::default(),
                image_generation: MediaCapability::default(),
                audio_input: MediaCapability::default(),
                audio_output: MediaCapability::default(),
                code_interpreter: false,
                web_search: WebSearchCapability {
                    enabled: false,
                    allowed_domains: false,
                    excluded_domains: false,
                },
            },
            disabled_capabilities: DisabledCapabilities {
                vision: DisabledMediaCapability::default(),
                reasoning: DisabledReasoningCapability::default(),
                function_calling: false,
                response_schema: false,
                streaming: false,
                file_input: DisabledMediaCapability::default(),
                image_generation: DisabledMediaCapability::default(),
                audio_input: DisabledMediaCapability::default(),
                audio_output: DisabledMediaCapability::default(),
                code_interpreter: false,
                web_search: DisabledWebSearchCapability::default(),
            },
            context_window: ContextWindow {
                max_input_tokens: 8192,
                max_output_tokens: Some(4096),
                output_vector_size: None,
            },
            default_parameters: DefaultInferenceParametersV1::default(),
            allow_parameter_override: false,
            allow_extra_params: Vec::new(),
            provider_settings: serde_json::Value::Object(serde_json::Map::new()),
        };

        crate::CreateModelRequestV1 {
            provider_id,
            lifecycle_status: crate::LifecycleStatus::Production,
            approval_status: None,
            info,
        }
    }

    /// Set up an in-memory `SQLite` database with all migrations applied.
    async fn setup_db() -> DBProvider<DbError> {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts)
            .await
            .expect("in-memory SQLite connection");

        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("apply initial migration");

        DBProvider::<DbError>::new(db)
    }

    /// Create a test provider and return its ID and slug.
    async fn create_test_provider(
        repo: &ProviderRepositoryImpl,
        conn: &impl toolkit_db::secure::DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        slug: &str,
    ) -> (Uuid, String) {
        let p = crate::domain::repo::ProviderRepository::create(
            repo,
            conn,
            scope,
            tenant_id,
            &make_create_req(slug, slug),
        )
        .await
        .expect("create test provider");
        (p.id, p.slug)
    }

    /// Resolve a provider by slug for the model helpers below.
    ///
    /// The repository `create` takes an already-resolved provider — the service
    /// owns the ownership check — but a test names its provider by slug, so the
    /// helper does the lookup the production path no longer performs.
    /// `ProviderRepositoryImpl` is a zero-state unit struct, so building one
    /// here costs nothing.
    async fn resolve_test_provider(
        conn: &impl toolkit_db::secure::DBRunner,
        scope: &AccessScope,
        provider_slug: &str,
    ) -> ProviderV1 {
        crate::domain::repo::ProviderRepository::find_all_by_slug(
            &ProviderRepositoryImpl::default(),
            conn,
            scope,
            provider_slug,
        )
        .await
        .expect("test provider lookup succeeds")
        .pop()
        .expect("test provider exists")
    }

    /// Create a test model owned by `tenant_id`, returning the model.
    async fn create_test_model(
        repo: &ModelRepositoryImpl,
        conn: &impl toolkit_db::secure::DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        provider_slug: &str,
        provider_model_id: &str,
    ) -> crate::ModelV1 {
        let provider = resolve_test_provider(conn, scope, provider_slug).await;
        let req = make_create_model_req(provider.id, provider_model_id);
        crate::domain::repo::ModelRepository::create(repo, conn, scope, tenant_id, &provider, &req)
            .await
            .expect("create test model")
    }

    /// Create a test model with `Approved` approval status.
    async fn create_test_approved_model(
        repo: &ModelRepositoryImpl,
        conn: &impl toolkit_db::secure::DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        provider_slug: &str,
        provider_model_id: &str,
    ) -> crate::ModelV1 {
        let provider = resolve_test_provider(conn, scope, provider_slug).await;
        let mut req = make_create_model_req(provider.id, provider_model_id);
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        crate::domain::repo::ModelRepository::create(repo, conn, scope, tenant_id, &provider, &req)
            .await
            .expect("create test approved model")
    }

    /// Build a full `Service` instance for testing.
    ///
    /// Build a full `Service` instance for testing, using the provided cache.
    ///
    /// Sets up real DB + `ProviderRepositoryImpl` + `ModelRepositoryImpl` with
    /// mocks for tenant-resolver and authz-resolver.
    fn build_service_with_cache<R: TenantResolverClient + Send + Sync + 'static>(
        db: DBProvider<DbError>,
        tenant_resolver: R,
        config: ModelRegistryConfig,
        cache: Arc<dyn ResolutionCache>,
    ) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl> {
        let enforcer = PolicyEnforcer::new(Arc::new(MockAuthZ));
        Service {
            db: Arc::new(db),
            provider_repo: Arc::new(ProviderRepositoryImpl::default()),
            model_repo: Arc::new(ModelRepositoryImpl::default()),
            cache,
            tenant_resolver: Arc::new(tenant_resolver),
            policy_enforcer: enforcer,
            config,
        }
    }

    /// Build a `Service` whose PDP is an arbitrary `AuthZResolverClient`, for
    /// tests that need a scope wider than the caller's own tenant.
    fn build_service_with_authz<
        R: TenantResolverClient + Send + Sync + 'static,
        A: AuthZResolverClient + Send + Sync + 'static,
    >(
        db: DBProvider<DbError>,
        tenant_resolver: R,
        config: ModelRegistryConfig,
        cache: Arc<dyn ResolutionCache>,
        authz: A,
    ) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl> {
        Service {
            db: Arc::new(db),
            provider_repo: Arc::new(ProviderRepositoryImpl::default()),
            model_repo: Arc::new(ModelRepositoryImpl::default()),
            cache,
            tenant_resolver: Arc::new(tenant_resolver),
            policy_enforcer: PolicyEnforcer::new(Arc::new(authz)),
            config,
        }
    }

    /// Build a full `Service` instance whose chain is never served warm.
    fn build_service<R: TenantResolverClient + Send + Sync + 'static>(
        db: DBProvider<DbError>,
        tenant_resolver: R,
        config: ModelRegistryConfig,
    ) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl> {
        build_service_with_cache(db, tenant_resolver, config, Arc::new(NoopResolutionCache))
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Slug validation tests
    // ═════════════════════════════════════════════════════════════════════════

    type TestService = super::Service<(), ()>;

    #[test]
    fn test_validate_slug_valid() {
        assert!(TestService::validate_slug("openai").is_ok());
        assert!(TestService::validate_slug("my-provider-42").is_ok());
        assert!(TestService::validate_slug("a").is_ok());
        let long_slug = "a".repeat(64);
        assert!(TestService::validate_slug(&long_slug).is_ok());
    }

    #[test]
    fn test_validate_slug_empty() {
        let err = TestService::validate_slug("").unwrap_err();
        assert!(err.to_string().contains("slug cannot be empty"));
    }

    #[test]
    fn test_validate_slug_too_long() {
        let slug = "a".repeat(65);
        let err = TestService::validate_slug(&slug).unwrap_err();
        assert!(err.to_string().contains("at most 64 characters"));
    }

    #[test]
    fn test_validate_slug_invalid_chars() {
        let err = TestService::validate_slug("OpenAI").unwrap_err();
        assert!(err.to_string().contains("lowercase alphanumeric"));

        let err = TestService::validate_slug("open_ai").unwrap_err();
        assert!(err.to_string().contains("lowercase alphanumeric"));
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Name validation tests
    // ═════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_validate_name_valid() {
        // Free-form display text: mixed case, spaces and punctuation all pass.
        assert!(TestService::validate_name("OpenAI").is_ok());
        assert!(TestService::validate_name("Azure OpenAI (EU West)").is_ok());
        assert!(TestService::validate_name(&"a".repeat(255)).is_ok());
    }

    #[test]
    fn test_validate_name_empty() {
        let err = TestService::validate_name("").unwrap_err();
        assert!(err.to_string().contains("name cannot be empty"));
    }

    #[test]
    fn test_validate_name_too_long() {
        let err = TestService::validate_name(&"a".repeat(256)).unwrap_err();
        assert!(err.to_string().contains("at most 255 characters"));
    }

    #[test]
    fn test_validate_name_counts_characters_not_bytes() {
        // 255 multi-byte characters fit the column; 256 do not.
        assert!(TestService::validate_name(&"\u{e9}".repeat(255)).is_ok());
        assert!(TestService::validate_name(&"\u{e9}".repeat(256)).is_err());
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_providers — fail closed on ancestor query errors
    // ═════════════════════════════════════════════════════════════════════════

    /// A `ModelRepository` stub that panics on every method — used only in
    /// `list_providers` tests where `model_repo` methods are never called.
    #[domain_model]
    struct PanicModelRepo;

    #[async_trait]
    impl ModelRepository for PanicModelRepo {
        async fn find_by_id(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in list_providers tests")
        }

        async fn find_by_canonical(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _canonical_id: &str,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in list_providers tests")
        }

        async fn list(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _query: &ODataQuery,
            _visibility: ListVisibility<'_>,
        ) -> Result<Page<crate::ModelV1>, DomainError> {
            unimplemented!("not used in list_providers tests")
        }

        async fn create(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _tenant_id: Uuid,
            _provider: &ProviderV1,
            _req: &crate::CreateModelRequestV1,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in list_providers tests")
        }

        async fn update(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
            _req: &crate::UpdateModelRequestV1,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in list_providers tests")
        }

        async fn soft_delete(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<(), DomainError> {
            unimplemented!("not used in list_providers tests")
        }
    }

    /// A `ModelRepository` that counts `list` calls, so a test can pin the
    /// query count of the chain-wide listing.
    #[domain_model]
    struct CountingModelRepo {
        list_count: AtomicUsize,
    }

    impl CountingModelRepo {
        fn new() -> Self {
            Self {
                list_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ModelRepository for CountingModelRepo {
        async fn find_by_id(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in query-count tests")
        }

        async fn find_by_canonical(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _canonical_id: &str,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in query-count tests")
        }

        async fn list(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _query: &ODataQuery,
            _visibility: ListVisibility<'_>,
        ) -> Result<Page<crate::ModelV1>, DomainError> {
            self.list_count.fetch_add(1, Ordering::SeqCst);
            Ok(Page {
                items: vec![],
                page_info: OdataPageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: 20,
                },
            })
        }

        async fn create(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _tenant_id: Uuid,
            _provider: &ProviderV1,
            _req: &crate::CreateModelRequestV1,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in query-count tests")
        }

        async fn update(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
            _req: &crate::UpdateModelRequestV1,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in query-count tests")
        }

        async fn soft_delete(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<(), DomainError> {
            unimplemented!("not used in query-count tests")
        }
    }

    /// A `ModelRepository` whose `list` always fails, to pin the fail-fast
    /// behavior of the single chain-wide model query.
    #[domain_model]
    struct FailingModelListRepo;

    #[async_trait]
    impl ModelRepository for FailingModelListRepo {
        async fn find_by_id(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn find_by_canonical(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _canonical_id: &str,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn list(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _query: &ODataQuery,
            _visibility: ListVisibility<'_>,
        ) -> Result<Page<crate::ModelV1>, DomainError> {
            Err(DomainError::internal("model listing unavailable"))
        }

        async fn create(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _tenant_id: Uuid,
            _provider: &ProviderV1,
            _req: &crate::CreateModelRequestV1,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn update(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
            _req: &crate::UpdateModelRequestV1,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn soft_delete(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<(), DomainError> {
            unimplemented!("not used in failure tests")
        }
    }

    /// A `ProviderRepository` whose chain-wide query always fails, to pin the
    /// fail-closed behavior of provider resolution.
    #[domain_model]
    struct FailingListAllRepo;

    #[async_trait]
    impl ProviderRepository for FailingListAllRepo {
        async fn find_by_id(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn find_all_by_slug(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _slug: &str,
        ) -> Result<Vec<ProviderV1>, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn list(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _query: &ODataQuery,
        ) -> Result<Page<ProviderV1>, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn list_all_for_tenant(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
        ) -> Result<Vec<ProviderV1>, DomainError> {
            Err(DomainError::internal("provider set unavailable"))
        }

        async fn find_by_ids(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _ids: &[Uuid],
        ) -> Result<Vec<ProviderV1>, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn create(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _tenant_id: Uuid,
            _req: &CreateProviderRequestV1,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn update(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
            _req: &UpdateProviderRequestV1,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!("not used in failure tests")
        }

        async fn delete(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<(), DomainError> {
            unimplemented!("not used in failure tests")
        }
    }

    #[tokio::test]
    async fn test_list_providers_issues_one_query_and_never_fans_out() {
        // Given: a provider repo whose second and later `list` calls fail, two
        // ancestors, and no own providers.
        let db = setup_db().await;
        let provider_repo = Arc::new(CountingProviderRepo::new());
        let model_repo = Arc::new(PanicModelRepo);
        let tenant_resolver: Arc<dyn TenantResolverClient> = Arc::new(TwoAncestorsResolver);
        let enforcer = PolicyEnforcer::new(Arc::new(MockAuthZ));
        let config = ModelRegistryConfig::default();

        let service: Service<CountingProviderRepo, PanicModelRepo> = Service {
            db: Arc::new(db),
            provider_repo: Arc::clone(&provider_repo),
            model_repo,
            cache: Arc::new(NoopResolutionCache),
            tenant_resolver,
            policy_enforcer: enforcer,
            config,
        };

        // When: listing providers as a tenant that has two ancestors.
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tenant())
            .build()
            .expect("ctx");
        let page = service
            .list_providers(&ctx, &ODataQuery::default())
            .await
            .expect("the listing must not query any ancestor");

        // Then: exactly one query was issued, under the PDP scope alone.
        assert!(page.items.is_empty());
        assert_eq!(
            provider_repo.call_count.load(Ordering::SeqCst),
            1,
            "the admin listing must issue exactly one provider query"
        );
    }

    #[tokio::test]
    async fn test_list_tenant_models_returns_own_rows_when_ancestors_have_none() {
        // Given: a service with real repos and TwoAncestorsResolver where the
        // ancestors own no providers at all. The chain-wide query still resolves
        // and the child's own rows come back untouched.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let child_scope = scope_for(child_tid);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "openai").await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &child_scope,
            child_tid,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let page = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("a chain whose ancestors own nothing still lists own rows");

        // The child's own model should be visible regardless of ancestor data.
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — chain-wide query shape
    // ═════════════════════════════════════════════════════════════════════════

    /// No chain tenant owns an active winning provider, so nothing can match.
    /// The service must synthesize the empty page rather than issue a model
    /// query whose `provider_id IN ()` predicate has no portable rendering.
    #[tokio::test]
    async fn test_list_tenant_models_empty_allow_list_skips_the_model_query() {
        let db = setup_db().await;
        let provider_repo = Arc::new(CountingProviderRepo::new());

        // PanicModelRepo panics on any call, so reaching the model query fails
        // the test outright rather than silently passing.
        let service: Service<CountingProviderRepo, PanicModelRepo> = Service {
            db: Arc::new(db),
            provider_repo: Arc::clone(&provider_repo),
            model_repo: Arc::new(PanicModelRepo),
            cache: Arc::new(NoopResolutionCache),
            tenant_resolver: Arc::new(TwoAncestorsResolver),
            policy_enforcer: PolicyEnforcer::new(Arc::new(MockAuthZ)),
            config: ModelRegistryConfig::default(),
        };

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tenant())
            .build()
            .expect("ctx");
        let page = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("an empty allow-list must not error");

        assert!(page.items.is_empty());
        assert_eq!(
            page.page_info.limit,
            ModelRegistryConfig::default().page_limits().clamp(None),
            "the synthetic page carries the limit the repository would have resolved"
        );
    }

    /// The whole point of the change: one provider query and one model query,
    /// regardless of how deep the chain is.
    #[tokio::test]
    async fn test_list_tenant_models_issues_one_query_per_entity() {
        let db = setup_db().await;

        // One active provider owned by the caller, so the listing gets past the
        // empty-allow-list short-circuit and actually issues the model query.
        let provider_repo = Arc::new(CountingProviderRepo::with_chain_providers(vec![
            provider_v1(Uuid::new_v4(), child_tenant(), "openai"),
        ]));
        let model_repo = Arc::new(CountingModelRepo::new());

        let service: Service<CountingProviderRepo, CountingModelRepo> = Service {
            db: Arc::new(db),
            provider_repo: Arc::clone(&provider_repo),
            model_repo: Arc::clone(&model_repo),
            cache: Arc::new(NoopResolutionCache),
            tenant_resolver: Arc::new(TwoAncestorsResolver),
            policy_enforcer: PolicyEnforcer::new(Arc::new(MockAuthZ)),
            config: ModelRegistryConfig::default(),
        };

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tenant())
            .build()
            .expect("ctx");
        service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("list should succeed");

        assert_eq!(
            provider_repo.list_all_count.load(Ordering::SeqCst),
            1,
            "a 3-tenant chain must still cost exactly one provider query"
        );
        assert_eq!(
            model_repo.list_count.load(Ordering::SeqCst),
            1,
            "a 3-tenant chain must still cost exactly one model query"
        );
    }

    /// Fail-closed: a provider query that failed would un-shadow an ancestor,
    /// so the error propagates rather than narrowing the caller's view.
    #[tokio::test]
    async fn test_list_tenant_models_fails_closed_when_the_provider_query_fails() {
        let db = setup_db().await;

        let service: Service<FailingListAllRepo, PanicModelRepo> = Service {
            db: Arc::new(db),
            provider_repo: Arc::new(FailingListAllRepo),
            model_repo: Arc::new(PanicModelRepo),
            cache: Arc::new(NoopResolutionCache),
            tenant_resolver: Arc::new(TwoAncestorsResolver),
            policy_enforcer: PolicyEnforcer::new(Arc::new(MockAuthZ)),
            config: ModelRegistryConfig::default(),
        };

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tenant())
            .build()
            .expect("ctx");
        let err = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect_err("a failing provider query must fail the request");
        assert!(
            err.to_string().contains("provider set unavailable"),
            "expected the provider error to propagate, got: {err}"
        );
    }

    /// With one chain-wide query there is no partial outcome to degrade to: a
    /// failing model query fails the request rather than dropping an ancestor's
    /// rows silently, as the per-ancestor fan-out used to.
    #[tokio::test]
    async fn test_list_tenant_models_fails_when_the_model_query_fails() {
        let db = setup_db().await;
        let child_tid = child_tenant();
        {
            let conn = db.conn().expect("conn");
            let provider_repo = ProviderRepositoryImpl::default();
            let child_scope = scope_for(child_tid);
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "openai").await;
        }

        let service: Service<ProviderRepositoryImpl, FailingModelListRepo> = Service {
            db: Arc::new(db),
            provider_repo: Arc::new(ProviderRepositoryImpl::default()),
            model_repo: Arc::new(FailingModelListRepo),
            cache: Arc::new(NoopResolutionCache),
            tenant_resolver: Arc::new(TwoAncestorsResolver),
            policy_enforcer: PolicyEnforcer::new(Arc::new(MockAuthZ)),
            config: ModelRegistryConfig::default(),
        };

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let err = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect_err("a failing model query must fail the request");
        assert!(
            err.to_string().contains("model listing unavailable"),
            "expected the model error to propagate, got: {err}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — ordering and pagination across the chain
    // ═════════════════════════════════════════════════════════════════════════

    /// Child owns `child-co::b`; the parent owns `openai::a` and `openai::c`.
    /// Under the default `canonical_id asc` order the inherited rows must
    /// interleave with the own row, not follow it as a block.
    async fn seed_interleaved_chain(db: &DBProvider<DbError>) {
        let conn = db.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();

        let parent_tid = parent_id();
        let parent_scope = scope_for(parent_tid);
        create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;
        for model_id in ["a", "c"] {
            create_test_approved_model(
                &model_repo,
                &conn,
                &parent_scope,
                parent_tid,
                "openai",
                model_id,
            )
            .await;
        }

        let child_tid = child_tenant();
        let child_scope = scope_for(child_tid);
        create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "child-co").await;
        create_test_approved_model(&model_repo, &conn, &child_scope, child_tid, "child-co", "b")
            .await;
    }

    fn child_ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tenant())
            .build()
            .expect("ctx")
    }

    #[tokio::test]
    async fn test_list_tenant_models_orders_inherited_rows_inline_with_own_rows() {
        let db = setup_db().await;
        seed_interleaved_chain(&db).await;
        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let page = service
            .list_tenant_models(&child_ctx(), &ODataQuery::default())
            .await
            .expect("list should succeed");

        let ids: Vec<&str> = page.items.iter().map(|m| m.canonical_id.as_str()).collect();
        assert_eq!(
            ids,
            ["child-co::b", "openai::a", "openai::c"],
            "the merged set must be globally sorted, not own-rows-first"
        );
    }

    /// Paging across inherited rows was structurally impossible before: the
    /// cursor was nulled whenever any inherited row was present.
    #[tokio::test]
    async fn test_list_tenant_models_pages_across_inherited_rows() {
        let db = setup_db().await;
        seed_interleaved_chain(&db).await;
        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());
        let ctx = child_ctx();

        let first = service
            .list_tenant_models(
                &ctx,
                &ODataQuery {
                    limit: Some(2),
                    ..ODataQuery::default()
                },
            )
            .await
            .expect("first page");

        let first_ids: Vec<&str> = first
            .items
            .iter()
            .map(|m| m.canonical_id.as_str())
            .collect();
        assert_eq!(first_ids, ["child-co::b", "openai::a"]);
        let token = first
            .page_info
            .next_cursor
            .clone()
            .expect("inherited rows must not suppress the cursor");
        let cursor = toolkit_odata::CursorV1::decode(&token).expect("cursor decodes");

        let second = service
            .list_tenant_models(
                &ctx,
                &ODataQuery {
                    limit: Some(2),
                    cursor: Some(cursor),
                    ..ODataQuery::default()
                },
            )
            .await
            .expect("second page");

        let second_ids: Vec<&str> = second
            .items
            .iter()
            .map(|m| m.canonical_id.as_str())
            .collect();
        assert_eq!(
            second_ids,
            ["openai::c"],
            "the walk must continue past the inherited rows with no overlap"
        );
        assert!(
            second.page_info.next_cursor.is_none(),
            "the walk is complete"
        );
    }

    #[tokio::test]
    async fn test_list_tenant_models_honours_an_explicit_orderby_across_the_chain() {
        let db = setup_db().await;
        seed_interleaved_chain(&db).await;
        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let query = ODataQuery {
            order: toolkit_odata::ODataOrderBy(vec![toolkit_odata::OrderKey {
                field: "canonical_id".to_owned(),
                dir: toolkit_odata::SortDir::Desc,
            }]),
            ..ODataQuery::default()
        };
        let page = service
            .list_tenant_models(&child_ctx(), &query)
            .await
            .expect("list should succeed");

        let ids: Vec<&str> = page.items.iter().map(|m| m.canonical_id.as_str()).collect();
        assert_eq!(ids, ["openai::c", "openai::a", "child-co::b"]);
    }

    /// The PDP scope must keep binding own-tenant rows. A flat
    /// `for_tenants(chain)` scope would subsume it and silently stop enforcing
    /// every row-level constraint the PDP compiled.
    #[tokio::test]
    async fn test_list_tenant_models_keeps_pdp_constraints_on_own_rows() {
        let db = setup_db().await;
        let child_tid = child_tenant();
        let visible = {
            let conn = db.conn().expect("conn");
            let provider_repo = ProviderRepositoryImpl::default();
            let model_repo = ModelRepositoryImpl::default();

            // Parent owns one inheritable model.
            let parent_tid = parent_id();
            let parent_scope = scope_for(parent_tid);
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;
            create_test_approved_model(
                &model_repo,
                &conn,
                &parent_scope,
                parent_tid,
                "openai",
                "gpt-4o",
            )
            .await;

            // Child owns two, but the PDP grants only one of them by id.
            let child_scope = scope_for(child_tid);
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "child-co").await;
            let visible = create_test_approved_model(
                &model_repo,
                &conn,
                &child_scope,
                child_tid,
                "child-co",
                "a",
            )
            .await;
            create_test_approved_model(
                &model_repo,
                &conn,
                &child_scope,
                child_tid,
                "child-co",
                "z",
            )
            .await;
            visible
        };

        let service = build_service_with_authz(
            db,
            TwoAncestorsResolver,
            ModelRegistryConfig::default(),
            Arc::new(NoopResolutionCache),
            MockAuthZRowScoped {
                tenant_id: child_tid,
                resource_id: visible.id,
            },
        );

        let page = service
            .list_tenant_models(&child_ctx(), &ODataQuery::default())
            .await
            .expect("list should succeed");

        let ids: Vec<&str> = page.items.iter().map(|m| m.canonical_id.as_str()).collect();
        assert_eq!(
            ids,
            ["child-co::a", "openai::gpt-4o"],
            "the PDP's row constraint must still hide `child-co::z`, while the \
             inherited row is admitted by the ancestor branch"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — not found
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_not_found() {
        let db = setup_db().await;
        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(test_tenant())
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "nonexistent::model")
            .await
            .expect_err("nonexistent slug should fail");

        // With the rewritten slug-resolution path a canonical_id with an
        // unresolvable slug yields ProviderNotFoundBySlug (still 404 on the wire).
        assert!(
            matches!(&err, DomainError::ProviderNotFoundBySlug { slug } if slug == "nonexistent"),
            "expected ProviderNotFoundBySlug('nonexistent'), got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — deprecated model returns ModelDeprecated
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_deprecated_returns_error() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Soft-delete the model.
        crate::domain::repo::ModelRepository::soft_delete(&model_repo, &conn, &scope, model.id)
            .await
            .expect("soft delete");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("deprecated model should return ModelDeprecated");

        assert!(
            matches!(&err, DomainError::ModelDeprecated { canonical_id } if canonical_id == "openai::gpt-4o"),
            "expected ModelDeprecated, got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — sunset is terminal too
    // ═════════════════════════════════════════════════════════════════════════

    /// `Sunset` is the second terminal lifecycle state, and gate 2 must treat
    /// it exactly like `Deprecated`.
    #[tokio::test]
    async fn test_get_tenant_model_sunset_returns_deprecated_error() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_approved_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        crate::domain::repo::ModelRepository::update(
            &model_repo,
            &conn,
            &scope,
            model.id,
            &crate::UpdateModelRequestV1 {
                lifecycle_status: Some(crate::LifecycleStatus::Sunset),
                ..Default::default()
            },
        )
        .await
        .expect("sunset the model");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("a sunset model should return ModelDeprecated");

        assert!(
            matches!(&err, DomainError::ModelDeprecated { canonical_id } if canonical_id == "openai::gpt-4o"),
            "expected ModelDeprecated, got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — pending/rejected model returned with populated status
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_pending_returns_not_approved() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Create model with initial approved status
        let provider = resolve_test_provider(&conn, &scope, &provider_slug).await;
        let mut req = make_create_model_req(provider.id, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Pending);
        let _model = crate::domain::repo::ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider,
            &req,
        )
        .await
        .expect("create model with pending approval");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("a pending model must not resolve for eval");
        assert!(
            matches!(
                &err,
                DomainError::ModelNotApproved { canonical_id } if canonical_id == "openai::gpt-4o"
            ),
            "expected ModelNotApproved, got: {err:?}"
        );
    }

    /// Every non-`approved` status fails the gate, not just `pending`.
    #[tokio::test]
    async fn test_get_tenant_model_rejected_and_revoked_are_not_approved() {
        for status in [
            crate::ApprovalStatus::Rejected,
            crate::ApprovalStatus::Revoked,
        ] {
            let db = setup_db().await;
            let conn = db.conn().expect("conn");

            let provider_repo = ProviderRepositoryImpl::default();
            let model_repo = ModelRepositoryImpl::default();
            let tenant_id = test_tenant();
            let scope = scope_for(tenant_id);
            let (_provider_id, provider_slug) =
                create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

            let provider = resolve_test_provider(&conn, &scope, &provider_slug).await;
            let mut req = make_create_model_req(provider.id, "gpt-4o");
            req.approval_status = Some(status);
            crate::domain::repo::ModelRepository::create(
                &model_repo,
                &conn,
                &scope,
                tenant_id,
                &provider,
                &req,
            )
            .await
            .expect("create model");

            let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());
            let ctx = SecurityContext::builder()
                .subject_id(Uuid::new_v4())
                .subject_tenant_id(tenant_id)
                .build()
                .expect("ctx");

            let err = service
                .get_tenant_model(&ctx, "openai::gpt-4o")
                .await
                .expect_err("non-approved model must not resolve for eval");
            assert!(
                matches!(&err, DomainError::ModelNotApproved { .. }),
                "expected ModelNotApproved for {status:?}, got: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_get_tenant_model_approved_returns_with_approved_status() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        let provider = resolve_test_provider(&conn, &scope, &provider_slug).await;
        let mut req = make_create_model_req(provider.id, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        let _model = crate::domain::repo::ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider,
            &req,
        )
        .await
        .expect("create model with approved status");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(
            result.is_ok(),
            "approved model should be returned, got: {:?}",
            result.err()
        );

        let found = result.unwrap();
        assert_eq!(found.approval_status, crate::ApprovalStatus::Approved);
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — inherited model visible through the chain
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_inherited_from_parent() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        // Create provider and model in the parent tenant.
        let parent_scope = scope_for(parent_tid);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Child tenant should inherit parent's model.
        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(
            result.is_ok(),
            "inherited model should be found, got: {:?}",
            result.err()
        );

        let found = result.unwrap();
        assert_eq!(found.canonical_id, "openai::gpt-4o");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — basic listing
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_returns_own_models() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o-mini",
        )
        .await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let page = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("list should succeed");

        assert_eq!(page.items.len(), 2);
        let canonical_ids: Vec<&str> = page.items.iter().map(|m| m.canonical_id.as_str()).collect();
        assert!(canonical_ids.contains(&"openai::gpt-4o"));
        assert!(canonical_ids.contains(&"openai::gpt-4o-mini"));
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — excludes deprecated
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_excludes_deprecated() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;
        let mini = create_test_approved_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o-mini",
        )
        .await;

        // Soft-delete gpt-4o-mini
        crate::domain::repo::ModelRepository::soft_delete(&model_repo, &conn, &scope, mini.id)
            .await
            .expect("soft delete");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let page = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("list should succeed");

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — inherits ancestor models
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_inherits_from_ancestors() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        // Create provider and model in parent tenant.
        let parent_scope = scope_for(parent_tid);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let page = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("list should succeed");

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — child shadows ancestor by canonical_id
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_child_shadows_ancestor() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        // Create provider in both tenants.
        let child_scope = scope_for(child_tid);
        let parent_scope = scope_for(parent_tid);

        let (_child_provider_id, child_provider_slug) =
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "openai").await;
        let (_parent_provider_id, parent_provider_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;

        // Create model with same canonical_id in both tenants.
        create_test_approved_model(
            &model_repo,
            &conn,
            &child_scope,
            child_tid,
            &child_provider_slug,
            "gpt-4o",
        )
        .await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &parent_provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let page = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("list should succeed");

        // Should see exactly one "openai::gpt-4o" (child shadows parent).
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — nearer ancestor shadows farther ancestor
    // ═════════════════════════════════════════════════════════════════════════

    /// Two ancestors owning the same `canonical_id` must collapse to one row,
    /// attributed to the closer ancestor. The child owns nothing here, so the
    /// collision is resolved purely by chain distance.
    #[tokio::test]
    async fn test_list_tenant_models_parent_shadows_grandparent() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let parent_tid = parent_id();
        let grandparent_tid = grandparent_id();

        let parent_scope = scope_for(parent_tid);
        let grandparent_scope = scope_for(grandparent_tid);

        let (_parent_provider_id, parent_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &parent_slug,
            "gpt-4o",
        )
        .await;

        let (_grandparent_provider_id, grandparent_slug) = create_test_provider(
            &provider_repo,
            &conn,
            &grandparent_scope,
            grandparent_tid,
            "openai",
        )
        .await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &grandparent_scope,
            grandparent_tid,
            &grandparent_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let page = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("list should succeed");

        let ids: Vec<&str> = page.items.iter().map(|m| m.canonical_id.as_str()).collect();
        assert_eq!(ids, ["openai::gpt-4o"], "parent must shadow grandparent");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — respects OData limit
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_respects_odata_limit() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Create 3 models
        for i in 0..3 {
            create_test_approved_model(
                &model_repo,
                &conn,
                &scope,
                tenant_id,
                &provider_slug,
                &format!("model-{i}"),
            )
            .await;
        }

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let query = ODataQuery {
            limit: Some(2),
            ..Default::default()
        };
        let page = service
            .list_tenant_models(&ctx, &query)
            .await
            .expect("list with limit should succeed");

        assert_eq!(page.items.len(), 2);
    }

    // ═════════════════════════════════════════════════════════════════════════
    // listing — $select is rejected on both surfaces
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_listing_rejects_select() {
        let db = setup_db().await;
        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(test_tenant())
            .build()
            .expect("ctx");
        let query = ODataQuery::default().with_select(vec!["vendor".to_owned()]);

        let err = service
            .list_tenant_models(&ctx, &query)
            .await
            .expect_err("$select is unsupported on models");
        assert!(
            matches!(&err, DomainError::Validation { .. }),
            "expected a validation error, got {err:?}"
        );

        let err = service
            .list_providers(&ctx, &query)
            .await
            .expect_err("$select is unsupported on providers");
        assert!(
            matches!(&err, DomainError::Validation { .. }),
            "expected a validation error, got {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — tenant isolation
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_tenant_isolation() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();
        let scope_a = scope_for(tenant_a);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope_a, tenant_a, "openai").await;
        create_test_model(
            &model_repo,
            &conn,
            &scope_a,
            tenant_a,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Tenant B should see no models (no ancestors relationship).
        let service_a = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx_b = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_b)
            .build()
            .expect("ctx");
        let page = service_a
            .list_tenant_models(&ctx_b, &ODataQuery::default())
            .await
            .expect("list should succeed");

        assert!(
            page.items.is_empty(),
            "tenant B should see no models (no ancestor relationship)"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models_management — management endpoint tests
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_management_returns_only_scoped_rows() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        // Provider "openai" in parent and child; the child shadows the parent
        // on the eval path.
        let child_scope = scope_for(child_tid);
        let parent_scope = scope_for(parent_tid);

        let (_child_provider_id, child_slug) =
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "openai").await;
        let (_parent_provider_id, parent_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;

        create_test_approved_model(
            &model_repo,
            &conn,
            &child_scope,
            child_tid,
            &child_slug,
            "gpt-4o-child",
        )
        .await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &parent_slug,
            "gpt-4o-parent",
        )
        .await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");

        // 1. The admin listing is bounded by the PDP scope, which `MockAuthZ`
        //    pins to the caller's own tenant. The parent row is out of scope.
        let mgmt = service
            .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
            .await
            .expect("management list should succeed");

        assert_eq!(
            mgmt.items.len(),
            1,
            "admin listing must not reach the parent tenant"
        );
        assert_eq!(mgmt.items[0].model.canonical_id, "openai::gpt-4o-child");
        assert!(mgmt.items[0].available_for_eval);
        assert!(!mgmt.items[0].provider_disabled);

        // 2. The eval listing still resolves the chain and shadows the parent.
        let eval = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("eval list should succeed");

        assert_eq!(eval.items.len(), 1, "eval should only show child model");
        assert_eq!(eval.items[0].canonical_id, "openai::gpt-4o-child");
    }

    #[tokio::test]
    async fn test_list_tenant_models_management_spans_every_tenant_the_pdp_grants() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        let child_scope = scope_for(child_tid);
        let parent_scope = scope_for(parent_tid);

        let (_child_provider_id, child_slug) =
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "openai").await;
        let (_parent_provider_id, parent_slug) = create_test_provider(
            &provider_repo,
            &conn,
            &parent_scope,
            parent_tid,
            "anthropic",
        )
        .await;

        create_test_approved_model(
            &model_repo,
            &conn,
            &child_scope,
            child_tid,
            &child_slug,
            "gpt-4o",
        )
        .await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &parent_slug,
            "claude",
        )
        .await;

        // A PDP grant spanning both tenants — no ancestor resolution involved.
        let service = build_service_with_authz(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            Arc::new(NoopResolutionCache),
            MockAuthZTenants(vec![child_tid, parent_tid]),
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");

        let mgmt = service
            .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
            .await
            .expect("management list should succeed");

        let mut ids: Vec<&str> = mgmt
            .items
            .iter()
            .map(|r| r.model.canonical_id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["anthropic::claude", "openai::gpt-4o"]);
        assert!(
            mgmt.items.iter().all(|r| r.available_for_eval),
            "both providers are active and both models approved"
        );
    }

    #[tokio::test]
    async fn test_list_tenant_models_management_disabled_provider() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tid = test_tenant();
        let scope = scope_for(tid);

        // Create an active provider.
        let (provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tid, "openai").await;
        // Create a model under this provider.
        create_test_model(&model_repo, &conn, &scope, tid, &provider_slug, "gpt-4o").await;

        // Disable the provider via direct repo call.
        let _ = provider_repo
            .update(
                &conn,
                &scope,
                provider_id,
                &UpdateProviderRequestV1 {
                    name: None,
                    status: Some(ProviderStatus::Disabled),
                    managed: None,
                    metadata: None,
                    discovery_enabled: None,
                    discovery_interval_seconds: None,
                },
            )
            .await
            .expect("disable provider");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tid)
            .build()
            .expect("ctx");

        // 1. Management listing shows the model with provider_disabled=true.
        let mgmt = service
            .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
            .await
            .expect("management list should succeed");

        assert_eq!(
            mgmt.items.len(),
            1,
            "management should still show the model"
        );
        assert!(
            mgmt.items[0].provider_disabled,
            "model should be marked provider_disabled"
        );
        assert!(
            !mgmt.items[0].available_for_eval,
            "model should not be available for eval"
        );

        // 2. Eval listing should NOT show the model (provider is disabled).
        let eval = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("eval list should succeed");

        assert!(
            eval.items.is_empty(),
            "eval should not show models from disabled provider"
        );
    }

    #[tokio::test]
    async fn test_list_tenant_models_management_non_approved_model() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tid = test_tenant();
        let scope = scope_for(tid);

        // Create an active provider.
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tid, "openai").await;

        // Create a model with Pending approval status.
        let provider = resolve_test_provider(&conn, &scope, &provider_slug).await;
        let req = {
            let mut r = make_create_model_req(provider.id, "gpt-4o");
            r.approval_status = Some(ApprovalStatus::Pending);
            r
        };
        let _ = model_repo
            .create(&conn, &scope, tid, &provider, &req)
            .await
            .expect("create model");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tid)
            .build()
            .expect("ctx");

        // 1. Management listing shows the model with available_for_eval=false.
        let mgmt = service
            .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
            .await
            .expect("management list should succeed");

        assert_eq!(mgmt.items.len(), 1, "management should show the model");
        assert!(
            !mgmt.items[0].available_for_eval,
            "pending model should not be available for eval"
        );
        assert!(
            !mgmt.items[0].provider_disabled,
            "provider is active, not disabled"
        );

        // 2. The eval listing excludes it — the approval predicate is mandatory.
        let eval = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("eval list should succeed");

        assert!(
            eval.items.is_empty(),
            "eval must not return a non-approved model, got {} row(s)",
            eval.items.len()
        );

        // 3. …and a $filter naming approval_status cannot re-admit it.
        let parsed = toolkit_odata::parse_filter_string("approval_status eq 'pending'")
            .expect("parse filter");
        let query = ODataQuery {
            filter: Some(Box::new(parsed.into_expr())),
            ..Default::default()
        };
        let narrowed = service
            .list_tenant_models(&ctx, &query)
            .await
            .expect("eval list with filter should succeed");
        assert!(
            narrowed.items.is_empty(),
            "$filter must narrow within the approved set, never widen it"
        );
    }

    #[tokio::test]
    async fn test_list_tenant_models_management_include_deprecated() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tid = test_tenant();
        let scope = scope_for(tid);

        // Create an active provider.
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tid, "openai").await;

        // Create a model in production lifecycle.
        create_test_model(
            &model_repo,
            &conn,
            &scope,
            tid,
            &provider_slug,
            "gpt-4o-active",
        )
        .await;

        // Create a model with Deprecated lifecycle.
        let provider = resolve_test_provider(&conn, &scope, &provider_slug).await;
        let req = {
            let mut r = make_create_model_req(provider.id, "gpt-4o-deprecated");
            r.lifecycle_status = LifecycleStatus::Deprecated;
            r
        };
        let _ = model_repo
            .create(&conn, &scope, tid, &provider, &req)
            .await
            .expect("create deprecated model");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tid)
            .build()
            .expect("ctx");

        // 1. With include_deprecated=false (default): only the active model appears.
        let mgmt = service
            .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
            .await
            .expect("management list should succeed");

        assert_eq!(mgmt.items.len(), 1, "default should hide deprecated");
        assert_eq!(
            mgmt.items[0].model.canonical_id, "openai::gpt-4o-active",
            "only the active model should appear"
        );

        // 2. With include_deprecated=true: both models appear.
        let mgmt_with_dep = service
            .list_tenant_models_management(&ctx, &ODataQuery::default(), true)
            .await
            .expect("management list should succeed");

        assert_eq!(
            mgmt_with_dep.items.len(),
            2,
            "include_deprecated=true should include deprecated model"
        );
        let deprecated_row = mgmt_with_dep
            .items
            .iter()
            .find(|r| r.model.canonical_id.as_str() == "openai::gpt-4o-deprecated")
            .expect("deprecated model should be included");
        assert!(
            !deprecated_row.available_for_eval,
            "deprecated model should not be available for eval"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — cross-tenant is not found (no ancestor relationship)
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_cross_tenant_not_found() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();
        let scope_a = scope_for(tenant_a);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope_a, tenant_a, "openai").await;
        create_test_model(
            &model_repo,
            &conn,
            &scope_a,
            tenant_a,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx_b = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_b)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx_b, "openai::gpt-4o")
            .await
            .expect_err("cross-tenant should be not found");

        // With the rewritten slug-resolution path, a tenant with no provider
        // for the slug yields ProviderNotFoundBySlug (still 404 on the wire).
        assert!(
            matches!(&err, DomainError::ProviderNotFoundBySlug { slug } if slug == "openai"),
            "expected ProviderNotFoundBySlug('openai'), got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — slug resolution gates
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_malformed_canonical_id() {
        // A canonical_id without `::` should yield ModelNotFound.
        let db = setup_db().await;
        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(test_tenant())
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "no-separator")
            .await
            .expect_err("malformed id should fail");

        assert!(
            matches!(&err, DomainError::ModelNotFound { canonical_id } if canonical_id == "no-separator"),
            "expected ModelNotFound for malformed id, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_get_tenant_model_disabled_winner_provider() {
        // A winning provider that is disabled should yield ProviderDisabled.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let tenant_id = test_tenant();
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let scope = scope_for(tenant_id);

        // Create a provider and model, then disable the provider.
        let (provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Disable the provider via update.
        crate::domain::repo::ProviderRepository::update(
            &provider_repo,
            &conn,
            &scope,
            provider_id,
            &crate::UpdateProviderRequestV1 {
                status: Some(crate::ProviderStatus::Disabled),
                ..Default::default()
            },
        )
        .await
        .expect("disable provider");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("disabled provider should fail");

        assert!(
            matches!(&err, DomainError::ProviderDisabled { .. }),
            "expected ProviderDisabled for disabled winner, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_get_tenant_model_gate_order_lifecycle_before_provider_status() {
        // Gate order: terminal lifecycle check comes before provider
        // status check. A deprecated model on a disabled provider should yield
        // ModelDeprecated, not ProviderDisabled.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let tenant_id = test_tenant();
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let scope = scope_for(tenant_id);

        // Create a provider and model.
        let (provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Soft-delete the model (sets lifecycle to Deprecated).
        crate::domain::repo::ModelRepository::soft_delete(&model_repo, &conn, &scope, model.id)
            .await
            .expect("soft delete model");

        // Disable the provider.
        crate::domain::repo::ProviderRepository::update(
            &provider_repo,
            &conn,
            &scope,
            provider_id,
            &crate::UpdateProviderRequestV1 {
                status: Some(crate::ProviderStatus::Disabled),
                ..Default::default()
            },
        )
        .await
        .expect("disable provider");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("should fail at lifecycle gate before provider status");

        // Must be ModelDeprecated (gate 2 checked before gate 3).
        assert!(
            matches!(&err, DomainError::ModelDeprecated { canonical_id } if canonical_id == "openai::gpt-4o"),
            "expected ModelDeprecated (lifecycle gate fires before provider status), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_get_tenant_model_unresolved_slug() {
        // When no tenant in the chain owns the slug, return ProviderNotFoundBySlug.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        // Create a provider with slug "other-slug" so there IS a DB hit but
        // the slug "unknown" won't match.
        let provider_repo = ProviderRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        create_test_provider(&provider_repo, &conn, &scope, tenant_id, "other-slug").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "unknown::model")
            .await
            .expect_err("unresolvable slug should fail");

        assert!(
            matches!(&err, DomainError::ProviderNotFoundBySlug { slug } if slug == "unknown"),
            "expected ProviderNotFoundBySlug('unknown'), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_get_tenant_model_slug_resolved_from_parent() {
        // NoAncestorsResolver provided by the child test tenant, but slug
        // resolves in the single-tenant case — this tests the normal path
        // where slug resolution and model read work together.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let parent_tid = parent_id();
        let child_tid = child_tenant();

        // Create provider and model in parent tenant.
        let parent_scope = scope_for(parent_tid);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let result = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect("inherited model from parent should be found");

        assert_eq!(result.canonical_id, "openai::gpt-4o");
    }

    #[tokio::test]
    async fn test_get_tenant_model_child_shadows_slug() {
        // When both child and parent own the slug, the child wins and the
        // model is read from the child tenant only.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let parent_tid = parent_id();
        let child_scope = scope_for(child_tid);
        let parent_scope = scope_for(parent_tid);

        // Both child and parent have a provider with slug "openai".
        let (child_provider_id, child_slug) =
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "openai").await;
        let (_parent_provider_id, parent_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;

        // Child has a model, parent also has a model with the same canonical_id.
        create_test_approved_model(
            &model_repo,
            &conn,
            &child_scope,
            child_tid,
            &child_slug,
            "gpt-4o",
        )
        .await;
        create_test_approved_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &parent_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let result = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect("child's model should be found (child wins slug)");

        // The returned model should belong to the child's provider.
        assert_eq!(result.canonical_id, "openai::gpt-4o");
        assert_eq!(
            result.provider_id, child_provider_id,
            "child's model must be returned, not the parent's"
        );
    }

    #[domain_model]
    struct FailingSlugRepo {
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProviderRepository for FailingSlugRepo {
        async fn find_by_id(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!()
        }
        async fn find_all_by_slug(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _slug: &str,
        ) -> Result<Vec<ProviderV1>, DomainError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Err(DomainError::internal("slug resolution unavailable"))
        }
        async fn list(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _query: &ODataQuery,
        ) -> Result<Page<ProviderV1>, DomainError> {
            unimplemented!()
        }
        async fn list_all_for_tenant(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
        ) -> Result<Vec<ProviderV1>, DomainError> {
            unimplemented!()
        }
        async fn find_by_ids(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _ids: &[Uuid],
        ) -> Result<Vec<ProviderV1>, DomainError> {
            unimplemented!()
        }
        async fn create(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _tenant_id: Uuid,
            _req: &CreateProviderRequestV1,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!()
        }
        async fn update(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
            _req: &UpdateProviderRequestV1,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!()
        }
        async fn delete(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<(), DomainError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn test_get_tenant_model_fail_closed_on_slug_query_error() {
        // A non-not-found error during slug resolution must propagate.
        let call_count = Arc::new(AtomicUsize::new(0));
        let provider_repo = Arc::new(FailingSlugRepo {
            call_count: Arc::clone(&call_count),
        });
        let model_repo = Arc::new(PanicModelRepo);
        let enforcer = PolicyEnforcer::new(Arc::new(MockAuthZ));

        let service: Service<FailingSlugRepo, PanicModelRepo> = Service {
            db: Arc::new(setup_db().await),
            provider_repo,
            model_repo,
            cache: Arc::new(NoopResolutionCache),
            tenant_resolver: Arc::new(NoAncestorsResolver),
            policy_enforcer: enforcer,
            config: ModelRegistryConfig::default(),
        };

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(test_tenant())
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("slug resolution error must propagate");

        assert!(
            matches!(&err, DomainError::Internal { detail, .. } if detail.contains("slug resolution unavailable")),
            "expected Internal error from slug resolution, got: {err:?}"
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "slug resolution is a single query"
        );
    }

    /// The chain-wide slug query costs one round trip no matter how deep the
    /// chain is — the per-hop `find_by_slug` walk it replaced cost one per hop.
    #[tokio::test]
    async fn test_get_tenant_model_issues_one_provider_query_for_a_deep_chain() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let service: Service<FailingSlugRepo, PanicModelRepo> = Service {
            db: Arc::new(setup_db().await),
            provider_repo: Arc::new(FailingSlugRepo {
                call_count: Arc::clone(&call_count),
            }),
            model_repo: Arc::new(PanicModelRepo),
            cache: Arc::new(NoopResolutionCache),
            tenant_resolver: Arc::new(TwoAncestorsResolver),
            policy_enforcer: PolicyEnforcer::new(Arc::new(MockAuthZ)),
            config: ModelRegistryConfig::default(),
        };

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tenant())
            .build()
            .expect("ctx");
        service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("the stub repo always fails");

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "a 3-tenant chain must still cost exactly one provider query"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // create_model
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_create_model_success() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (provider_id, _provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let req = make_create_model_req(provider_id, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let model = service
            .create_model(&ctx, &req)
            .await
            .expect("create model");

        // canonical_id is derived server-side from the resolved provider's slug
        // plus the requested provider_model_id; the caller never supplied it.
        assert_eq!(model.canonical_id, "openai::gpt-4o");
        assert_eq!(model.provider_id, provider_id);
        assert_eq!(model.lifecycle_status, crate::LifecycleStatus::Production);
        assert_eq!(model.approval_status, crate::ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn test_create_model_with_initial_approval() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (provider_id, _provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let mut req = make_create_model_req(provider_id, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let model = service
            .create_model(&ctx, &req)
            .await
            .expect("create model");

        assert_eq!(model.approval_status, crate::ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn test_create_model_disabled_provider_rejected() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (provider_id, _provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Disable the provider via the repository.
        let update = crate::UpdateProviderRequestV1 {
            status: Some(crate::ProviderStatus::Disabled),
            ..Default::default()
        };
        provider_repo
            .update(&conn, &scope, provider_id, &update)
            .await
            .expect("disable provider");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let req = make_create_model_req(provider_id, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .create_model(&ctx, &req)
            .await
            .expect_err("disabled provider should be rejected");

        assert!(
            matches!(&err, DomainError::ProviderDisabled { .. }),
            "expected ProviderDisabled, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_create_model_deprecated_lifecycle_rejected() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (provider_id, _provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let mut req = make_create_model_req(provider_id, "gpt-4o");
        req.lifecycle_status = crate::LifecycleStatus::Deprecated;
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .create_model(&ctx, &req)
            .await
            .expect_err("terminal lifecycle should be rejected");

        assert!(
            matches!(&err, DomainError::Validation { .. }),
            "expected Validation error for terminal lifecycle, got: {err:?}"
        );

        // Also verify Sunset is rejected.
        req.lifecycle_status = crate::LifecycleStatus::Sunset;
        let err = service
            .create_model(&ctx, &req)
            .await
            .expect_err("Sunset lifecycle should also be rejected");

        assert!(
            matches!(&err, DomainError::Validation { .. }),
            "expected Validation error for Sunset lifecycle, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_create_model_ancestor_provider_out_of_scope_is_not_found() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let parent_tid = parent_id();
        let child_tid = child_tenant();

        // Provider exists in the parent tenant only.
        let parent_scope = scope_for(parent_tid);
        let (parent_provider_id, _provider_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;

        // `MockAuthZ` pins the scope to the caller's own tenant, so the parent's
        // provider is unreachable even though it is an ancestor.
        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let req = make_create_model_req(parent_provider_id, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let err = service
            .create_model(&ctx, &req)
            .await
            .expect_err("out-of-scope provider should be rejected");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { id } if *id == parent_provider_id),
            "expected ProviderNotFound({parent_provider_id}), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_create_model_takes_the_providers_tenant() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let parent_tid = parent_id();
        let child_tid = child_tenant();

        let parent_scope = scope_for(parent_tid);
        let (parent_provider_id, _provider_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;

        // A PDP grant covering both tenants; no ancestor resolution involved.
        let service = build_service_with_authz(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            Arc::new(NoopResolutionCache),
            MockAuthZTenants(vec![child_tid, parent_tid]),
        );

        let req = make_create_model_req(parent_provider_id, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let model = service
            .create_model(&ctx, &req)
            .await
            .expect("in-scope provider should be accepted");

        assert_eq!(
            model.tenant_id, parent_tid,
            "a model takes its provider's tenant, not the caller's"
        );
        assert_eq!(model.provider_id, parent_provider_id);
        assert_eq!(model.canonical_id, "openai::gpt-4o");
    }

    #[tokio::test]
    async fn test_create_model_provider_not_found() {
        let db = setup_db().await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let missing = Uuid::new_v4();
        let req = make_create_model_req(missing, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(test_tenant())
            .build()
            .expect("ctx");
        let err = service
            .create_model(&ctx, &req)
            .await
            .expect_err("nonexistent provider should fail");

        // 404 (not 403): the id resolves in no tenant of the chain, so there is
        // nothing to withhold.
        assert!(
            matches!(&err, DomainError::ProviderNotFound { id } if *id == missing),
            "expected ProviderNotFound({missing}), got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // update_model
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_update_model_lifecycle_status() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let updated = service
            .update_model(
                &ctx,
                model.id,
                &crate::UpdateModelRequestV1 {
                    lifecycle_status: Some(crate::LifecycleStatus::Preview),
                    ..Default::default()
                },
            )
            .await
            .expect("update lifecycle status");

        assert_eq!(updated.lifecycle_status, crate::LifecycleStatus::Preview);
        assert_eq!(updated.canonical_id, "openai::gpt-4o");
    }

    #[tokio::test]
    async fn test_update_model_approval_status() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");

        // Approve
        let updated = service
            .update_model(
                &ctx,
                model.id,
                &crate::UpdateModelRequestV1 {
                    approval_status: Some(crate::ApprovalStatus::Approved),
                    ..Default::default()
                },
            )
            .await
            .expect("approve model");
        assert_eq!(updated.approval_status, crate::ApprovalStatus::Approved);

        // Reject
        let updated = service
            .update_model(
                &ctx,
                model.id,
                &crate::UpdateModelRequestV1 {
                    approval_status: Some(crate::ApprovalStatus::Rejected),
                    ..Default::default()
                },
            )
            .await
            .expect("reject model");
        assert_eq!(updated.approval_status, crate::ApprovalStatus::Rejected);

        // Revoke
        let updated = service
            .update_model(
                &ctx,
                model.id,
                &crate::UpdateModelRequestV1 {
                    approval_status: Some(crate::ApprovalStatus::Revoked),
                    ..Default::default()
                },
            )
            .await
            .expect("revoke model");
        assert_eq!(updated.approval_status, crate::ApprovalStatus::Revoked);
    }

    #[tokio::test]
    async fn test_update_model_both_fields_and_approval() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let updated = service
            .update_model(
                &ctx,
                model.id,
                &crate::UpdateModelRequestV1 {
                    lifecycle_status: Some(crate::LifecycleStatus::Preview),
                    approval_status: Some(crate::ApprovalStatus::Approved),
                    ..Default::default()
                },
            )
            .await
            .expect("update both fields and approval");

        assert_eq!(updated.lifecycle_status, crate::LifecycleStatus::Preview);
        assert_eq!(updated.approval_status, crate::ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn test_update_model_not_found() {
        let db = setup_db().await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(test_tenant())
            .build()
            .expect("ctx");
        let missing = Uuid::new_v4();
        let err = service
            .update_model(
                &ctx,
                missing,
                &crate::UpdateModelRequestV1 {
                    lifecycle_status: Some(crate::LifecycleStatus::Preview),
                    ..Default::default()
                },
            )
            .await
            .expect_err("nonexistent model should fail");

        assert!(
            matches!(&err, DomainError::ModelNotFoundById { id } if *id == missing),
            "expected ModelNotFoundById({missing}), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_update_model_invalid_transition_from_deprecated() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Soft-delete (deprecate) the model manually via the repo.
        crate::domain::repo::ModelRepository::soft_delete(&model_repo, &conn, &scope, model.id)
            .await
            .expect("soft delete");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .update_model(
                &ctx,
                model.id,
                &crate::UpdateModelRequestV1 {
                    lifecycle_status: Some(crate::LifecycleStatus::Production),
                    ..Default::default()
                },
            )
            .await
            .expect_err("transition from deprecated should fail");

        assert!(
            matches!(&err, DomainError::InvalidTransition { .. }),
            "expected InvalidTransition, got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // delete_model
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_delete_model_soft_deletes() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        service
            .delete_model(&ctx, model.id)
            .await
            .expect("delete model");

        // After soft-delete, direct fetch should return ModelDeprecated.
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("deprecated model should return error");

        assert!(
            matches!(&err, DomainError::ModelDeprecated { .. }),
            "expected ModelDeprecated after soft-delete, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_delete_model_not_found() {
        let db = setup_db().await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(test_tenant())
            .build()
            .expect("ctx");
        let missing = Uuid::new_v4();
        let err = service
            .delete_model(&ctx, missing)
            .await
            .expect_err("nonexistent model should fail");

        assert!(
            matches!(&err, DomainError::ModelNotFoundById { id } if *id == missing),
            "expected ModelNotFoundById({missing}), got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Writes never touch the cache
    //
    // The only cached entity is a tenant's ancestor chain, which no provider or
    // model write can change. A counting stub proves it: not one cache
    // operation is issued on any write path, cross-tenant writes included.
    // ═════════════════════════════════════════════════════════════════════════

    /// A [`ResolutionCache`] that counts every operation it is asked to
    /// perform, and serves whatever it was seeded with.
    #[derive(Default)]
    struct CountingCache {
        entries: std::sync::Mutex<HashMap<Uuid, Vec<Uuid>>>,
        gets: AtomicUsize,
        puts: AtomicUsize,
    }

    impl CountingCache {
        fn gets(&self) -> usize {
            self.gets.load(Ordering::SeqCst)
        }

        fn puts(&self) -> usize {
            self.puts.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ResolutionCache for CountingCache {
        async fn get_chain(&self, tenant_id: Uuid) -> Option<Vec<Uuid>> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&tenant_id)
                .cloned()
        }

        async fn put_chain(&self, tenant_id: Uuid, chain: &[Uuid]) {
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(tenant_id, chain.to_vec());
        }
    }

    #[tokio::test]
    async fn test_provider_writes_do_not_touch_the_cache() {
        let db = setup_db().await;
        let tenant_id = test_tenant();
        let cache = Arc::new(CountingCache::default());
        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            Arc::clone(&cache) as Arc<dyn ResolutionCache>,
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");

        let created = service
            .create_provider(&ctx, &make_create_req("openai", "OpenAI"))
            .await
            .expect("create");
        service
            .update_provider(
                &ctx,
                created.id,
                &crate::UpdateProviderRequestV1 {
                    name: Some("renamed".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("update");
        service
            .delete_provider(&ctx, created.id)
            .await
            .expect("delete");

        assert_eq!(cache.gets(), 0, "a provider write reads no cache entry");
        assert_eq!(cache.puts(), 0, "a provider write writes no cache entry");
    }

    /// A cross-tenant write lands on a row the caller does not own. Under the
    /// old design that made *whose* prefix to invalidate load-bearing; now no
    /// prefix exists and the write must still leave the cache alone.
    #[tokio::test]
    async fn test_cross_tenant_provider_write_does_not_touch_the_cache() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");
        let owner_tenant = other_tenant();
        let caller_tenant = test_tenant();

        // The provider lives in `owner_tenant`, not in the caller's tenant.
        let provider_repo = ProviderRepositoryImpl::default();
        let (provider_id, _slug) = create_test_provider(
            &provider_repo,
            &conn,
            &scope_for(owner_tenant),
            owner_tenant,
            "openai",
        )
        .await;

        let cache = Arc::new(CountingCache::default());
        let service = build_service_with_authz(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            Arc::clone(&cache) as Arc<dyn ResolutionCache>,
            MockAuthZTenants(vec![caller_tenant, owner_tenant]),
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(caller_tenant)
            .build()
            .expect("ctx");

        let updated = service
            .update_provider(
                &ctx,
                provider_id,
                &crate::UpdateProviderRequestV1 {
                    name: Some("renamed".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("cross-tenant update should be permitted by this PDP");
        assert_eq!(
            updated.tenant_id, owner_tenant,
            "the updated row belongs to the owner tenant, not the caller"
        );

        service
            .delete_provider(&ctx, provider_id)
            .await
            .expect("cross-tenant delete should be permitted by this PDP");

        assert_eq!(cache.gets(), 0);
        assert_eq!(cache.puts(), 0);
    }

    #[tokio::test]
    async fn test_model_writes_do_not_touch_the_cache() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let provider_repo = ProviderRepositoryImpl::default();
        let (provider_id, _provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        let cache = Arc::new(CountingCache::default());
        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            Arc::clone(&cache) as Arc<dyn ResolutionCache>,
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");

        let created = service
            .create_model(&ctx, &make_create_model_req(provider_id, "gpt-4o"))
            .await
            .expect("create");
        service
            .update_model(
                &ctx,
                created.id,
                &crate::UpdateModelRequestV1 {
                    approval_status: Some(crate::ApprovalStatus::Approved),
                    ..Default::default()
                },
            )
            .await
            .expect("update");
        service
            .delete_model(&ctx, created.id)
            .await
            .expect("delete");

        assert_eq!(cache.gets(), 0, "a model write reads no cache entry");
        assert_eq!(cache.puts(), 0, "a model write writes no cache entry");
    }

    /// The property the old row-caching design could not offer: an approval
    /// flip is visible to the very next read, with no invalidation step.
    #[tokio::test]
    async fn test_approval_flip_is_visible_on_the_next_read() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let created = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // A warm chain, so the read path is exercised with the cache in play.
        let cache = Arc::new(CountingCache::default());
        cache.put_chain(tenant_id, &[]).await;
        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            Arc::clone(&cache) as Arc<dyn ResolutionCache>,
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");

        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("a pending model is not readable");
        assert!(
            matches!(&err, DomainError::ModelNotApproved { .. }),
            "expected ModelNotApproved, got: {err:?}"
        );

        service
            .update_model(
                &ctx,
                created.id,
                &crate::UpdateModelRequestV1 {
                    approval_status: Some(crate::ApprovalStatus::Approved),
                    ..Default::default()
                },
            )
            .await
            .expect("approve");

        let model = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect("the approval is visible immediately");
        assert_eq!(model.approval_status, crate::ApprovalStatus::Approved);
    }
}
