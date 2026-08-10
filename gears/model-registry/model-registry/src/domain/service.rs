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

use super::cache::{CacheService, cache_key};
use super::error::DomainError;
use super::inheritance::{
    AncestorFailure, InheritanceContext, cache_ttl_seconds, find_in_chain, merge_inherited_page,
    resolve_ancestors,
};
use super::repo::{ListVisibility, ModelRepository, ProviderRepository};

use crate::config::ModelRegistryConfig;
use crate::{CreateProviderRequestV1, LifecycleStatus, ProviderV1, UpdateProviderRequestV1};

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

        // 3. Try cache for each tenant in chain (closest first)
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

        // 5. Cache under the owning tenant, with the TTL its ownership implies.
        let key = cache_key(&owner_tenant_id, "provider", &id.to_string());
        let ttl = cache_ttl_seconds(inheritance.classify(owner_tenant_id), &self.config);
        self.cache.set(&key, &provider, ttl).await;

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
            |scope, ancestor_query| async move {
                self.provider_repo.list(conn, &scope, &ancestor_query).await
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

        // 4. Invalidate own tenant cache
        self.cache.invalidate_tenant(tenant_id).await;

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

        // 3. Invalidate cache
        let tenant_id = ctx.subject_tenant_id();
        self.cache.invalidate_tenant(tenant_id).await;

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
        self.provider_repo.delete(&conn, &scope, id).await?;

        // 3. Invalidate cache
        let tenant_id = ctx.subject_tenant_id();
        self.cache.invalidate_tenant(tenant_id).await;

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
    /// Get a model by canonical ID (cache-first, inheritance, approval resolve).
    ///
    /// Returns the model with `approval_status` populated (never fail-closed on
    /// pending/rejected/revoked — the caller decides). Returns `ModelDeprecated`
    /// when a soft-deleted model is fetched directly. `ModelNotFound` when the
    /// model does not exist in the tenant chain.
    ///
    /// Cache-first: tries each tenant in the chain (closest first), falls back
    /// to DB, and populates cache on miss with TTL selected by ownership.
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

        // 3. Try cache for each tenant in chain (closest first)
        for tenant_id in inheritance.chain_ids() {
            let key = cache_key(tenant_id, "model", canonical_id);
            if let Some(model) = self.cache.get::<crate::ModelV1>(&key).await {
                // Return ModelDeprecated if the cached model is deprecated or sunset.
                // Delete the stale cache entry so it doesn't accumulate.
                if matches!(
                    model.lifecycle_status,
                    crate::LifecycleStatus::Deprecated | crate::LifecycleStatus::Sunset
                ) {
                    self.cache.delete(&key).await;
                    return Err(DomainError::model_deprecated(canonical_id));
                }
                return Ok(model);
            }
        }

        // 4. Cache miss — walk the chain in the DB, closest tenant first.
        let conn = self.db.conn().map_err(DomainError::from)?;
        let conn = &conn;
        let found = find_in_chain(
            &inheritance,
            &own_scope,
            |e| matches!(e, DomainError::ModelNotFound { .. }),
            |scope| async move {
                self.model_repo
                    .find_by_canonical(conn, &scope, canonical_id)
                    .await
            },
        )
        .await?;

        let Some((owner_tenant_id, model)) = found else {
            return Err(DomainError::model_not_found(canonical_id));
        };

        // 5. A deprecated or sunset model is reported as such, never cached.
        if matches!(
            model.lifecycle_status,
            crate::LifecycleStatus::Deprecated | crate::LifecycleStatus::Sunset
        ) {
            return Err(DomainError::model_deprecated(canonical_id));
        }

        // 6. Cache under the owning tenant, with the TTL its ownership implies.
        let key = cache_key(&owner_tenant_id, "model", canonical_id);
        let ttl = cache_ttl_seconds(inheritance.classify(owner_tenant_id), &self.config);
        self.cache.set(&key, &model, ttl).await;

        Ok(model)
    }

    /// List models with `OData` filtering and inheritance.
    ///
    /// Returns models from the own tenant (with full `OData` support) merged with
    /// models inherited from ancestor tenants. Child-tenant models shadow
    /// ancestor models with the same `canonical_id`. Deprecated models are
    /// excluded from the default list (filtered by the repository layer).
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

        // 3. Get own tenant models with OData
        // Interim value: `Management` with `include_deprecated: false` keeps
        // behavior-preserving semantics until Task 8 wires `ChainProviders`.
        let own_page = self
            .model_repo
            .list(
                &conn,
                &own_scope,
                query,
                ListVisibility::Management {
                    include_deprecated: false,
                },
            )
            .await?;

        // 4. Merge the inherited set, shadowing ancestor models by canonical_id.
        //    Skip ancestor query errors (model queries narrow, never widen).
        let conn = &conn;
        merge_inherited_page(
            &inheritance,
            own_page,
            query,
            |m| m.canonical_id.clone(),
            AncestorFailure::Skip,
            |scope, ancestor_query| async move {
                self.model_repo
                    .list(
                        conn,
                        &scope,
                        &ancestor_query,
                        ListVisibility::Management {
                            include_deprecated: false,
                        },
                    )
                    .await
            },
        )
        .await
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

        // 2. Verify authorization
        let tenant_id = ctx.subject_tenant_id();
        self.derive_access_scope(ctx, &MODEL_RESOURCE, actions::CREATE)
            .await?;

        // 3. Resolve ancestor chain
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;

        // 4. Find the provider in own or ancestor tenants, verify it is
        //    active (not disabled), and build a scope that can resolve the FK.
        let (provider_tenant_id, provider) = self
            .find_visible_provider(&conn, &inheritance, &req.provider_slug)
            .await?;

        if !matches!(provider.status, crate::ProviderStatus::Active) {
            return Err(DomainError::ProviderDisabled { id: provider.id });
        }

        let scope = AccessScope::for_tenants(vec![tenant_id, provider_tenant_id]);

        // 5. Create via repo (handles canonical_id derivation, approval default)
        let model = self
            .model_repo
            .create(&conn, &scope, tenant_id, req)
            .await?;

        // 6. Invalidate own tenant cache
        self.cache.invalidate_tenant(tenant_id).await;

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

        let tenant_id = ctx.subject_tenant_id();
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

        // 5. Invalidate cache
        self.cache.invalidate_tenant(tenant_id).await;

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
        self.model_repo
            .soft_delete(&conn, &scope, canonical_id)
            .await?;

        // 3. Invalidate cache
        let tenant_id = ctx.subject_tenant_id();
        self.cache.invalidate_tenant(tenant_id).await;

        Ok(())
    }

    /// Resolve a provider slug against the tenant chain, returning the owning
    /// tenant ID and the provider itself.
    ///
    /// Searches the own tenant first, then ancestors in chain order, so a child
    /// tenant's provider shadows an inherited one with the same slug.
    ///
    /// Returns a `Validation` error when the slug does not exist in any tenant
    /// in the chain.
    async fn find_visible_provider(
        &self,
        conn: &impl toolkit_db::secure::DBRunner,
        inheritance: &InheritanceContext,
        slug: &str,
    ) -> Result<(Uuid, ProviderV1), DomainError> {
        let own_scope = AccessScope::for_tenant(inheritance.tenant_id());
        find_in_chain(
            inheritance,
            &own_scope,
            |e| matches!(e, DomainError::ProviderNotFoundBySlug { .. }),
            |scope| async move { self.provider_repo.find_by_slug(conn, &scope, slug).await },
        )
        .await?
        .ok_or_else(|| {
            DomainError::validation(format!(
                "provider with slug `{slug}` not found in own or ancestor tenants"
            ))
        })
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
    use crate::domain::cache::InMemoryCache;
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
            provider_repo: Arc::new(ProviderRepositoryImpl),
            model_repo: Arc::new(ModelRepositoryImpl),
            cache: Arc::new(cache),
            tenant_resolver: Arc::new(tenant_resolver),
            policy_enforcer: enforcer,
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
        ) -> Result<(), DomainError> {
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
    async fn test_list_tenant_models_skip_ancestor_query_error() {
        // Given: a service with real repos and TwoAncestorsResolver where the
        // parent has no provider/models. The ancestor query succeeds but returns
        // empty — this proves Skip returns partial results even with empty
        // ancestor pages.
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
    // get_tenant_model — cache hit path
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_cache_hit_own_tenant() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        // Create a model in the test tenant.
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
            .expect_err("should return ModelNotFound");

        assert!(
            matches!(&err, DomainError::ModelNotFound { canonical_id } if canonical_id == "nonexistent::model"),
            "expected ModelNotFound, got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // get_tenant_model — deprecated model returns ModelDeprecated
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_deprecated_returns_error() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let tenant_id = test_tenant();
        let cache = InMemoryCache::new();

        // Build a deprecated ModelV1 directly via struct literal. The SDK
        // entity/info structs are not `#[non_exhaustive]`, so no JSON
        // round-trip is needed.
        let deprecated_model: crate::ModelV1 = crate::ModelV1 {
            id: Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
            provider_id: Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
            own_ttl_seconds: 1800,
            inherited_ttl_seconds: 300,
            max_page_size: 100,
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
    // get_tenant_model — cross-tenant is not found (no ancestor relationship)
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_cross_tenant_not_found() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound for cross-tenant, got: {err:?}"
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // create_model
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_create_model_success() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
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
    async fn test_create_model_with_inherited_provider() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl;
        let parent_tid = parent_id();
        let child_tid = child_tenant();

        // Create provider in the parent tenant only.
        let parent_scope = scope_for(parent_tid);
        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &parent_scope, parent_tid, "openai").await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        // Child tenant should be able to create a model referencing the parent's provider.
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(child_tid)
            .build()
            .expect("ctx");
        let model = service
            .create_model(&ctx, &req)
            .await
            .expect("create model with inherited provider");

        assert_eq!(model.canonical_id, "openai::gpt-4o");
        assert_eq!(model.approval_status, crate::ApprovalStatus::Pending);
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
            matches!(&err, DomainError::Validation { .. }),
            "expected Validation for missing provider, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_create_model_cache_invalidation() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let provider_repo = ProviderRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
}
