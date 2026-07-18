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
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{pep_properties, AccessScope, SecurityContext};
use uuid::Uuid;

use super::cache::{cache_key, CacheService};
use super::error::DomainError;
use super::inheritance::{resolve_ancestors, cache_ttl_seconds, Ownership};
use super::repo::{ModelRepository, ProviderRepository};
use crate::config::ModelRegistryConfig;
use crate::{
    CreateProviderRequestV1, LifecycleStatus, ProviderV1, UpdateProviderRequestV1,
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
        // 1. Verify basic authorization (user can "get" providers)
        self.derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::GET)
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

        // 4. Cache miss — search own tenant DB
        let conn = self.db.conn().map_err(DomainError::from)?;
        let own_tenant_id = ctx.subject_tenant_id();
        let own_scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::GET)
            .await?;

        match self
            .provider_repo
            .find_by_id(&conn, &own_scope, id)
            .await
        {
            Ok(provider) => {
                let key = cache_key(&own_tenant_id, "provider", &id.to_string());
                let ttl = cache_ttl_seconds(Ownership::Own, &self.config);
                self.cache.set(&key, &provider, ttl).await;
                return Ok(provider);
            }
            Err(DomainError::ProviderNotFound { .. }) => { /* fall through */ }
            Err(e) => return Err(e),
        }

        // 5. Search ancestor tenants
        for ancestor in &inheritance.ancestors {
            let ancestor_id = ancestor.id.0;
            let ancestor_scope = AccessScope::for_tenant(ancestor_id);

            match self
                .provider_repo
                .find_by_id(&conn, &ancestor_scope, id)
                .await
            {
                Ok(provider) => {
                    let key = cache_key(&ancestor_id, "provider", &id.to_string());
                    let ttl = cache_ttl_seconds(Ownership::Inherited, &self.config);
                    self.cache.set(&key, &provider, ttl).await;
                    return Ok(provider);
                }
                Err(DomainError::ProviderNotFound { .. }) => { /* continue */ }
                Err(e) => return Err(e),
            }
        }

        Err(DomainError::provider_not_found(id))
    }

    /// List providers visible to the caller's tenant with `OData` filtering.
    ///
    /// Returns providers from the own tenant (with full `OData` support) merged
    /// with providers inherited from ancestor tenants. Child-tenant providers
    /// shadow ancestor providers with the same slug.
    pub async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: ODataQuery,
    ) -> Result<Page<ProviderV1>, DomainError> {
        // 1. Verify authorization
        self.derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::LIST)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;

        // 3. Get own tenant providers with OData
        let own_scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::LIST)
            .await?;
        let mut page = self
            .provider_repo
            .list(&conn, &own_scope, &query)
            .await?;

        // 4. Get all providers from each ancestor
        let mut ancestor_providers: Vec<ProviderV1> = Vec::new();
        for ancestor in &inheritance.ancestors {
            let ancestor_id = ancestor.id.0;
            let ancestor_scope = AccessScope::for_tenant(ancestor_id);
            let default_query = ODataQuery::default();
            if let Ok(ancestor_page) = self
                .provider_repo
                .list(&conn, &ancestor_scope, &default_query)
                .await
            {
                ancestor_providers.extend(ancestor_page.items);
            }
        }

        // 5. Apply additive visibility with child-shadowing by slug
        let own_slugs: std::collections::HashSet<String> =
            page.items.iter().map(|p| p.slug.clone()).collect();

        for ancestor_provider in ancestor_providers {
            if !own_slugs.contains(&ancestor_provider.slug) {
                page.items.push(ancestor_provider);
            }
        }

        Ok(page)
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
        // 1. Validate slug format
        Self::validate_slug(req.slug())?;

        // 2. Verify authorization
        let tenant_id = ctx.subject_tenant_id();
        self.derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::CREATE)
            .await?;

        // 3. Create via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::CREATE)
            .await?;
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
        // 1. Verify authorization
        self.derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::UPDATE)
            .await?;

        // 2. Update via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::UPDATE)
            .await?;
        let provider = self
            .provider_repo
            .update(&conn, &scope, id, req)
            .await?;

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
        // 1. Verify authorization
        self.derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::DELETE)
            .await?;

        // 2. Delete via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let scope = self
            .derive_access_scope(ctx, &PROVIDER_RESOURCE, actions::DELETE)
            .await?;
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
        // 1. Verify authorization (user can "get" models)
        self.derive_access_scope(ctx, &MODEL_RESOURCE, actions::GET)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;

        // 3. Try cache for each tenant in chain (closest first)
        for tenant_id in inheritance.chain_ids() {
            let key = cache_key(tenant_id, "model", canonical_id);
            if let Some(model) = self.cache.get::<crate::ModelV1>(&key).await {
                // Return ModelDeprecated if the cached model is deprecated
                if model.lifecycle_status == crate::LifecycleStatus::Deprecated {
                    return Err(DomainError::model_deprecated(canonical_id));
                }
                return Ok(model);
            }
        }

        // 4. Cache miss — search own tenant DB
        let conn = self.db.conn().map_err(DomainError::from)?;
        let own_tenant_id = ctx.subject_tenant_id();
        let own_scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::GET)
            .await?;

        match self
            .model_repo
            .find_by_canonical(&conn, &own_scope, canonical_id)
            .await
        {
            Ok(model) => {
                // If deprecated, return ModelDeprecated before caching
                if model.lifecycle_status == crate::LifecycleStatus::Deprecated {
                    return Err(DomainError::model_deprecated(canonical_id));
                }
                let key = cache_key(&own_tenant_id, "model", canonical_id);
                let ttl = cache_ttl_seconds(Ownership::Own, &self.config);
                self.cache.set(&key, &model, ttl).await;
                return Ok(model);
            }
            Err(DomainError::ModelNotFound { .. }) => { /* fall through */ }
            Err(e) => return Err(e),
        }

        // 5. Search ancestor tenants
        for ancestor in &inheritance.ancestors {
            let ancestor_id = ancestor.id.0;
            let ancestor_scope = AccessScope::for_tenant(ancestor_id);

            match self
                .model_repo
                .find_by_canonical(&conn, &ancestor_scope, canonical_id)
                .await
            {
                Ok(model) => {
                    // If deprecated, return ModelDeprecated before caching
                    if model.lifecycle_status == crate::LifecycleStatus::Deprecated {
                        return Err(DomainError::model_deprecated(canonical_id));
                    }
                    let key = cache_key(&ancestor_id, "model", canonical_id);
                    let ttl = cache_ttl_seconds(Ownership::Inherited, &self.config);
                    self.cache.set(&key, &model, ttl).await;
                    return Ok(model);
                }
                Err(DomainError::ModelNotFound { .. }) => { /* continue */ }
                Err(e) => return Err(e),
            }
        }

        Err(DomainError::model_not_found(canonical_id))
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
        query: ODataQuery,
    ) -> Result<Page<crate::ModelV1>, DomainError> {
        // 1. Verify authorization
        self.derive_access_scope(ctx, &MODEL_RESOURCE, actions::LIST)
            .await?;

        // 2. Resolve ancestor chain
        let inheritance = resolve_ancestors(self.tenant_resolver.as_ref(), ctx).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;

        // 3. Get own tenant models with OData
        let own_scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::LIST)
            .await?;
        let mut page = self
            .model_repo
            .list(&conn, &own_scope, &query)
            .await?;

        // 4. Get all models from each ancestor
        let mut ancestor_models: Vec<crate::ModelV1> = Vec::new();
        for ancestor in &inheritance.ancestors {
            let ancestor_id = ancestor.id.0;
            let ancestor_scope = AccessScope::for_tenant(ancestor_id);
            let default_query = ODataQuery::default();
            if let Ok(ancestor_page) = self
                .model_repo
                .list(&conn, &ancestor_scope, &default_query)
                .await
            {
                ancestor_models.extend(ancestor_page.items);
            }
        }

        // 5. Apply additive visibility with child-shadowing by canonical_id
        let own_canonical_ids: std::collections::HashSet<String> =
            page.items.iter().map(|m| m.canonical_id.clone()).collect();

        for ancestor_model in ancestor_models {
            if !own_canonical_ids.contains(&ancestor_model.canonical_id) {
                page.items.push(ancestor_model);
            }
        }

        Ok(page)
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

        // 4. Find the provider in own or ancestor tenants and build a scope
        //    that can resolve the FK.
        let provider_tenant_id = self
            .find_provider_tenant(&conn, &inheritance, &req.provider_slug)
            .await?;

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
    /// Applies non-status field patches directly via the repository, and writes
    /// `approval_status` transitions to `model_approvals` (P1 direct-write
    /// path). Validates lifecycle state transitions. Invalidates cache on
    /// success.
    pub async fn update_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
        req: &crate::UpdateModelRequestV1,
    ) -> Result<crate::ModelV1, DomainError> {
        // 1. Verify authorization
        self.derive_access_scope(ctx, &MODEL_RESOURCE, actions::UPDATE)
            .await?;

        let tenant_id = ctx.subject_tenant_id();
        let conn = self.db.conn().map_err(DomainError::from)?;
        let scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::UPDATE)
            .await?;

        // 2. Fetch existing model to validate state transitions and get model_id
        let existing = self
            .model_repo
            .find_by_canonical(&conn, &scope, canonical_id)
            .await?;

        // 3. Validate lifecycle state transitions
        if let Some(new_lifecycle) = &req.lifecycle_status {
            Self::validate_lifecycle_transition(
                existing.lifecycle_status,
                *new_lifecycle,
            )?;
        }

        // 4. If approval_status is being changed, write to model_approvals
        //    (the P1 direct-write path). This keeps both the `model_approvals`
        //    table and the denormalized `models.approval_status` column in sync.
        if let Some(approval_status) = req.approval_status {
            self.model_repo
                .set_approval(&conn, &scope, existing.id, approval_status)
                .await?;
        }

        // 5. Update model fields (PATCH semantics via mapper). The mapper
        //    handles approval_status too, but since set_approval above already
        //    updated the denormalized column, this is a no-op for that field.
        let model = self
            .model_repo
            .update(&conn, &scope, canonical_id, req)
            .await?;

        // 6. Invalidate cache
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
        // 1. Verify authorization
        self.derive_access_scope(ctx, &MODEL_RESOURCE, actions::DELETE)
            .await?;

        // 2. Soft-delete via repo
        let conn = self.db.conn().map_err(DomainError::from)?;
        let scope = self
            .derive_access_scope(ctx, &MODEL_RESOURCE, actions::DELETE)
            .await?;
        self.model_repo
            .soft_delete(&conn, &scope, canonical_id)
            .await?;

        // 3. Invalidate cache
        let tenant_id = ctx.subject_tenant_id();
        self.cache.invalidate_tenant(tenant_id).await;

        Ok(())
    }

    /// Find the provider tenant that owns a given slug, searching own tenant
    /// first then ancestor tenants in chain order.
    ///
    /// Returns the tenant ID where the provider was found, or a `Validation`
    /// error if the provider does not exist in any tenant in the chain.
    async fn find_provider_tenant(
        &self,
        conn: &impl toolkit_db::secure::DBRunner,
        inheritance: &super::inheritance::InheritanceContext,
        slug: &str,
    ) -> Result<Uuid, DomainError> {
        // Search own tenant first
        let own_tenant_id = inheritance.tenant_id();
        let own_scope = AccessScope::for_tenants(vec![own_tenant_id]);
        if self
            .provider_repo
            .find_by_slug(conn, &own_scope, slug)
            .await
            .is_ok()
        {
            return Ok(own_tenant_id);
        }

        // Search ancestor tenants in chain order (closest first)
        for ancestor in &inheritance.ancestors {
            let ancestor_id = ancestor.id.0;
            let ancestor_scope = AccessScope::for_tenants(vec![ancestor_id]);
            if self
                .provider_repo
                .find_by_slug(conn, &ancestor_scope, slug)
                .await
                .is_ok()
            {
                return Ok(ancestor_id);
            }
        }

        Err(DomainError::validation(format!(
            "provider with slug `{slug}` not found in own or ancestor tenants"
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use authz_resolver_sdk::pep::PolicyEnforcer;
    use authz_resolver_sdk::{
        AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
    };
    use sea_orm_migration::MigratorTrait;
    use tenant_resolver_sdk::{
        GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
        GetTenantsOptions, IsAncestorOptions, TenantId, TenantRef, TenantResolverClient,
        TenantResolverError, TenantStatus,
    };
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{connect_db, ConnectOpts, DbError, DBProvider};
    use toolkit_odata::ODataQuery;
    use toolkit_security::{AccessScope, SecurityContext};
    use uuid::Uuid;

    use super::*;
    use crate::domain::cache::InMemoryCache;
    use crate::infra::storage::migrations::Migrator;
    use crate::infra::storage::sea_orm_repo::SeaOrmRepository;

    // ═════════════════════════════════════════════════════════════════════════
    // Mock AuthZResolverClient — always returns permissive responses
    // ═════════════════════════════════════════════════════════════════════════

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
        let gts =
            gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
        crate::CreateProviderRequestV1::builder(slug, name, gts).build()
    }

    fn make_create_model_req(
        provider_slug: &str,
        provider_model_id: &str,
    ) -> crate::CreateModelRequestV1 {
        let gts_leaf = "cf.genai._.openai.v1~";
        let gts_type = format!("gts.cf.genai.model.info.v1~{gts_leaf}");

        let info_value = serde_json::json!({
            "gts_type": gts_type,
            "display_name": format!("Test {provider_model_id}"),
            "description": null,
            "family": "test-family",
            "vendor": "TestVendor",
            "managed": false,
            "architecture": "transformer",
            "size_bytes": null,
            "format": "api-only",
            "region": null,
            "hosted_by": null,
            "last_release_at": null,
            "reasoning_level": null,
            "version": null,
            "sort_order": null,
            "icon": null,
            "multiplier_display": null,
            "performance": {
                "response_latency_ms": null,
                "tokens_per_second": null
            },
            "additional_info": {},
            "supported_api": ["completion"],
            "provider_model_id": provider_model_id,
            "capabilities": {
                "vision": { "enabled": true, "supported_mime_types": ["image/jpeg"] },
                "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                "function_calling": true,
                "response_schema": false,
                "streaming": true,
                "file_input": { "enabled": false, "supported_mime_types": [] },
                "image_generation": { "enabled": false, "supported_mime_types": [] },
                "audio_input": { "enabled": false, "supported_mime_types": [] },
                "audio_output": { "enabled": false, "supported_mime_types": [] },
                "code_interpreter": false,
                "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
            },
            "disabled_capabilities": {
                "vision": { "disabled": false, "disabled_mime_types": [] },
                "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                "function_calling": false,
                "response_schema": false,
                "streaming": false,
                "file_input": { "disabled": false, "disabled_mime_types": [] },
                "image_generation": { "disabled": false, "disabled_mime_types": [] },
                "audio_input": { "disabled": false, "disabled_mime_types": [] },
                "audio_output": { "disabled": false, "disabled_mime_types": [] },
                "code_interpreter": false,
                "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
            },
            "context_window": {
                "max_input_tokens": 8192,
                "max_output_tokens": 4096
            },
            "default_parameters": {
                "temperature": null, "top_p": null, "max_output_tokens": null,
                "max_tool_calls": null, "presence_penalty": null, "frequency_penalty": null,
                "top_logprobs": null, "truncation": null, "service_tier": null,
                "parallel_tool_calls": null, "text": null, "reasoning": null,
                "tool_choice": null, "store": null
            },
            "allow_parameter_override": false,
            "allow_extra_params": [],
            "provider_settings": {}
        });

        let info = serde_json::from_value(info_value).expect("ModelInfoV1 from test JSON");

        crate::CreateModelRequestV1 {
            provider_slug: provider_slug.to_owned(),
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
        repo: &SeaOrmRepository,
        conn: &impl toolkit_db::secure::DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        slug: &str,
    ) -> (Uuid, String) {
        let p = crate::domain::repo::ProviderRepository::create(
            repo, conn, scope, tenant_id, &make_create_req(slug, slug),
        )
        .await
        .expect("create test provider");
        (p.id, p.slug)
    }

    /// Create a test model owned by `tenant_id`, returning the model.
    async fn create_test_model(
        repo: &SeaOrmRepository,
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
    /// Sets up real DB + `SeaOrmRepository` + `InMemoryCache` with mocks for
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
    ) -> Service<SeaOrmRepository, SeaOrmRepository, InMemoryCache> {
        let enforcer = PolicyEnforcer::new(Arc::new(MockAuthZ));
        Service {
            db: Arc::new(db),
            provider_repo: Arc::new(SeaOrmRepository),
            model_repo: Arc::new(SeaOrmRepository),
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
    ) -> Service<SeaOrmRepository, SeaOrmRepository, InMemoryCache> {
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
    // get_tenant_model — cache hit path
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_tenant_model_cache_hit_own_tenant() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        // Create a model in the test tenant.
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let model = create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        // Pre-populate cache.
        let cache = InMemoryCache::new();
        let key = cache_key(&tenant_id, "model", "openai::gpt-4o");
        cache.set(&key, &model, 1800).await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(result.is_ok(), "cache hit should succeed, got: {:?}", result.err());

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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let _model = create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        let cache = InMemoryCache::new();
        let service = build_service_with_cache(db, NoAncestorsResolver, ModelRegistryConfig::default(), cache.clone());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(result.is_ok(), "cache miss + DB populate should succeed, got: {:?}", result.err());

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

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(test_tenant()).build().expect("ctx");
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        // Soft-delete the model.
        crate::domain::repo::ModelRepository::soft_delete(
            &repo, &conn, &scope, "openai::gpt-4o",
        )
        .await
        .expect("soft delete");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
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

        // Build a deprecated ModelV1 via JSON string deserialization to avoid
        // serde_json::json! macro recursion limit with deeply nested info.
        let deprecated_model: crate::ModelV1 = serde_json::from_str(r#"{
            "id": "00000000-0000-0000-0000-000000000099",
            "canonical_id": "openai::gpt-4o-old",
            "lifecycle_status": "deprecated",
            "approval_status": "pending",
            "info": {
                "gts_type": "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~",
                "display_name": "deprecated",
                "description": null,
                "family": null, "vendor": null, "managed": false,
                "architecture": null, "size_bytes": null, "format": null,
                "region": null, "hosted_by": null, "last_release_at": null,
                "reasoning_level": null, "version": null, "sort_order": null,
                "icon": null, "multiplier_display": null,
                "performance": { "response_latency_ms": null, "tokens_per_second": null },
                "additional_info": {},
                "supported_api": ["completion"],
                "provider_model_id": "gpt-4o-old",
                "capabilities": {
                    "vision": { "enabled": false, "supported_mime_types": [] },
                    "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                    "function_calling": false, "response_schema": false, "streaming": false,
                    "file_input": { "enabled": false, "supported_mime_types": [] },
                    "image_generation": { "enabled": false, "supported_mime_types": [] },
                    "audio_input": { "enabled": false, "supported_mime_types": [] },
                    "audio_output": { "enabled": false, "supported_mime_types": [] },
                    "code_interpreter": false,
                    "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
                },
                "disabled_capabilities": {
                    "vision": { "disabled": false, "disabled_mime_types": [] },
                    "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                    "function_calling": false, "response_schema": false, "streaming": false,
                    "file_input": { "disabled": false, "disabled_mime_types": [] },
                    "image_generation": { "disabled": false, "disabled_mime_types": [] },
                    "audio_input": { "disabled": false, "disabled_mime_types": [] },
                    "audio_output": { "disabled": false, "disabled_mime_types": [] },
                    "code_interpreter": false,
                    "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
                },
                "context_window": { "max_input_tokens": 0, "max_output_tokens": null, "output_vector_size": null },
                "default_parameters": {
                    "temperature": null, "top_p": null, "max_output_tokens": null,
                    "max_tool_calls": null, "presence_penalty": null, "frequency_penalty": null,
                    "top_logprobs": null, "truncation": null, "service_tier": null,
                    "parallel_tool_calls": null, "text": null, "reasoning": null,
                    "tool_choice": null, "store": null
                },
                "allow_parameter_override": false,
                "allow_extra_params": [],
                "provider_settings": null
            }
        }"#).expect("ModelV1 from JSON string");

        let key = cache_key(&tenant_id, "model", "openai::gpt-4o-old");
        cache.set(&key, &deprecated_model, 1800).await;

        let service = build_service_with_cache(db, NoAncestorsResolver, ModelRegistryConfig::default(), cache);

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;

        // Create model with initial approved status
        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Pending);
        let _model = crate::domain::repo::ModelRepository::create(
            &repo, &conn, &scope, tenant_id, &req,
        )
        .await
        .expect("create model with pending approval");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(result.is_ok(), "pending model should be returned, got: {:?}", result.err());

        let found = result.unwrap();
        // Approval status should be populated (not fail-closed).
        assert_eq!(found.approval_status, crate::ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn test_get_tenant_model_approved_returns_with_approved_status() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;

        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        let _model = crate::domain::repo::ModelRepository::create(
            &repo, &conn, &scope, tenant_id, &req,
        )
        .await
        .expect("create model with approved status");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(result.is_ok(), "approved model should be returned, got: {:?}", result.err());

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

        let repo = SeaOrmRepository;
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        // Create provider and model in the parent tenant.
        let parent_scope = scope_for(parent_tid);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &parent_scope, parent_tid, "openai").await;
        create_test_model(&repo, &conn, &parent_scope, parent_tid, &provider_slug, "gpt-4o").await;

        // Child tenant should inherit parent's model.
        let config = ModelRegistryConfig {
            own_ttl_seconds: 1800,
            inherited_ttl_seconds: 300,
            max_page_size: 100,
        };
        let service = build_service(db, TwoAncestorsResolver, config);

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(child_tid).build().expect("ctx");
        let result = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
        assert!(result.is_ok(), "inherited model should be found, got: {:?}", result.err());

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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o-mini").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let page = service
            .list_tenant_models(&ctx, ODataQuery::default())
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o-mini").await;

        // Soft-delete gpt-4o-mini
        crate::domain::repo::ModelRepository::soft_delete(
            &repo, &conn, &scope, "openai::gpt-4o-mini",
        )
        .await
        .expect("soft delete");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let page = service
            .list_tenant_models(&ctx, ODataQuery::default())
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

        let repo = SeaOrmRepository;
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        // Create provider and model in parent tenant.
        let parent_scope = scope_for(parent_tid);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &parent_scope, parent_tid, "openai").await;
        create_test_model(&repo, &conn, &parent_scope, parent_tid, &provider_slug, "gpt-4o").await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(child_tid).build().expect("ctx");
        let page = service
            .list_tenant_models(&ctx, ODataQuery::default())
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

        let repo = SeaOrmRepository;
        let child_tid = child_tenant();
        let parent_tid = parent_id();

        // Create provider in both tenants.
        let child_scope = scope_for(child_tid);
        let parent_scope = scope_for(parent_tid);

        let (_child_provider_id, child_provider_slug) =
            create_test_provider(&repo, &conn, &child_scope, child_tid, "openai").await;
        let (_parent_provider_id, parent_provider_slug) =
            create_test_provider(&repo, &conn, &parent_scope, parent_tid, "openai").await;

        // Create model with same canonical_id in both tenants.
        create_test_model(&repo, &conn, &child_scope, child_tid, &child_provider_slug, "gpt-4o").await;
        create_test_model(&repo, &conn, &parent_scope, parent_tid, &parent_provider_slug, "gpt-4o").await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(child_tid).build().expect("ctx");
        let page = service
            .list_tenant_models(&ctx, ODataQuery::default())
            .await
            .expect("list should succeed");

        // Should see exactly one "openai::gpt-4o" (child shadows parent).
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — respects OData limit
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_respects_odata_limit() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;

        // Create 3 models
        for i in 0..3 {
            create_test_model(
                &repo, &conn, &scope, tenant_id, &provider_slug, &format!("model-{i}"),
            )
            .await;
        }

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let query = ODataQuery {
            limit: Some(2),
            ..Default::default()
        };
        let page = service
            .list_tenant_models(&ctx, query)
            .await
            .expect("list with limit should succeed");

        assert_eq!(page.items.len(), 2);
    }

    // ═════════════════════════════════════════════════════════════════════════
    // list_tenant_models — tenant isolation
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_list_tenant_models_tenant_isolation() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();
        let scope_a = scope_for(tenant_a);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope_a, tenant_a, "openai").await;
        create_test_model(&repo, &conn, &scope_a, tenant_a, &provider_slug, "gpt-4o").await;

        // Tenant B should see no models (no ancestors relationship).
        let service_a = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx_b = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_b).build().expect("ctx");
        let page = service_a
            .list_tenant_models(&ctx_b, ODataQuery::default())
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

        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();
        let scope_a = scope_for(tenant_a);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope_a, tenant_a, "openai").await;
        create_test_model(&repo, &conn, &scope_a, tenant_a, &provider_slug, "gpt-4o").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx_b = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_b).build().expect("ctx");
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let model = service.create_model(&ctx, &req).await.expect("create model");

        assert_eq!(model.canonical_id, "openai::gpt-4o");
        assert_eq!(model.lifecycle_status, crate::LifecycleStatus::Production);
        assert_eq!(model.approval_status, crate::ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn test_create_model_with_initial_approval() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let model = service.create_model(&ctx, &req).await.expect("create model");

        assert_eq!(model.approval_status, crate::ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn test_create_model_with_inherited_provider() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let repo = SeaOrmRepository;
        let parent_tid = parent_id();
        let child_tid = child_tenant();

        // Create provider in the parent tenant only.
        let parent_scope = scope_for(parent_tid);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &parent_scope, parent_tid, "openai").await;

        let service = build_service(db, TwoAncestorsResolver, ModelRegistryConfig::default());

        // Child tenant should be able to create a model referencing the parent's provider.
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(child_tid).build().expect("ctx");
        let model = service.create_model(&ctx, &req).await.expect("create model with inherited provider");

        assert_eq!(model.canonical_id, "openai::gpt-4o");
        assert_eq!(model.approval_status, crate::ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn test_create_model_provider_not_found() {
        let db = setup_db().await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let req = make_create_model_req("nonexistent", "gpt-4o");
        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(test_tenant()).build().expect("ctx");
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;

        // Pre-populate cache with a stale entry.
        let cache = InMemoryCache::new();
        let stale_model: crate::ModelV1 = serde_json::from_str(r#"{
            "id": "00000000-0000-0000-0000-000000000099",
            "canonical_id": "openai::gpt-4o",
            "lifecycle_status": "production",
            "approval_status": "pending",
            "info": {
                "gts_type": "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~",
                "display_name": "stale",
                "description": null, "family": null, "vendor": null, "managed": false,
                "architecture": null, "size_bytes": null, "format": null,
                "region": null, "hosted_by": null, "last_release_at": null,
                "reasoning_level": null, "version": null, "sort_order": null,
                "icon": null, "multiplier_display": null,
                "performance": { "response_latency_ms": null, "tokens_per_second": null },
                "additional_info": {},
                "supported_api": ["completion"],
                "provider_model_id": "gpt-4o-old",
                "capabilities": {
                    "vision": { "enabled": false, "supported_mime_types": [] },
                    "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                    "function_calling": false, "response_schema": false, "streaming": false,
                    "file_input": { "enabled": false, "supported_mime_types": [] },
                    "image_generation": { "enabled": false, "supported_mime_types": [] },
                    "audio_input": { "enabled": false, "supported_mime_types": [] },
                    "audio_output": { "enabled": false, "supported_mime_types": [] },
                    "code_interpreter": false,
                    "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
                },
                "disabled_capabilities": {
                    "vision": { "disabled": false, "disabled_mime_types": [] },
                    "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                    "function_calling": false, "response_schema": false, "streaming": false,
                    "file_input": { "disabled": false, "disabled_mime_types": [] },
                    "image_generation": { "disabled": false, "disabled_mime_types": [] },
                    "audio_input": { "disabled": false, "disabled_mime_types": [] },
                    "audio_output": { "disabled": false, "disabled_mime_types": [] },
                    "code_interpreter": false,
                    "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
                },
                "context_window": { "max_input_tokens": 0, "max_output_tokens": null, "output_vector_size": null },
                "default_parameters": {
                    "temperature": null, "top_p": null, "max_output_tokens": null,
                    "max_tool_calls": null, "presence_penalty": null, "frequency_penalty": null,
                    "top_logprobs": null, "truncation": null, "service_tier": null,
                    "parallel_tool_calls": null, "text": null, "reasoning": null,
                    "tool_choice": null, "store": null
                },
                "allow_parameter_override": false,
                "allow_extra_params": [],
                "provider_settings": null
            }
        }"#).expect("ModelV1 from JSON");
        let stale_key = cache_key(&tenant_id, "model", "stale-key");
        cache.set(&stale_key, &stale_model, 1800).await;

        let service = build_service_with_cache(db, NoAncestorsResolver, ModelRegistryConfig::default(), cache.clone());

        // The stale key should still be in cache before create.
        assert!(cache.get::<crate::ModelV1>(&stale_key).await.is_some(), "stale entry should be present before create");

        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let _model = service.create_model(&ctx, &req).await.expect("create model");

        // After create, all tenant entries should be invalidated (including the stale key).
        assert!(cache.get::<crate::ModelV1>(&stale_key).await.is_none(), "stale entry should be invalidated after create");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // update_model
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_update_model_lifecycle_status() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");

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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
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

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(test_tenant()).build().expect("ctx");
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        // Soft-delete (deprecate) the model manually via the repo.
        crate::domain::repo::ModelRepository::soft_delete(
            &repo, &conn, &scope, "openai::gpt-4o",
        )
        .await
        .expect("soft delete");

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        let cache = InMemoryCache::new();
        let service = build_service_with_cache(db, NoAncestorsResolver, ModelRegistryConfig::default(), cache.clone());

        // Pre-populate cache with the model
        let key = cache_key(&tenant_id, "model", "openai::gpt-4o");
        let _model_ref = service
            .get_tenant_model(
                &SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx"),
                "openai::gpt-4o",
            )
            .await
            .expect("get model to populate cache");
        assert!(cache.get::<crate::ModelV1>(&key).await.is_some(), "model should be cached");

        // Update the model
        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
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
        assert!(cache.get::<crate::ModelV1>(&key).await.is_none(), "cache should be invalidated after update");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // delete_model
    // ═════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_delete_model_soft_deletes() {
        let db = setup_db().await;
        let conn = db.conn().expect("conn");

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        let service = build_service(db, NoAncestorsResolver, ModelRegistryConfig::default());

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
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

        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(test_tenant()).build().expect("ctx");
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

        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);
        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        create_test_model(&repo, &conn, &scope, tenant_id, &provider_slug, "gpt-4o").await;

        let cache = InMemoryCache::new();
        let service = build_service_with_cache(db, NoAncestorsResolver, ModelRegistryConfig::default(), cache.clone());

        // Pre-populate cache with the model via get
        let ctx = SecurityContext::builder().subject_id(Uuid::new_v4()).subject_tenant_id(tenant_id).build().expect("ctx");
        let _model_ref = service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect("get model to populate cache");

        let key = cache_key(&tenant_id, "model", "openai::gpt-4o");
        assert!(cache.get::<crate::ModelV1>(&key).await.is_some(), "model should be cached before delete");

        // Delete the model
        service
            .delete_model(&ctx, "openai::gpt-4o")
            .await
            .expect("delete model");

        // Cache should be invalidated
        assert!(cache.get::<crate::ModelV1>(&key).await.is_none(), "cache should be invalidated after delete");
    }
}
