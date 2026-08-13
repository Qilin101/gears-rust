//! Application service for the Model Registry gear.
//!
//! Orchestrates authorization, caching, inheritance resolution, and
//! persistence for providers and models. Generic over the two repository
//! traits and the cache backend so unit tests can inject mocks.
//!
//! ## Provider operations (Task 11)
//!
//! All five provider CRUD methods with:
//! - Authz via [`PolicyEnforcer`]
//! - Cache read-through for reads / invalidation on writes
//! - Inheritance resolution for reads (ancestor tenant visibility)
//! - Slug format validation on create
//!
//! ## Model operations (Tasks 12-13)
//!
//! Stubs only — placeholders until the model-read and model-CRUD tasks.

use std::sync::Arc;

use authz_resolver_sdk::pep::{PolicyEnforcer, ResourceType};
use tenant_resolver_sdk::TenantResolverClient;
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::cache::{CacheService, SlugOwnership, cache_key};
use super::error::DomainError;
use super::inheritance::{
    AncestorFailure, build_chain_providers, find_in_chain, merge_inherited_page, resolve_ancestors,
};
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
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Application service orchestrating authorization, caching, inheritance
/// resolution, and persistence.
///
/// Generic over:
/// - `R`: repository implementing [`ProviderRepository`]
/// - `M`: repository implementing [`ModelRepository`]
/// - `C`: cache backend implementing [`CacheService`]
#[domain_model]
pub struct Service<R, M, C> {
    db: Arc<DBProvider<toolkit_db::DbError>>,
    provider_repo: Arc<R>,
    model_repo: Arc<M>,
    cache: Arc<C>,
    tenant_resolver: Arc<dyn TenantResolverClient>,
    policy_enforcer: PolicyEnforcer,
    config: ModelRegistryConfig,
}

impl<R: ProviderRepository, M: ModelRepository, C: CacheService> Service<R, M, C> {
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
        cache: Arc<C>,
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
    async fn derive_access_scope(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, resource, action, None)
            .await?)
    }

    // ── Provider operations ──────────────────────────────────────────────

    /// Get a provider by ID with cache-first lookup and inheritance resolution.
    ///
    /// Searches the own tenant first (cache then DB), then falls back to each
    /// ancestor tenant. The closest match wins (child shadows parent). Results
    /// are cached with TTL appropriate to the ownership classification.
    pub async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderV1, DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let own_scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::GET)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;

        // 3. Try cache for each tenant in chain (closest first). Provider ids
        //    are globally unique and entries are keyed under the owning tenant,
        //    so at most one of these keys can exist.
        for tenant_id in inheritance.chain_ids() {
            let key = cache_key(tenant_id, "provider", &id.to_string());
            if let Some(provider) = self.cache.get::<ProviderV1>(&key).await {
                return Ok(provider);
            }
        }

        // 4. Cache miss — walk the chain in the DB, closest tenant first.
        let conn = self.db.conn().map_err(DomainError::from)?;
        let conn = &conn;
        let found = find_in_chain(
            &inheritance,
            &own_scope,
            |e| matches!(e, DomainError::ProviderNotFound { .. }),
            |scope| async move { self.provider_repo.find_by_id(conn, &scope, id).await },
        )
        .await?;

        let Some((owner_tenant_id, provider)) = found else {
            return Err(DomainError::provider_not_found(id));
        };

        // 5. Cache under the owning tenant.
        let key = cache_key(&owner_tenant_id, "provider", &id.to_string());
        self.cache
            .set(&key, &provider, self.config.cache_ttl_seconds)
            .await;

        Ok(provider)
    }

    /// List providers visible to the caller's tenant with `OData` filtering.
    ///
    /// Returns providers from the own tenant (with full `OData` support) merged
    /// with providers inherited from ancestor tenants. Child-tenant providers
    /// shadow ancestor providers with the same slug.
    pub async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, DomainError> {
        reject_select(query)?;

        // 1. Derive access scope (authorization check + DB scope)
        let own_scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::LIST)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;

        // 3. Get own tenant providers with OData
        let own_page = self.provider_repo.list(&conn, &own_scope, query).await?;

        // 4. Merge the inherited set, shadowing ancestor providers by slug.
        //    Fail closed on ancestor query errors (B5): skipping an ancestor
        //    provider row would un-shadow an ancestor and widen the caller's view.
        let conn = &conn;
        merge_inherited_page(
            &inheritance,
            own_page,
            query,
            |p| p.slug.clone(),
            AncestorFailure::FailClosed,
            |_tenant_id, scope, ancestor_query| async move {
                Some(self.provider_repo.list(conn, &scope, &ancestor_query).await)
            },
        )
        .await
    }

    /// Create a new provider.
    ///
    /// Validates slug format, checks authorization, delegates to the repository,
    /// and invalidates the own-tenant cache on success.
    pub async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: &CreateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError> {
        // 1. Validate slug format and discovery interval
        Self::validate_slug(req.slug())?;
        Self::validate_discovery_interval(req.discovery_interval_seconds())?;

        // 2. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::CREATE)
            .await?;

        // 3. Create via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let tenant_id = ctx.subject_tenant_id();
        let provider = self
            .provider_repo
            .create(&conn, &scope, tenant_id, req)
            .await?;

        // 4. Invalidate the owning tenant's cache. Equal to the caller's tenant
        //    here by construction — `create` stamps the row with it — but read
        //    from the row so all six write paths invalidate the same way.
        self.cache.invalidate_tenant(provider.tenant_id).await;

        Ok(provider)
    }

    /// Update a provider (PATCH semantics).
    ///
    /// Slug is immutable — the repository rejects changes to it. Cache is
    /// invalidated on success.
    pub async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: &UpdateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError> {
        // 1. Validate discovery interval (if being updated)
        Self::validate_discovery_interval(req.discovery_interval_seconds.flatten())?;

        // 2. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::UPDATE)
            .await?;

        // 3. Update via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let provider = self.provider_repo.update(&conn, &scope, id, req).await?;

        // 4. Invalidate the cache of the tenant that owns the row, which is not
        //    necessarily the caller's: the PDP scope may permit writes across a
        //    subtree, and cache keys are prefixed by the owning tenant.
        self.cache.invalidate_tenant(provider.tenant_id).await;

        Ok(provider)
    }

    /// Delete a provider.
    ///
    /// Removes the provider and invalidates the own-tenant cache.
    pub async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::DELETE)
            .await?;

        // 2. Delete via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let deleted = self.provider_repo.delete(&conn, &scope, id).await?;

        // 3. Invalidate the owning tenant's cache (see `update_provider`).
        self.cache.invalidate_tenant(deleted.tenant_id).await;

        Ok(())
    }
}

// ── Validation (no trait bounds) ─────────────────────────────────────

impl<R, M, C> Service<R, M, C> {
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

// ── Model read operations (Task 12) ────────────────────────────────────

impl<R: ProviderRepository, M: ModelRepository, C: CacheService> Service<R, M, C> {
    /// Resolve a single `(tenant_id, slug)` hop, cache-first with tombstones.
    ///
    /// Returns:
    /// - `Ok(SlugOwnership::Owned(p))` when the tenant owns a provider with this slug
    /// - `Ok(SlugOwnership::None)` when the tenant has no provider with this slug
    ///   (a tombstone from a prior DB miss, so the caller skips a round-trip)
    /// - `Err(e)` on any non-not-found query error (fail-closed — a skipped
    ///   ancestor hop would un-shadow an earlier ancestor, B5)
    ///
    /// Both polarities take the same TTL.
    async fn resolve_slug_ownership(
        &self,
        conn: &impl toolkit_db::secure::DBRunner,
        tenant_id: Uuid,
        slug: &str,
    ) -> Result<SlugOwnership, DomainError> {
        let key = cache_key(&tenant_id, "provider_slug", slug);
        let ttl = self.config.cache_ttl_seconds;

        // Cache-first.
        if let Some(result) = self.cache.get::<SlugOwnership>(&key).await {
            return Ok(result);
        }

        // Cache miss — query DB.
        let scope = AccessScope::for_tenant(tenant_id);
        match self.provider_repo.find_by_slug(conn, &scope, slug).await {
            Ok(provider) => {
                let result = SlugOwnership::Owned(provider);
                self.cache.set(&key, &result, ttl).await;
                Ok(result)
            }
            Err(DomainError::ProviderNotFoundBySlug { .. }) => {
                // Write a tombstone so the next lookup skips the DB round-trip.
                self.cache.set(&key, &SlugOwnership::None, ttl).await;
                Ok(SlugOwnership::None)
            }
            Err(e) => Err(e),
        }
    }

    /// Get a model by canonical ID via slug resolution (DESIGN §3.5).
    ///
    /// Resolves the provider slug from `canonical_id` (the segment before `::`)
    /// closest-first across the tenant chain, then reads the model only from
    /// the winning tenant. Applies gates in order (C2, C4):
    ///
    /// 1. `provider_id` mismatch → `ModelNotFound` (stale cache row)
    /// 2. Terminal lifecycle → `ModelDeprecated`
    /// 3. Disabled provider → `ProviderDisabled`
    ///
    /// Approval is **reported, not enforced** — `ModelNotApproved` stays
    /// unreachable from this path.
    ///
    /// A malformed `canonical_id` (no `::` separator) yields `ModelNotFound` (C5).
    /// An unresolved slug yields `ProviderNotFoundBySlug` (C3).
    ///
    /// Slug resolution is **fail-closed**: any non-not-found query error at any
    /// chain hop propagates as `Internal` (B5).
    pub async fn get_tenant_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<crate::ModelV1, DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let own_scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::GET)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;

        // 3. Split canonical_id on the first `::` to get slug and model id (C5).
        let slug = canonical_id.split_once("::").map(|(s, _)| s);
        let Some(slug) = slug else {
            return Err(DomainError::model_not_found(canonical_id));
        };

        // 4. Resolve the slug closest-first via the Task 6 cache-first helper.
        //    Stop at the first owner. Fail-closed on non-not-found errors (B5).
        let conn = self.db.conn().map_err(DomainError::from)?;
        let conn = &conn;

        let mut winner_tenant: Option<Uuid> = None;
        let mut winner_provider: Option<crate::ProviderV1> = None;

        for tenant_id in inheritance.chain_ids() {
            match self.resolve_slug_ownership(conn, *tenant_id, slug).await {
                Ok(SlugOwnership::Owned(provider)) => {
                    winner_tenant = Some(*tenant_id);
                    winner_provider = Some(provider);
                    break;
                }
                Ok(SlugOwnership::None) => {
                    // Tombstone — no provider with this slug in this tenant.
                }
                Err(e) => {
                    // Fail-closed: a skipped ancestor provider query would
                    // un-shadow an earlier ancestor (B5).
                    return Err(e);
                }
            }
        }

        let Some(winner) = winner_provider else {
            return Err(DomainError::provider_not_found_by_slug(slug));
        };
        let Some(winner_tenant_id) = winner_tenant else {
            return Err(DomainError::provider_not_found_by_slug(slug));
        };

        // 5. Scope the model read (C1):
        //    - Winner is own tenant → use the PDP-derived own_scope (preserves
        //      compiled constraints).
        //    - Winner is an ancestor → construct a scoped scope.
        let model_scope = if winner_tenant_id == inheritance.tenant_id() {
            own_scope
        } else {
            AccessScope::for_tenant(winner_tenant_id)
        };

        // 6. Read the model: cache under winner's tenant, then DB.
        let model_key = cache_key(&winner_tenant_id, "model", canonical_id);
        let model = if let Some(model) = self.cache.get::<crate::ModelV1>(&model_key).await {
            model
        } else {
            match self
                .model_repo
                .find_by_canonical(conn, &model_scope, canonical_id)
                .await
            {
                Ok(model) => {
                    self.cache
                        .set(&model_key, &model, self.config.cache_ttl_seconds)
                        .await;
                    model
                }
                Err(DomainError::ModelNotFound { .. }) => {
                    return Err(DomainError::model_not_found(canonical_id));
                }
                Err(e) => return Err(e),
            }
        };

        // 7. Apply gates in order (C2, C4).
        //    Gate 1: provider_id must match the winning provider.
        if model.provider_id != winner.id {
            // Stale cache row or inconsistent data — do not serve.
            self.cache.delete(&model_key).await;
            return Err(DomainError::model_not_found(canonical_id));
        }

        //    Gate 2: terminal lifecycle → ModelDeprecated.
        //    NOTE: We keep the cached entry even though the model is deprecated
        //    — the state is terminal, so re-fetching from DB every time just to
        //    return the same error wastes a round-trip.
        if matches!(
            model.lifecycle_status,
            crate::LifecycleStatus::Deprecated | crate::LifecycleStatus::Sunset
        ) {
            return Err(DomainError::model_deprecated(canonical_id));
        }

        //    Gate 3: disabled winning provider → ProviderDisabled.
        if !matches!(winner.status, crate::ProviderStatus::Active) {
            self.cache.delete(&model_key).await;
            return Err(DomainError::provider_disabled(winner.id));
        }

        // 8. Approval is reported, not enforced (see doc comment).
        Ok(model)
    }

    /// List models with `OData` filtering and inheritance.
    ///
    /// Returns models from the own tenant (with full `OData` support) merged with
    /// models inherited from ancestor tenants. Child-tenant models shadow
    /// ancestor models with the same `canonical_id`. Deprecated models are
    /// excluded from the eval list (filtered by the repository layer).
    ///
    /// The eval path builds `ChainProviders(T0)` and queries each chain tenant
    /// with `ListVisibility::Eval`, passing only the winning active provider ids
    /// for that tenant. Ancestors whose allow-list slice is empty are skipped
    /// (B3). The `canonical_id` dedupe in `merge_inherited_page` is kept as a
    /// redundant safety net (B1).
    pub async fn list_tenant_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<crate::ModelV1>, DomainError> {
        reject_select(query)?;

        // 1. Derive access scope (authorization check + DB scope)
        let own_scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::LIST)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;

        // 3. Build ChainProviders(T0) — fail closed on any ancestor provider
        //    query error (B5). A skipped ancestor would un-shadow an earlier one.
        let conn = &conn;
        let chain = build_chain_providers(&inheritance, |scope| async move {
            self.provider_repo.list_all_for_tenant(conn, &scope).await
        })
        .await?;

        // 4. Get own-tenant models with ListVisibility::Eval.
        //    Skip own tenant when its allow-list slice is empty (B3) —
        //    synthesize an empty page rather than querying with an empty list.
        let own_slice = chain.allow_slice_for(inheritance.tenant_id());
        let own_page = if own_slice.is_empty() {
            Page {
                items: vec![],
                page_info: toolkit_odata::PageInfo {
                    // The same effective limit the repository would have
                    // resolved — `merge_inherited_page` truncates the inherited
                    // rows to it, so reporting 0 here would empty the page.
                    limit: self.config.page_limits().clamp(query.limit),
                    next_cursor: None,
                    prev_cursor: None,
                },
            }
        } else {
            self.model_repo
                .list(
                    conn,
                    &own_scope,
                    query,
                    ListVisibility::Eval {
                        allow_list: &own_slice,
                    },
                )
                .await?
        };

        // 5. Merge the inherited set, shadowing ancestor models by canonical_id.
        //    Skip ancestor query errors (model queries narrow, never widen).
        //    Inside the closure, check each tenant's allow-list slice and
        //    return None to skip tenants with nothing to contribute.
        merge_inherited_page(
            &inheritance,
            own_page,
            query,
            |m| m.canonical_id.clone(),
            AncestorFailure::Skip,
            |tenant_id, scope, ancestor_query| {
                let slice = chain.allow_slice_for(tenant_id);
                async move {
                    if slice.is_empty() {
                        None
                    } else {
                        Some(
                            self.model_repo
                                .list(
                                    conn,
                                    &scope,
                                    &ancestor_query,
                                    ListVisibility::Eval { allow_list: &slice },
                                )
                                .await,
                        )
                    }
                }
            },
        )
        .await
    }

    /// List models with management flags for the admin endpoint.
    ///
    /// Builds `ChainProviders(T0)` (fail-closed), then queries every chain tenant
    /// with `ListVisibility::Management` and merges in chain order **without**
    /// `canonical_id` dedupe — two chain tenants owning the same slug is the
    /// exact case this endpoint exists to display.
    ///
    /// Each returned row carries `shadowed`, `provider_disabled`, and
    /// `available_for_eval` flags computed from the same `ChainProviders`.
    pub async fn list_tenant_models_management(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        include_deprecated: bool,
    ) -> Result<Page<crate::ModelManagementV1>, DomainError> {
        reject_select(query)?;

        // 1. Derive access scope (authorization check + DB scope).
        let own_scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::LIST_MANAGEMENT)
            .await?;

        // 2. Resolve ancestor chain.
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;

        // 3. Build ChainProviders(T0) — fail closed on any ancestor provider
        //    query error.
        let conn = &conn;
        let chain = build_chain_providers(&inheritance, |scope| async move {
            self.provider_repo.list_all_for_tenant(conn, &scope).await
        })
        .await?;

        // 4. Get own-tenant models with ListVisibility::Management.
        let own_page = self
            .model_repo
            .list(
                conn,
                &own_scope,
                query,
                ListVisibility::Management { include_deprecated },
            )
            .await?;

        // 5. Merge inherited models in chain order WITHOUT canonical_id dedupe.
        //    Pass key_fn = |m| m.id so that apply_additive_visibility collapses
        //    nothing (model ids are unique) while chain ordering is preserved.
        let merged = merge_inherited_page(
            &inheritance,
            own_page,
            query,
            |m| m.id,
            AncestorFailure::Skip,
            |_tenant_id, scope, ancestor_query| async move {
                Some(
                    self.model_repo
                        .list(
                            conn,
                            &scope,
                            &ancestor_query,
                            ListVisibility::Management { include_deprecated },
                        )
                        .await,
                )
            },
        )
        .await?;

        // 6. Annotate each row with management flags from ChainProviders.
        let items: Vec<crate::ModelManagementV1> = merged
            .items
            .into_iter()
            .map(|model| {
                let provider = chain.get(model.provider_id);
                let shadowed = provider.is_some_and(|p| !p.winner);
                let provider_disabled =
                    provider.is_some_and(|p| p.status != ProviderStatus::Active);
                let available_for_eval = provider
                    .is_some_and(|p| p.winner && p.status == ProviderStatus::Active)
                    && !matches!(
                        model.lifecycle_status,
                        LifecycleStatus::Deprecated | LifecycleStatus::Sunset
                    )
                    && matches!(model.approval_status, ApprovalStatus::Approved);

                ModelManagementV1 {
                    model,
                    shadowed,
                    provider_disabled,
                    available_for_eval,
                }
            })
            .collect();

        Ok(Page {
            items,
            page_info: merged.page_info,
        })
    }

    /// Create a new model.
    ///
    /// Validates the provider slug, checks that the provider exists (own tenant
    /// or inherited from an ancestor), derives `canonical_id` from
    /// `provider_slug::info.provider_model_id`, writes the initial approval
    /// status (defaults to `Pending`), and invalidates the own-tenant cache.
    pub async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: &crate::CreateModelRequestV1,
    ) -> Result<crate::ModelV1, DomainError> {
        // 1. Validate provider_slug format
        Self::validate_slug(&req.provider_slug)?;

        // 1b. Reject models created directly in a terminal lifecycle state.
        if matches!(
            req.lifecycle_status,
            crate::LifecycleStatus::Deprecated | crate::LifecycleStatus::Sunset
        ) {
            return Err(DomainError::validation(format!(
                "cannot create a model with terminal lifecycle status `{:?}`",
                req.lifecycle_status,
            )));
        }

        // 2. Verify authorization
        let tenant_id = ctx.subject_tenant_id();
        self.derive_access_scope(ctx, &MODEL_RESOURCE, actions::CREATE)
            .await?;

        // 3. Resolve the provider **own-tenant-only** (E1). If the slug exists
        //    only in an ancestor tenant, the caller does not own it and may not
        //    create models against it (§3.1 Invariants).
        let conn = self.db.conn().map_err(DomainError::from)?;
        let conn = &conn;

        let own_scope = AccessScope::for_tenant(tenant_id);
        let provider = match self
            .provider_repo
            .find_by_slug(conn, &own_scope, &req.provider_slug)
            .await
        {
            Ok(provider) => provider,
            Err(e @ DomainError::ProviderNotFoundBySlug { .. }) => {
                // The slug does not exist in the caller's tenant. Check ancestors
                // to decide whether it is "not found anywhere" (E3 → 404) or
                // "found in an ancestor" (E1/E2 → 403).
                let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;
                match find_in_chain(
                    &inheritance,
                    &own_scope,
                    |e| matches!(e, DomainError::ProviderNotFoundBySlug { .. }),
                    |scope| async move {
                        self.provider_repo
                            .find_by_slug(conn, &scope, &req.provider_slug)
                            .await
                    },
                )
                .await?
                {
                    Some((_, _)) => {
                        return Err(DomainError::provider_not_owned(&req.provider_slug));
                    }
                    None => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };

        if !matches!(provider.status, crate::ProviderStatus::Active) {
            return Err(DomainError::ProviderDisabled { id: provider.id });
        }

        // 4. Create via repo with own-tenant scope only (no cross-tenant
        //    widening — model.tenant_id MUST equal provider.tenant_id).
        let model = self
            .model_repo
            .create(conn, &own_scope, tenant_id, req)
            .await?;

        // 5. Invalidate the owning tenant's cache (see `create_provider`).
        self.cache.invalidate_tenant(model.tenant_id).await;

        Ok(model)
    }

    /// Update a model (PATCH semantics) including approval status.
    ///
    /// Applies non-status field patches and `approval_status` transitions in a
    /// single repository call. Validates lifecycle state transitions.
    /// Invalidates cache on success.
    pub async fn update_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
        req: &crate::UpdateModelRequestV1,
    ) -> Result<crate::ModelV1, DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::UPDATE)
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // 2. Fetch existing model to validate state transitions.
        let existing = self
            .model_repo
            .find_by_canonical(&conn, &scope, canonical_id)
            .await?;

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
        let model = self
            .model_repo
            .update(&conn, &scope, canonical_id, req)
            .await?;

        // 5. Invalidate the owning tenant's cache (see `update_provider`).
        self.cache.invalidate_tenant(model.tenant_id).await;

        Ok(model)
    }

    /// Soft-delete a model by canonical ID.
    ///
    /// Sets `lifecycle_status` to `Deprecated` and records the deprecation
    /// timestamp. Invalidates the own-tenant cache on success.
    pub async fn delete_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<(), DomainError> {
        // 1. Derive access scope (authorization check + DB scope)
        let scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::DELETE)
            .await?;

        // 2. Soft-delete via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let deprecated = self
            .model_repo
            .soft_delete(&conn, &scope, canonical_id)
            .await?;

        // 3. Invalidate the owning tenant's cache (see `update_provider`).
        self.cache.invalidate_tenant(deprecated.tenant_id).await;

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
    use crate::domain::cache::{InMemoryCache, SlugOwnership};
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
    // FailingAncestorProviderRepo — mock ProviderRepository that fails on
    // ancestor queries (succeeds only on the first `list` call)
    // ═════════════════════════════════════════════════════════════════════════

    use std::sync::atomic::{AtomicUsize, Ordering};
    use toolkit_odata::PageInfo as OdataPageInfo;

    #[domain_model]
    struct FailingAncestorProviderRepo {
        call_count: AtomicUsize,
    }

    impl FailingAncestorProviderRepo {
        fn new() -> Self {
            Self {
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ProviderRepository for FailingAncestorProviderRepo {
        async fn find_by_id(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<ProviderV1, DomainError> {
            unimplemented!("not used in list_providers test")
        }

        async fn find_by_slug(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _slug: &str,
        ) -> Result<ProviderV1, DomainError> {
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
        ) -> Result<ProviderV1, DomainError> {
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
        let gts = gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
        crate::CreateProviderRequestV1::builder(slug, name, gts).build()
    }

    fn make_create_model_req(
        provider_slug: &str,
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
            provider_slug: provider_slug.to_owned(),
            lifecycle_status: crate::LifecycleStatus::Production,
            approval_status: None,
            info,
        }
    }

    /// Helper: build a fully-deprecated `ModelInfoV1` for cache tests.
    /// Constructs directly via struct literal — no JSON round-trip.
    fn make_deprecated_info(provider_model_id: &str) -> model_registry_sdk::ModelInfoV1 {
        model_registry_sdk::ModelInfoV1 {
            gts_type: gts::GtsTypeId::new("gts.cf.genai.model.info.v1~cf.genai._.openai.v1~"),
            display_name: "deprecated".to_owned(),
            description: None,
            family: None,
            vendor: None,
            managed: false,
            architecture: None,
            size_bytes: None,
            format: None,
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
                vision: MediaCapability::default(),
                reasoning: ReasoningCapability {
                    effort: false,
                    toggle: false,
                    resume: false,
                    budget: false,
                },
                function_calling: false,
                response_schema: false,
                streaming: false,
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
                max_input_tokens: 0,
                max_output_tokens: None,
                output_vector_size: None,
            },
            default_parameters: DefaultInferenceParametersV1::default(),
            allow_parameter_override: false,
            allow_extra_params: Vec::new(),
            provider_settings: serde_json::Value::Null,
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

    /// Create a test model owned by `tenant_id`, returning the model.
    async fn create_test_model(
        repo: &ModelRepositoryImpl,
        conn: &impl toolkit_db::secure::DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        provider_slug: &str,
        provider_model_id: &str,
    ) -> crate::ModelV1 {
        let req = make_create_model_req(provider_slug, provider_model_id);
        crate::domain::repo::ModelRepository::create(repo, conn, scope, tenant_id, &req)
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
        let mut req = make_create_model_req(provider_slug, provider_model_id);
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        crate::domain::repo::ModelRepository::create(repo, conn, scope, tenant_id, &req)
            .await
            .expect("create test approved model")
    }

    /// Build a full `Service` instance for testing.
    ///
    /// Sets up real DB + `ProviderRepositoryImpl` + `ModelRepositoryImpl` + `InMemoryCache` with mocks for
    /// tenant-resolver and authz-resolver.
    #[allow(clippy::type_complexity)]
    /// Build a full `Service` instance for testing, using the provided cache.
    ///
    /// Most test callers pass `InMemoryCache::new()` to start with an empty cache.
    /// Tests that pre-populate the cache pass their prepared cache instead.
    fn build_service_with_cache<R: TenantResolverClient + Send + Sync + 'static>(
        db: DBProvider<DbError>,
        tenant_resolver: R,
        config: ModelRegistryConfig,
        cache: InMemoryCache,
    ) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl, InMemoryCache> {
        let enforcer = PolicyEnforcer::new(Arc::new(MockAuthZ));
        Service {
            db: Arc::new(db),
            provider_repo: Arc::new(ProviderRepositoryImpl::default()),
            model_repo: Arc::new(ModelRepositoryImpl::default()),
            cache: Arc::new(cache),
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
        cache: InMemoryCache,
        authz: A,
    ) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl, InMemoryCache> {
        Service {
            db: Arc::new(db),
            provider_repo: Arc::new(ProviderRepositoryImpl::default()),
            model_repo: Arc::new(ModelRepositoryImpl::default()),
            cache: Arc::new(cache),
            tenant_resolver: Arc::new(tenant_resolver),
            policy_enforcer: PolicyEnforcer::new(Arc::new(authz)),
            config,
        }
    }

    /// Build a full `Service` instance with a fresh `InMemoryCache`.
    fn build_service<R: TenantResolverClient + Send + Sync + 'static>(
        db: DBProvider<DbError>,
        tenant_resolver: R,
        config: ModelRegistryConfig,
    ) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl, InMemoryCache> {
        build_service_with_cache(db, tenant_resolver, config, InMemoryCache::new())
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Slug validation tests
    // ═════════════════════════════════════════════════════════════════════════

    type TestService = super::Service<(), (), InMemoryCache>;

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
    // list_providers — fail closed on ancestor query errors
    // ═════════════════════════════════════════════════════════════════════════

    /// A `ModelRepository` stub that panics on every method — used only in
    /// `list_providers` tests where `model_repo` methods are never called.
    #[domain_model]
    struct PanicModelRepo;

    #[async_trait]
    impl ModelRepository for PanicModelRepo {
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
            _req: &crate::CreateModelRequestV1,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in list_providers tests")
        }

        async fn update(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _canonical_id: &str,
            _req: &crate::UpdateModelRequestV1,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in list_providers tests")
        }

        async fn soft_delete(
            &self,
            _conn: &impl DBRunner,
            _scope: &AccessScope,
            _canonical_id: &str,
        ) -> Result<crate::ModelV1, DomainError> {
            unimplemented!("not used in list_providers tests")
        }
    }

    #[tokio::test]
    async fn test_list_providers_fails_closed_on_ancestor_query_error() {
        // Given: a provider repo that fails on ancestor queries (only the first
        // own-tenant list call succeeds), two ancestors, and no own providers.
        let db = setup_db().await;
        let provider_repo = Arc::new(FailingAncestorProviderRepo::new());
        let model_repo = Arc::new(PanicModelRepo);
        let cache = Arc::new(InMemoryCache::new());
        let tenant_resolver: Arc<dyn TenantResolverClient> = Arc::new(TwoAncestorsResolver);
        let enforcer = PolicyEnforcer::new(Arc::new(MockAuthZ));
        let config = ModelRegistryConfig::default();

        let service: Service<FailingAncestorProviderRepo, PanicModelRepo, InMemoryCache> =
            Service {
                db: Arc::new(db),
                provider_repo,
                model_repo,
                cache,
                tenant_resolver,
                policy_enforcer: enforcer,
                config,
            };

        // When: listing providers with ancestors that fail to query.
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tenant())
            .build()
            .expect("ctx");
        let err = service
            .list_providers(&ctx, &ODataQuery::default())
            .await
            .expect_err("ancestor query failure should propagate");

        // Then: the error must be an Internal with the failure detail.
        assert!(
            matches!(&err, DomainError::Internal { detail, .. }
                if detail.contains("ancestor provider query failed")),
            "expected Internal error with ancestor failure detail, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_list_tenant_models_skips_ancestor_with_empty_page() {
        // Given: a service with real repos and TwoAncestorsResolver where the
        // parent has no provider/models. The ancestor query succeeds but returns
        // empty — this proves Skip returns partial results even with empty
        // ancestor pages.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let child_scope = scope_for(child_tid);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "openai").await;
        create_test_model(
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
            .expect("Skip should allow partial results");

        // The child's own model should be visible regardless of ancestor data.
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");
    }

    // ═════════════════════════════════════════════════════════════════════════
    #[tokio::test]
    async fn test_list_tenant_models_empty_allow_list_no_query() {
        // Given: a child tenant with NO providers of its own. The
        // allow-list slice for the child is empty — the own-tenant query
        // is skipped and replaced with an empty synthetic page.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();

        // Parent has a provider and model that the child WOULD inherit,
        // but the child shadows the slug with no provider of its own.
        let parent_tid = test_tenant();
        let parent_scope = scope_for(parent_tid);
        let (_parent_pid, parent_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;
        create_test_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &parent_slug,
            "gpt-4o",
        )
        .await;

        // Child tenant — NO provider, so allow_slice_for returns empty.
        let child_tid = child_tenant();

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let page = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("empty allow-list must not error");

        // The child shadows "openai" (no `register_provider` for that
        // slug) so the ancestor model is hidden by G3, and the child has
        // no own model — the result is empty.
        assert!(
            page.items.is_empty(),
            "child with empty allow-list must see no models"
        );
    }

    // get_tenant_model — cache hit path
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_cache_hit_own_tenant() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        // Create a model in the test tenant.
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

        // Pre-populate cache.
        let cache = InMemoryCache::new();
        let key = cache_key(&tenant_id, "model", "openai::gpt-4o");
        cache.set(&key, &model, 1800).await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(
            result.is_ok(),
            "cache hit should succeed, got: {:?}",
            result.err()
        );

        let found = result.unwrap();
        assert_eq!(found.canonical_id, "openai::gpt-4o");
        // approval_status should be populated
        assert_eq!(found.approval_status, crate::ApprovalStatus::Pending);
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — cache miss → DB populate
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_cache_miss_populates_cache() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let _model = create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        let cache = InMemoryCache::new();
        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(
            result.is_ok(),
            "cache miss + DB populate should succeed, got: {:?}",
            result.err()
        );

        let found = result.unwrap();
        assert_eq!(found.canonical_id, "openai::gpt-4o");

        // Verify cache was populated.
        let key = cache_key(&tenant_id, "model", "openai::gpt-4o");
        let cached: Option<crate::ModelV1> = cache.get(&key).await;
        assert!(cached.is_some(), "model should be cached after DB hit");
        assert_eq!(cached.unwrap().canonical_id, "openai::gpt-4o");
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
        create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Soft-delete the model.
        crate::domain::repo::ModelRepository::soft_delete(
            &model_repo,
            &conn,
            &scope,
            "openai::gpt-4o",
        )
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
    // get_tenant_model — deprecated from cache also returns error
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_deprecated_in_cache_returns_error() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let tenant_id = test_tenant();
        let cache = InMemoryCache::new();

        // Create a real provider so the slug resolution succeeds and the model
        // can be found in cache. The cached model's provider_id must match the
        // winner to reach the lifecycle gate (C2).
        let provider_repo = ProviderRepositoryImpl::default();
        let scope = scope_for(tenant_id);
        let (provider_id, _slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Build a deprecated ModelV1 directly via struct literal with the
        // real provider_id so the gate check passes.
        let deprecated_model: crate::ModelV1 = crate::ModelV1 {
            id: Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
            tenant_id,
            provider_id,
            canonical_id: "openai::gpt-4o-old".to_owned(),
            lifecycle_status: crate::LifecycleStatus::Deprecated,
            approval_status: crate::ApprovalStatus::Pending,
            info: make_deprecated_info("openai::gpt-4o-old"),
        };

        let key = cache_key(&tenant_id, "model", "openai::gpt-4o-old");
        cache.set(&key, &deprecated_model, 1800).await;

        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache,
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o-old")
            .await
            .expect_err("deprecated in cache should return ModelDeprecated");

        assert!(
            matches!(&err, DomainError::ModelDeprecated { canonical_id } if canonical_id == "openai::gpt-4o-old"),
            "expected ModelDeprecated, got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — pending/rejected model returned with populated status
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_pending_returns_model_with_status() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Create model with initial approved status
        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Pending);
        let _model = crate::domain::repo::ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
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
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(
            result.is_ok(),
            "pending model should be returned, got: {:?}",
            result.err()
        );

        let found = result.unwrap();
        // Approval status should be populated (not fail-closed).
        assert_eq!(found.approval_status, crate::ApprovalStatus::Pending);
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

        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        let _model = crate::domain::repo::ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
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
    // get_tenant_model — inherited model visible with shorter TTL
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
        create_test_model(
            &model_repo,
            &conn,
            &parent_scope,
            parent_tid,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Child tenant should inherit parent's model.
        let config = ModelRegistryConfig {
            cache_ttl_seconds: 600,
            ..Default::default()
        };
        let service = build_service(db, TwoAncestorsResolver, config);

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
        create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;
        create_test_model(
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
        create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;
        create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o-mini",
        )
        .await;

        // Soft-delete gpt-4o-mini
        crate::domain::repo::ModelRepository::soft_delete(
            &model_repo,
            &conn,
            &scope,
            "openai::gpt-4o-mini",
        )
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
        create_test_model(
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
        create_test_model(
            &model_repo,
            &conn,
            &child_scope,
            child_tid,
            &child_provider_slug,
            "gpt-4o",
        )
        .await;
        create_test_model(
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
        create_test_model(
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
        create_test_model(
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
            create_test_model(
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
    async fn test_list_tenant_models_management_shadowed_ancestor() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        // Create provider "openai" in parent and child (child shadows parent).
        let child_scope = scope_for(child_tid);
        let parent_scope = scope_for(parent_tid);

        let (_child_provider_id, child_slug) =
            create_test_provider(&provider_repo, &conn, &child_scope, child_tid, "openai").await;
        let (_parent_provider_id, parent_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;

        // Create a model in each tenant with DIFFERENT canonical IDs,
        // both approved so available_for_eval reflects the provider/winner state.
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

        // 1. Management listing shows both models with correct flags.
        let mgmt = service
            .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
            .await
            .expect("management list should succeed");

        assert_eq!(mgmt.items.len(), 2, "management should return both models");

        let parent_row = mgmt
            .items
            .iter()
            .find(|r| r.model.canonical_id.as_str() == "openai::gpt-4o-parent")
            .expect("parent model should be in management listing");
        assert!(parent_row.shadowed, "parent model should be shadowed");
        assert!(
            !parent_row.available_for_eval,
            "shadowed model should not be available for eval"
        );
        assert!(!parent_row.provider_disabled, "parent provider is active");

        let child_row = mgmt
            .items
            .iter()
            .find(|r| r.model.canonical_id.as_str() == "openai::gpt-4o-child")
            .expect("child model should be in management listing");
        assert!(!child_row.shadowed, "child model should not be shadowed");
        assert!(
            child_row.available_for_eval,
            "child model should be available for eval"
        );
        assert!(!child_row.provider_disabled, "child provider is active");

        // 2. Eval listing shows only the child model.
        let eval = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("eval list should succeed");

        assert_eq!(eval.items.len(), 1, "eval should only show child model");
        assert_eq!(eval.items[0].canonical_id, "openai::gpt-4o-child");
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
        assert!(!mgmt.items[0].shadowed, "own-tenant model is not shadowed");

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
        let req = {
            let mut r = make_create_model_req(&provider_slug, "gpt-4o");
            r.approval_status = Some(ApprovalStatus::Pending);
            r
        };
        let _ = model_repo
            .create(&conn, &scope, tid, &req)
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
        assert!(!mgmt.items[0].shadowed, "own-tenant model is not shadowed");
        assert!(
            !mgmt.items[0].provider_disabled,
            "provider is active, not disabled"
        );

        // 2. Eval listing also shows the model (approval is reported, not enforced).
        let eval = service
            .list_tenant_models(&ctx, &ODataQuery::default())
            .await
            .expect("eval list should succeed");

        assert_eq!(
            eval.items.len(),
            1,
            "eval should still show non-approved model (approval is reported, not enforced)"
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
        let req = {
            let mut r = make_create_model_req(&provider_slug, "gpt-4o-deprecated");
            r.lifecycle_status = LifecycleStatus::Deprecated;
            r
        };
        let _ = model_repo
            .create(&conn, &scope, tid, &req)
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
    // get_tenant_model — slug resolution gates (Task 7)
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_malformed_canonical_id() {
        // A canonical_id without `::` should yield ModelNotFound (C5).
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
    async fn test_get_tenant_model_stale_cache_provider_id_mismatch() {
        // A stale cached row whose provider_id no longer matches the winning
        // provider should yield ModelNotFound (C2), not the row.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let tenant_id = test_tenant();
        let cache = InMemoryCache::new();

        // Create a provider.
        let provider_repo = ProviderRepositoryImpl::default();
        let scope = scope_for(tenant_id);
        let (_provider_id, _slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Pre-populate the model cache with a row whose provider_id does NOT
        // match the winning provider (simulate a stale entry).
        let stale_model: crate::ModelV1 = crate::ModelV1 {
            id: Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
            tenant_id,
            provider_id: Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap(), // wrong!
            canonical_id: "openai::gpt-4o".to_owned(),
            lifecycle_status: crate::LifecycleStatus::Production,
            approval_status: crate::ApprovalStatus::Pending,
            info: make_deprecated_info("gpt-4o"),
        };
        let model_key = cache_key(&tenant_id, "model", "openai::gpt-4o");
        cache.set(&model_key, &stale_model, 1800).await;

        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let err = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect_err("stale cached row should fail gate C2");

        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound for stale provider_id, got: {err:?}"
        );

        // The stale cache entry should have been deleted.
        assert!(
            cache.get::<crate::ModelV1>(&model_key).await.is_none(),
            "stale cache entry should have been deleted"
        );
    }

    #[tokio::test]
    async fn test_get_tenant_model_disabled_winner_provider() {
        // A winning provider that is disabled should yield ProviderDisabled (C4).
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
        // Gate order (C4): terminal lifecycle check comes before provider
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
        create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Soft-delete the model (sets lifecycle to Deprecated).
        crate::domain::repo::ModelRepository::soft_delete(
            &model_repo,
            &conn,
            &scope,
            "openai::gpt-4o",
        )
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
        // When no tenant in the chain owns the slug, return ProviderNotFoundBySlug (C3).
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
        create_test_model(
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
        create_test_model(
            &model_repo,
            &conn,
            &child_scope,
            child_tid,
            &child_slug,
            "gpt-4o",
        )
        .await;
        create_test_model(
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

    #[tokio::test]
    async fn test_get_tenant_model_fail_closed_on_slug_query_error() {
        // A non-not-found error during slug resolution must propagate (B5).
        use std::sync::atomic::AtomicUsize;

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
            async fn find_by_slug(
                &self,
                _conn: &impl DBRunner,
                _scope: &AccessScope,
                _slug: &str,
            ) -> Result<ProviderV1, DomainError> {
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
            ) -> Result<ProviderV1, DomainError> {
                unimplemented!()
            }
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let provider_repo = Arc::new(FailingSlugRepo {
            call_count: Arc::clone(&call_count),
        });
        let model_repo = Arc::new(PanicModelRepo);
        let cache = Arc::new(InMemoryCache::new());
        let enforcer = PolicyEnforcer::new(Arc::new(MockAuthZ));

        let service: Service<FailingSlugRepo, PanicModelRepo, InMemoryCache> = Service {
            db: Arc::new(setup_db().await),
            provider_repo,
            model_repo,
            cache,
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
    }

    // ═════════════════════════════════════════════════════════════════════════
    // resolve_slug_ownership — cache-first slug resolution with tombstones
    // ═════════════════════════════════════════════════════════════════════════

    /// Create a `ProviderV1` struct literal for test cache pre-population.
    fn make_test_provider(tenant_id: Uuid, slug: &str) -> ProviderV1 {
        use chrono::Utc;
        use gts::GtsTypeId;
        ProviderV1 {
            id: Uuid::new_v4(),
            tenant_id,
            slug: slug.to_owned(),
            name: slug.to_owned(),
            gts_type: GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.generic.v1~"),
            status: crate::ProviderStatus::Active,
            managed: false,
            metadata: None,
            discovery_enabled: false,
            discovery_interval_seconds: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_resolve_slug_ownership_cache_hit_owned() {
        let cache = InMemoryCache::new();
        let service = build_service_with_cache(
            setup_db().await,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        let tenant_id = test_tenant();
        let slug = "openai";

        // Pre-populate cache with a slug ownership.
        let provider = make_test_provider(tenant_id, slug);
        let key = cache_key(&tenant_id, "provider_slug", slug);
        cache.set(&key, &SlugOwnership::Owned(provider), 60).await;

        let conn = service.db.conn().expect("conn");
        let result = service
            .resolve_slug_ownership(&conn, tenant_id, slug)
            .await
            .expect("cache hit should succeed");

        match result {
            SlugOwnership::Owned(p) => assert_eq!(p.slug, slug),
            SlugOwnership::None => panic!("expected Owned, got None tombstone"),
        }
    }

    #[tokio::test]
    async fn test_resolve_slug_ownership_cache_hit_tombstone() {
        let cache = InMemoryCache::new();
        let service = build_service_with_cache(
            setup_db().await,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        let tenant_id = test_tenant();
        let slug = "nonexistent";

        // Pre-populate cache with a tombstone.
        let key = cache_key(&tenant_id, "provider_slug", slug);
        cache.set(&key, &SlugOwnership::None, 60).await;

        let conn = service.db.conn().expect("conn");
        let result = service
            .resolve_slug_ownership(&conn, tenant_id, slug)
            .await
            .expect("tombstone hit should succeed");

        // The tombstone means we skip the DB and get None back.
        match result {
            SlugOwnership::Owned(_) => panic!("expected None tombstone, got Owned"),
            SlugOwnership::None => { /* expected - no DB query was made */ }
        }
    }

    #[tokio::test]
    async fn test_resolve_slug_ownership_db_miss_writes_tombstone() {
        let cache = InMemoryCache::new();
        let service = build_service_with_cache(
            setup_db().await,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        let tenant_id = test_tenant();
        let slug = "nonexistent";

        // First call: cache miss → DB miss → tombstone written.
        let conn = service.db.conn().expect("conn");
        let result = service
            .resolve_slug_ownership(&conn, tenant_id, slug)
            .await
            .expect("DB miss should succeed");

        match result {
            SlugOwnership::Owned(_) => panic!("expected None for nonexistent slug"),
            SlugOwnership::None => { /* expected — no provider exists */ }
        }

        // Verify tombstone was written.
        let key = cache_key(&tenant_id, "provider_slug", slug);
        let cached: Option<SlugOwnership> = cache.get(&key).await;
        match cached {
            Some(SlugOwnership::None) => { /* tombstone confirmed */ }
            other => panic!("expected tombstone (SlugOwnership::None) in cache, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_resolve_slug_ownership_db_hit_populates_cache() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        // Create a provider in the DB.
        let provider_repo = ProviderRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let slug = "openai";
        create_test_provider(&provider_repo, &conn, &scope, tenant_id, slug).await;

        let cache = InMemoryCache::new();
        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        // First call: cache miss → DB hit → cache populated.
        let svc_conn = service.db.conn().expect("conn");
        let result = service
            .resolve_slug_ownership(&svc_conn, tenant_id, slug)
            .await
            .expect("DB hit should succeed");

        match result {
            SlugOwnership::Owned(p) => assert_eq!(p.slug, slug),
            SlugOwnership::None => panic!("expected Owned for existing provider"),
        }

        // Verify cache was populated.
        let key = cache_key(&tenant_id, "provider_slug", slug);
        let cached: Option<SlugOwnership> = cache.get(&key).await;
        match cached {
            Some(SlugOwnership::Owned(p)) => assert_eq!(p.slug, slug),
            other => panic!("expected Owned in cache, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_resolve_slug_ownership_tombstone_avoids_second_db_query() {
        // Use a mock provider repo that counts calls.
        use std::sync::atomic::AtomicUsize;

        #[domain_model]
        struct CountingProviderRepo {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ProviderRepository for CountingProviderRepo {
            async fn find_by_id(
                &self,
                _conn: &impl DBRunner,
                _scope: &AccessScope,
                _id: Uuid,
            ) -> Result<ProviderV1, DomainError> {
                unimplemented!()
            }
            async fn find_by_slug(
                &self,
                _conn: &impl DBRunner,
                _scope: &AccessScope,
                _slug: &str,
            ) -> Result<ProviderV1, DomainError> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                Err(DomainError::provider_not_found_by_slug("stub"))
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
            ) -> Result<ProviderV1, DomainError> {
                unimplemented!()
            }
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let provider_repo = Arc::new(CountingProviderRepo {
            call_count: Arc::clone(&call_count),
        });

        let cache = Arc::new(InMemoryCache::new());
        let enforcer = PolicyEnforcer::new(Arc::new(MockAuthZ));
        let model_repo = Arc::new(PanicModelRepo);

        let service: Service<CountingProviderRepo, PanicModelRepo, InMemoryCache> = Service {
            db: Arc::new(setup_db().await),
            provider_repo,
            model_repo,
            cache: Arc::clone(&cache),
            tenant_resolver: Arc::new(NoAncestorsResolver),
            policy_enforcer: enforcer,
            config: ModelRegistryConfig::default(),
        };

        let tenant_id = test_tenant();
        let slug = "no-such-slug";
        let conn = service.db.conn().expect("conn");

        // First call: cache miss → DB miss → tombstone written.
        service
            .resolve_slug_ownership(&conn, tenant_id, slug)
            .await
            .expect("first call should succeed");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "first call should hit the DB once"
        );

        // Second call: cache hit (tombstone) — no DB query.
        service
            .resolve_slug_ownership(&conn, tenant_id, slug)
            .await
            .expect("second call should succeed");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "second call should NOT hit the DB - tombstone is a cache hit"
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
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let model = service
            .create_model(&ctx, &req)
            .await
            .expect("create model");

        assert_eq!(model.canonical_id, "openai::gpt-4o");
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
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
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
        let (provider_id, provider_slug) =
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

        let req = make_create_model_req(&provider_slug, "gpt-4o");
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
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
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
    async fn test_create_model_with_inherited_provider() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let parent_tid = parent_id();
        let child_tid = child_tenant();

        // Create provider in the parent tenant only.
        let parent_scope = scope_for(parent_tid);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        // Child tenant must NOT create a model referencing a parent's provider (E1).
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let err = service
            .create_model(&ctx, &req)
            .await
            .expect_err("inherited provider should be rejected");

        assert!(
            matches!(&err, DomainError::ProviderNotOwned { slug } if slug == "openai"),
            "expected ProviderNotOwned('openai'), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_create_model_provider_not_found() {
        let db = setup_db().await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let req = make_create_model_req("nonexistent", "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(test_tenant())
            .build()
            .expect("ctx");
        let err = service
            .create_model(&ctx, &req)
            .await
            .expect_err("nonexistent provider should fail");

        assert!(
            matches!(&err, DomainError::ProviderNotFoundBySlug { slug } if slug == "nonexistent"),
            "expected ProviderNotFoundBySlug('nonexistent'), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_create_model_cache_invalidation() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Pre-populate cache with a stale entry.
        let cache = InMemoryCache::new();
        let mut stale_info = make_deprecated_info("gpt-4o-old");
        stale_info.display_name = "stale".to_owned();
        let stale_model: crate::ModelV1 = crate::ModelV1 {
            id: Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
            tenant_id,
            provider_id: Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
            canonical_id: "openai::gpt-4o".to_owned(),
            lifecycle_status: crate::LifecycleStatus::Production,
            approval_status: crate::ApprovalStatus::Pending,
            info: stale_info,
        };
        let stale_key = cache_key(&tenant_id, "model", "stale-key");
        cache.set(&stale_key, &stale_model, 1800).await;

        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        // The stale key should still be in cache before create.
        assert!(
            cache.get::<crate::ModelV1>(&stale_key).await.is_some(),
            "stale entry should be present before create"
        );

        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let _model = service
            .create_model(&ctx, &req)
            .await
            .expect("create model");

        // After create, all tenant entries should be invalidated (including the stale key).
        assert!(
            cache.get::<crate::ModelV1>(&stale_key).await.is_none(),
            "stale entry should be invalidated after create"
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
        create_test_model(
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
                "openai::gpt-4o",
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
        create_test_model(
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
                "openai::gpt-4o",
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
                "openai::gpt-4o",
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
                "openai::gpt-4o",
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
        create_test_model(
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
                "openai::gpt-4o",
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
        let err = service
            .update_model(
                &ctx,
                "nonexistent::model",
                &crate::UpdateModelRequestV1 {
                    lifecycle_status: Some(crate::LifecycleStatus::Preview),
                    ..Default::default()
                },
            )
            .await
            .expect_err("nonexistent model should fail");

        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound, got: {err:?}"
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
        create_test_model(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &provider_slug,
            "gpt-4o",
        )
        .await;

        // Soft-delete (deprecate) the model manually via the repo.
        crate::domain::repo::ModelRepository::soft_delete(
            &model_repo,
            &conn,
            &scope,
            "openai::gpt-4o",
        )
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
                "openai::gpt-4o",
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

    #[tokio::test]
    async fn test_update_model_cache_invalidation() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
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

        let cache = InMemoryCache::new();
        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        // Pre-populate cache with the model
        let key = cache_key(&tenant_id, "model", "openai::gpt-4o");
        let _model_ref = service
            .get_tenant_model(
                &SecurityContext::builder()
                    .subject_id(Uuid::new_v4())
                    .subject_tenant_id(tenant_id)
                    .build()
                    .expect("ctx"),
                "openai::gpt-4o",
            )
            .await
            .expect("get model to populate cache");
        assert!(
            cache.get::<crate::ModelV1>(&key).await.is_some(),
            "model should be cached"
        );

        // Update the model
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let _updated = service
            .update_model(
                &ctx,
                "openai::gpt-4o",
                &crate::UpdateModelRequestV1 {
                    lifecycle_status: Some(crate::LifecycleStatus::Preview),
                    ..Default::default()
                },
            )
            .await
            .expect("update model");

        // Cache should be invalidated
        assert!(
            cache.get::<crate::ModelV1>(&key).await.is_none(),
            "cache should be invalidated after update"
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
        create_test_model(
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
            .delete_model(&ctx, "openai::gpt-4o")
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
        let err = service
            .delete_model(&ctx, "nonexistent::model")
            .await
            .expect_err("nonexistent model should fail");

        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_delete_model_cache_invalidation() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
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

        let cache = InMemoryCache::new();
        let service = build_service_with_cache(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
        );

        // Pre-populate cache with the model via get
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .expect("ctx");
        let _model_ref = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect("get model to populate cache");

        let key = cache_key(&tenant_id, "model", "openai::gpt-4o");
        assert!(
            cache.get::<crate::ModelV1>(&key).await.is_some(),
            "model should be cached before delete"
        );

        // Delete the model
        service
            .delete_model(&ctx, "openai::gpt-4o")
            .await
            .expect("delete model");

        // Cache should be invalidated
        assert!(
            cache.get::<crate::ModelV1>(&key).await.is_none(),
            "cache should be invalidated after delete"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Cross-tenant writes invalidate the row's tenant, not the caller's
    //
    // Cache keys are prefixed by the tenant that *owns* the row. When the PDP
    // hands back a scope spanning more than the caller's own tenant, a write can
    // land on a row owned by someone else — and invalidating
    // `ctx.subject_tenant_id()` would then clear the wrong prefix and leave the
    // stale entry serving for the rest of its TTL.
    // ═════════════════════════════════════════════════════════════════════════

    /// Seed one cache entry under each tenant so both directions are observable:
    /// the owner's entry must be dropped, the caller's must survive.
    async fn seed_two_tenant_cache(
        cache: &InMemoryCache,
        owner_tenant: Uuid,
        caller_tenant: Uuid,
        owner_key: &str,
    ) -> String {
        cache
            .set(owner_key, &make_test_provider(owner_tenant, "openai"), 1800)
            .await;

        let caller_key = cache_key(&caller_tenant, "provider", &Uuid::new_v4().to_string());
        cache
            .set(
                &caller_key,
                &make_test_provider(caller_tenant, "unrelated"),
                1800,
            )
            .await;

        caller_key
    }

    #[tokio::test]
    async fn test_update_provider_invalidates_owner_tenant_not_caller() {
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

        let cache = InMemoryCache::new();
        let owner_key = cache_key(&owner_tenant, "provider", &provider_id.to_string());
        let caller_key =
            seed_two_tenant_cache(&cache, owner_tenant, caller_tenant, &owner_key).await;

        let service = build_service_with_authz(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
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
        assert!(
            cache.get::<ProviderV1>(&owner_key).await.is_none(),
            "the owning tenant's cache prefix must be invalidated"
        );
        assert!(
            cache.get::<ProviderV1>(&caller_key).await.is_some(),
            "the caller's unrelated entries must survive: invalidation follows \
             the row, not the caller"
        );
    }

    #[tokio::test]
    async fn test_delete_provider_invalidates_owner_tenant_not_caller() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");
        let owner_tenant = other_tenant();
        let caller_tenant = test_tenant();

        let provider_repo = ProviderRepositoryImpl::default();
        let (provider_id, _slug) = create_test_provider(
            &provider_repo,
            &conn,
            &scope_for(owner_tenant),
            owner_tenant,
            "openai",
        )
        .await;

        let cache = InMemoryCache::new();
        let owner_key = cache_key(&owner_tenant, "provider", &provider_id.to_string());
        let caller_key =
            seed_two_tenant_cache(&cache, owner_tenant, caller_tenant, &owner_key).await;

        let service = build_service_with_authz(
            db,
            NoAncestorsResolver,
            ModelRegistryConfig::default(),
            cache.clone(),
            MockAuthZTenants(vec![caller_tenant, owner_tenant]),
        );

        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(caller_tenant)
            .build()
            .expect("ctx");

        service
            .delete_provider(&ctx, provider_id)
            .await
            .expect("cross-tenant delete should be permitted by this PDP");

        assert!(
            cache.get::<ProviderV1>(&owner_key).await.is_none(),
            "the owning tenant's cache prefix must be invalidated"
        );
        assert!(
            cache.get::<ProviderV1>(&caller_key).await.is_some(),
            "the caller's unrelated entries must survive"
        );
    }
}
