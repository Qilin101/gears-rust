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
use crate::{CreateProviderRequestV1, ProviderV1, UpdateProviderRequestV1};

// ---------------------------------------------------------------------------
// Authorization resource type constants
// ---------------------------------------------------------------------------

/// Authorization resource type for provider operations.
pub(crate) const PROVIDER_RESOURCE: ResourceType = ResourceType::from_static(
    "model_registry.provider",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type for model operations.
///
/// Not read until Task 12-13; suppress the lint until then.
#[allow(dead_code)]
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
    /// Not read until Task 12-13; suppress the lint until then.
    #[allow(dead_code)]
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
}

// ── Model operation stubs (Tasks 12-13) ────────────────────────────────

impl<R: ProviderRepository, M: ModelRepository, C: CacheService> Service<R, M, C> {
    /// Get a model by canonical ID (cache-first, inheritance, approval resolve).
    ///
    /// **Task 12**: implement.
    #[allow(dead_code, unreachable_code, clippy::unused_async)]
    pub async fn get_tenant_model(
        &self,
        _ctx: &SecurityContext,
        _canonical_id: &str,
    ) -> Result<crate::ModelV1, DomainError> {
        todo!("implemented in Task 12")
    }

    /// List models with `OData` filtering and inheritance.
    ///
    /// **Task 12**: implement.
    #[allow(dead_code, unreachable_code, clippy::unused_async)]
    pub async fn list_tenant_models(
        &self,
        _ctx: &SecurityContext,
        _query: ODataQuery,
    ) -> Result<Page<crate::ModelV1>, DomainError> {
        todo!("implemented in Task 12")
    }

    /// Create a new model.
    ///
    /// **Task 13**: implement.
    #[allow(dead_code, unreachable_code, clippy::unused_async)]
    pub async fn create_model(
        &self,
        _ctx: &SecurityContext,
        _req: &crate::CreateModelRequestV1,
    ) -> Result<crate::ModelV1, DomainError> {
        todo!("implemented in Task 13")
    }

    /// Update a model (PATCH semantics) including approval status.
    ///
    /// **Task 13**: implement.
    #[allow(dead_code, unreachable_code, clippy::unused_async)]
    pub async fn update_model(
        &self,
        _ctx: &SecurityContext,
        _canonical_id: &str,
        _req: &crate::UpdateModelRequestV1,
    ) -> Result<crate::ModelV1, DomainError> {
        todo!("implemented in Task 13")
    }

    /// Soft-delete a model.
    ///
    /// **Task 13**: implement.
    #[allow(dead_code, unreachable_code, clippy::unused_async)]
    pub async fn delete_model(
        &self,
        _ctx: &SecurityContext,
        _canonical_id: &str,
    ) -> Result<(), DomainError> {
        todo!("implemented in Task 13")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::domain::cache::InMemoryCache;

    // ── Slug validation tests ────────────────────────────────────────────

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
}
