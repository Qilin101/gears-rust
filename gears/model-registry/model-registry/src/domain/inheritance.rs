//! Tenant inheritance resolver helper — the **eval-path** visibility mechanism.
//!
//! Implements DESIGN §2.1 "Additive Inheritance": providers/models are visible
//! to a tenant if they are owned by that tenant **or** any of its ancestors.
//! When a parent and child both have a resource with the same key (provider
//! slug or model `canonical_id`), the child's version **shadows** the parent's.
//!
//! Used by `get_tenant_model` and `list_tenant_models` only. The admin surface
//! reads and writes within its PDP access scope and does not resolve the chain.
//!
//! Both eval reads issue **one** chain-wide query rather than one per chain
//! tenant: the scope carries every chain tenant at once and each returned row is
//! attributed to its own `tenant_id`.

use std::collections::HashMap;

use tenant_resolver_sdk::{BarrierMode, GetAncestorsOptions, TenantId, TenantResolverClient};
use toolkit_macros::domain_model;
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, SecurityContext, pep_properties,
};
use uuid::Uuid;

use super::cache::ResolutionCache;
use super::error::DomainError;
use crate::ProviderV1;
use model_registry_sdk::models::ProviderStatus;

// ---------------------------------------------------------------------------
// ChainProviders — tenant visibility primitive (DESIGN §3.5)
// ---------------------------------------------------------------------------

/// A single provider in the [`ChainProviders`] result, tagged with its
/// owning tenant and its winner status.
#[derive(Debug, Clone)]
#[domain_model]
pub struct ChainProvider {
    pub id: Uuid,
    pub owner_tenant: Uuid,
    pub slug: String,
    pub status: ProviderStatus,
    pub winner: bool,
}

/// Complete provider map for a tenant chain, with pre-computed winner
/// assignments and the eval-path allow-list.
///
/// `winner(p)` is determined by **ownership alone**: the closest chain
/// tenant owning slug `p.slug` wins it, regardless of `p.status`. If
/// status were folded into `winner`, a disabled shadow would hand the
/// slug back to the ancestor, re-exposing exactly the models the shadow
/// exists to hide.
///
/// `allow_list` is `winner AND status == active` — the eval path filters
/// on this set. A disabled winner still *wins* its slug (so the ancestor's
/// models are excluded) but is absent from `allow_list` (so its own models
/// are excluded too).
#[derive(Debug, Clone)]
#[domain_model]
pub struct ChainProviders {
    by_id: HashMap<Uuid, ChainProvider>,
    allow_list: Vec<Uuid>,
}

impl ChainProviders {
    /// Look up a provider by its UUID.
    #[must_use]
    pub fn get(&self, provider_id: Uuid) -> Option<&ChainProvider> {
        self.by_id.get(&provider_id)
    }

    /// The eval-path allow-list: provider ids that are both winners and active.
    ///
    /// Spans the whole chain — the listing path passes it to a single
    /// `provider_id IN (…)` predicate rather than slicing it per tenant.
    #[must_use]
    pub fn allow_list(&self) -> &[Uuid] {
        &self.allow_list
    }

    /// Returns `true` when `provider_id` is in the eval-path allow-list.
    #[must_use]
    pub fn is_allowed(&self, provider_id: Uuid) -> bool {
        self.allow_list.contains(&provider_id)
    }

    /// The provider that wins `slug` across the chain, regardless of status.
    ///
    /// The single read needs the winner even when it is `disabled`, so the gate
    /// order can report `ProviderDisabled` only after the model is found
    /// (DESIGN §3.5, normative). At most one provider per slug is a winner, so
    /// the scan is unambiguous.
    #[must_use]
    pub fn winner_for_slug(&self, slug: &str) -> Option<&ChainProvider> {
        self.by_id.values().find(|p| p.winner && p.slug == slug)
    }
}

/// Build the complete [`ChainProviders`] from the rows of **one** chain-wide
/// provider query.
///
/// Each row is attributed to its own `provider.tenant_id` and ranked by chain
/// distance; a row owned by a tenant outside the chain is dropped (defensive —
/// the caller's `AccessScope::for_tenants(chain)` already excludes it).
/// Ordering by [`InheritanceContext`] position rather than by arrival is what
/// makes the single batch equivalent to the closest-first fan-out it replaces.
///
/// The winner is the closest chain tenant owning a given slug — ownership
/// only, status is not a factor. Failing closed is now the caller's `?` on the
/// single query: there is no partial provider set to skip.
#[must_use]
pub fn build_chain_providers(
    inheritance: &InheritanceContext,
    providers: Vec<ProviderV1>,
) -> ChainProviders {
    // Rank every row by chain distance, dropping tenants outside the chain.
    let mut ranked: Vec<(usize, ProviderV1)> = providers
        .into_iter()
        .filter_map(|p| inheritance.position(p.tenant_id).map(|pos| (pos, p)))
        .collect();

    // Stable sort, so the closest tenant's row for a slug comes first.
    ranked.sort_by_key(|(pos, _)| *pos);

    // Determine winners: the first occurrence of each slug wins.
    let mut winners: HashMap<String, Uuid> = HashMap::new();
    for (_pos, provider) in &ranked {
        winners
            .entry(provider.slug.clone())
            .or_insert(provider.tenant_id);
    }

    // Build the by_id map, tagging each provider with its winner status.
    let mut by_id: HashMap<Uuid, ChainProvider> = HashMap::with_capacity(ranked.len());
    for (_pos, provider) in ranked {
        let winner = winners.get(&provider.slug) == Some(&provider.tenant_id);
        by_id.insert(
            provider.id,
            ChainProvider {
                id: provider.id,
                owner_tenant: provider.tenant_id,
                slug: provider.slug,
                status: provider.status,
                winner,
            },
        );
    }

    // Build the allow-list: winners whose status is active.
    let allow_list: Vec<Uuid> = by_id
        .values()
        .filter(|p| p.winner && p.status == ProviderStatus::Active)
        .map(|p| p.id)
        .collect();

    ChainProviders { by_id, allow_list }
}

/// Resolved ancestor chain for a tenant, providing the chain-distance ranking
/// that decides which tenant shadows which.
#[derive(Debug, Clone)]
#[domain_model]
pub struct InheritanceContext {
    /// The requesting tenant's ID.
    tenant_id: Uuid,
    /// Full chain from closest (self) to root: `[self, parent, grandparent, ...]`.
    chain_ids: Vec<Uuid>,
    /// Distance of each chain tenant from the requestor: `0` is self, `1` the
    /// direct parent, and so on. Drives both membership checks and the
    /// closest-wins ordering in [`build_chain_providers`].
    pos_in_chain: HashMap<Uuid, usize>,
}

impl InheritanceContext {
    /// Build a new context from the requesting tenant's ID and its ancestor IDs.
    ///
    /// `ancestor_ids` should be ordered from direct parent to root (as returned
    /// by [`TenantResolverClient::get_ancestors`]).
    #[must_use]
    pub fn new(tenant_id: Uuid, ancestor_ids: Vec<Uuid>) -> Self {
        let mut chain_ids = Vec::with_capacity(1 + ancestor_ids.len());
        chain_ids.push(tenant_id);
        chain_ids.extend(ancestor_ids);

        // Keep the *first* (closest) position for each tenant, so a malformed
        // chain that repeats a tenant cannot demote it to a farther position.
        let mut pos_in_chain: HashMap<Uuid, usize> = HashMap::with_capacity(chain_ids.len());
        for (pos, id) in chain_ids.iter().enumerate() {
            pos_in_chain.entry(*id).or_insert(pos);
        }

        Self {
            tenant_id,
            chain_ids,
            pos_in_chain,
        }
    }

    /// The requesting tenant's ID.
    #[must_use]
    pub fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    /// Returns an iterator over all tenant IDs in the chain, starting with
    /// the requesting tenant and ending at the root ancestor.
    pub fn chain_ids(&self) -> impl Iterator<Item = &Uuid> + '_ {
        self.chain_ids.iter()
    }

    /// Distance of `tenant_id` from the requestor: `0` is self, `1` the direct
    /// parent. `None` when the tenant is outside the chain.
    #[must_use]
    pub fn position(&self, tenant_id: Uuid) -> Option<usize> {
        self.pos_in_chain.get(&tenant_id).copied()
    }
}

// ---------------------------------------------------------------------------
// chain_read_scope
// ---------------------------------------------------------------------------

/// The [`AccessScope`] for the chain-wide eval **model** query.
///
/// ORs the caller's PDP scope — which binds own-tenant rows and carries every
/// constraint the PDP compiled — with one plain
/// `owner_tenant_id IN (ancestors)` constraint. That second branch is the very
/// scope the per-ancestor fan-out used to synthesize, OR-ed in rather than
/// issued separately, so ancestor rows stay scoped by tenant alone per the
/// tenant-isolation principle (DESIGN §2.1).
///
/// A scope carrying no constraints is returned verbatim: `allow_all` holds an
/// empty constraint list with `unconstrained = true`, so pushing onto it would
/// silently narrow allow-all to *ancestors only* and drop every own-tenant row.
/// `deny_all` falls out of the same branch.
#[must_use]
pub fn chain_read_scope(own_scope: &AccessScope, inheritance: &InheritanceContext) -> AccessScope {
    let ancestors: Vec<Uuid> = inheritance.chain_ids().skip(1).copied().collect();
    if ancestors.is_empty() || own_scope.constraints().is_empty() {
        return own_scope.clone();
    }

    let mut constraints = own_scope.constraints().to_vec();
    constraints.push(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
        pep_properties::OWNER_TENANT_ID,
        ancestors,
    )]));
    AccessScope::from_constraints(constraints)
}

// ---------------------------------------------------------------------------
// resolve_ancestors
// ---------------------------------------------------------------------------

/// Resolve the ancestor chain for the tenant identified by
/// `ctx.subject_tenant_id()`, cache-first.
///
/// On a [`ResolutionCache`] hit the chain is rebuilt from the cached IDs with
/// no `tenant-resolver` call. On a miss the resolver is queried with
/// [`BarrierMode::Respect`] and the resulting ancestor IDs are stored.
///
/// Only IDs are cached: `status` / `self_managed` / `tenant_type` are mutable
/// and unused here. [`BarrierMode::Respect`] is hardcoded at this single call
/// site, which is what makes keying by `tenant_id` alone correct — if a second
/// call site ever varies it, the key must include it.
///
/// # Errors
///
/// Returns [`DomainError::Internal`] when the tenant-resolver call fails
/// (network error, invalid response, etc.). A cache failure is never an error:
/// it degrades to a resolver call.
pub async fn resolve_ancestors<T, C>(
    resolver: &T,
    cache: &C,
    ctx: &SecurityContext,
) -> Result<InheritanceContext, DomainError>
where
    T: TenantResolverClient + ?Sized,
    C: ResolutionCache + ?Sized,
{
    let tenant_id = ctx.subject_tenant_id();

    // An empty `Vec` is a hit (a root tenant), not a miss.
    if let Some(ancestor_ids) = cache.get_chain(tenant_id).await {
        return Ok(InheritanceContext::new(tenant_id, ancestor_ids));
    }

    let response = resolver
        .get_ancestors(
            ctx,
            TenantId(tenant_id),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Respect,
            },
        )
        .await
        .map_err(|e| DomainError::internal_from("tenant-resolver ancestors call failed", e))?;

    let ancestor_ids: Vec<Uuid> = response.ancestors.into_iter().map(|a| a.id.0).collect();
    cache.put_chain(tenant_id, &ancestor_ids).await;

    Ok(InheritanceContext::new(tenant_id, ancestor_ids))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cache::NoopResolutionCache;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tenant_resolver_sdk::TenantRef;
    use tenant_resolver_sdk::{
        GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse, GetTenantsOptions,
        IsAncestorOptions, TenantInfo, TenantResolverError, TenantStatus,
    };

    // ── Mock TenantResolverClient ─────────────────────────────────────────

    #[domain_model]
    struct MockTenantResolver {
        ancestors: Vec<TenantRef>,
        calls: Arc<AtomicUsize>,
    }

    impl MockTenantResolver {
        fn new(ancestors: Vec<TenantRef>) -> Self {
            Self {
                ancestors,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl TenantResolverClient for MockTenantResolver {
        async fn get_tenant(
            &self,
            _ctx: &SecurityContext,
            _id: TenantId,
        ) -> Result<TenantInfo, TenantResolverError> {
            unimplemented!("not used in tests")
        }

        async fn get_root_tenant(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<TenantInfo, TenantResolverError> {
            unimplemented!("not used in tests")
        }

        async fn get_tenants(
            &self,
            _ctx: &SecurityContext,
            _ids: &[TenantId],
            _options: &GetTenantsOptions,
        ) -> Result<Vec<TenantInfo>, TenantResolverError> {
            unimplemented!("not used in tests")
        }

        async fn get_ancestors(
            &self,
            _ctx: &SecurityContext,
            _id: TenantId,
            _options: &GetAncestorsOptions,
        ) -> Result<GetAncestorsResponse, TenantResolverError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(GetAncestorsResponse {
                tenant: TenantRef {
                    id: _id,
                    status: TenantStatus::Active,
                    tenant_type: None,
                    parent_id: None,
                    self_managed: false,
                },
                ancestors: self.ancestors.clone(),
            })
        }

        async fn get_descendants(
            &self,
            _ctx: &SecurityContext,
            _id: TenantId,
            _options: &GetDescendantsOptions,
        ) -> Result<GetDescendantsResponse, TenantResolverError> {
            unimplemented!("not used in tests")
        }

        async fn is_ancestor(
            &self,
            _ctx: &SecurityContext,
            _ancestor_id: TenantId,
            _descendant_id: TenantId,
            _options: &IsAncestorOptions,
        ) -> Result<bool, TenantResolverError> {
            unimplemented!("not used in tests")
        }
    }

    // ── Stub ResolutionCache ──────────────────────────────────────────────

    /// In-process [`ResolutionCache`] that counts writes, so a test can assert
    /// *which* side of the cache a chain came from.
    #[derive(Default)]
    struct StubCache {
        entries: std::sync::Mutex<HashMap<Uuid, Vec<Uuid>>>,
        puts: AtomicUsize,
    }

    impl StubCache {
        fn warm(tenant_id: Uuid, chain: Vec<Uuid>) -> Self {
            let stub = Self::default();
            stub.insert(tenant_id, chain);
            stub
        }

        fn insert(&self, tenant_id: Uuid, chain: Vec<Uuid>) {
            self.entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(tenant_id, chain);
        }

        fn puts(&self) -> usize {
            self.puts.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ResolutionCache for StubCache {
        async fn get_chain(&self, tenant_id: Uuid) -> Option<Vec<Uuid>> {
            self.entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&tenant_id)
                .cloned()
        }

        async fn put_chain(&self, tenant_id: Uuid, chain: &[Uuid]) {
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.insert(tenant_id, chain.to_vec());
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    fn parent_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }

    fn grandparent_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()
    }

    fn child_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000010").unwrap()
    }

    fn outsider_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-0000000000ff").unwrap()
    }

    /// The same chain as [`make_ancestors`], as the IDs `InheritanceContext`
    /// now carries.
    fn make_ancestor_ids() -> Vec<Uuid> {
        vec![parent_id(), grandparent_id()]
    }

    fn make_ancestors() -> Vec<TenantRef> {
        vec![
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
        ]
    }

    // ── Tests: InheritanceContext construction ─────────────────────────────

    #[test]
    fn test_new_context_sets_chain_correctly() {
        let ctx = InheritanceContext::new(child_id(), make_ancestor_ids());
        assert_eq!(ctx.tenant_id(), child_id());
        assert_eq!(ctx.chain_ids.len(), 3);
        assert_eq!(ctx.chain_ids[0], child_id());
        assert_eq!(ctx.chain_ids[1], parent_id());
        assert_eq!(ctx.chain_ids[2], grandparent_id());
    }

    #[test]
    fn test_new_context_with_no_ancestors() {
        let ctx = InheritanceContext::new(child_id(), vec![]);
        assert_eq!(ctx.chain_ids.len(), 1);
        assert_eq!(ctx.chain_ids[0], child_id());
    }

    // ── Tests: chain-distance ranking ──────────────────────────────────────

    #[test]
    fn test_position_ranks_by_chain_distance() {
        let ctx = InheritanceContext::new(child_id(), make_ancestor_ids());
        assert_eq!(ctx.position(child_id()), Some(0));
        assert_eq!(ctx.position(parent_id()), Some(1));
        assert_eq!(ctx.position(grandparent_id()), Some(2));
    }

    #[test]
    fn test_position_is_none_outside_the_chain() {
        let ctx = InheritanceContext::new(child_id(), make_ancestor_ids());
        assert_eq!(ctx.position(outsider_id()), None);
    }

    #[test]
    fn test_single_tenant_no_ancestors_resolve_correctly() {
        let ctx = InheritanceContext::new(child_id(), vec![]);
        assert_eq!(ctx.chain_ids.len(), 1);
        assert_eq!(ctx.chain_ids[0], child_id());
        assert_eq!(ctx.position(parent_id()), None);
        assert_eq!(ctx.position(grandparent_id()), None);
    }

    // ── Tests: chain_read_scope ────────────────────────────────────────────

    fn pdp_scope_for(tenant: Uuid, resource_ids: Vec<Uuid>) -> AccessScope {
        AccessScope::single(ScopeConstraint::new(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
            ScopeFilter::in_uuids(pep_properties::RESOURCE_ID, resource_ids),
        ]))
    }

    #[test]
    fn test_chain_read_scope_preserves_pdp_constraints_and_adds_ancestors() {
        let ctx = InheritanceContext::new(child_id(), make_ancestor_ids());
        let allowed_row = Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap();
        let own = pdp_scope_for(child_id(), vec![allowed_row]);

        let scope = chain_read_scope(&own, &ctx);

        // Branch 1 is the PDP scope, byte-identical.
        assert_eq!(scope.constraints().len(), 2);
        assert_eq!(scope.constraints()[0], own.constraints()[0]);

        // Branch 2 is a plain ancestor tenant filter — and nothing else, so the
        // PDP's row-level constraint never binds an inherited row.
        let ancestor_branch = &scope.constraints()[1];
        assert_eq!(ancestor_branch.filters().len(), 1);
        assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, parent_id()));
        assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, grandparent_id()));
    }

    /// The allow-all regression guard: `allow_all` carries an empty constraint
    /// list, so OR-ing an ancestor branch onto it would narrow the scope to
    /// ancestors only and drop every own-tenant row.
    #[test]
    fn test_chain_read_scope_leaves_an_unconstrained_scope_unconstrained() {
        let ctx = InheritanceContext::new(child_id(), make_ancestor_ids());
        let scope = chain_read_scope(&AccessScope::allow_all(), &ctx);
        assert!(scope.is_unconstrained());
    }

    #[test]
    fn test_chain_read_scope_leaves_a_deny_all_scope_denying() {
        let ctx = InheritanceContext::new(child_id(), make_ancestor_ids());
        let scope = chain_read_scope(&AccessScope::deny_all(), &ctx);
        assert!(scope.is_deny_all());
    }

    #[test]
    fn test_chain_read_scope_is_identity_for_a_root_tenant() {
        let ctx = InheritanceContext::new(child_id(), vec![]);
        let own = AccessScope::for_tenant(child_id());
        assert_eq!(chain_read_scope(&own, &ctx), own);
    }

    // ── Tests: resolve_ancestors ──────────────────────────────────────────

    #[tokio::test]
    async fn test_resolve_ancestors_with_mock() {
        let resolver = MockTenantResolver::new(make_ancestors());
        let ctx = SecurityContext::anonymous();
        let result = resolve_ancestors(&resolver, &NoopResolutionCache, &ctx).await;
        assert!(result.is_ok());

        let inheritance = result.unwrap();
        let chain: Vec<Uuid> = inheritance.chain_ids().copied().collect();
        // ancestors are ordered direct-parent-first, behind the requestor
        assert_eq!(
            chain,
            vec![ctx.subject_tenant_id(), parent_id(), grandparent_id()]
        );
    }

    #[tokio::test]
    async fn test_resolve_ancestors_no_ancestors() {
        let resolver = MockTenantResolver::new(vec![]);
        let ctx = SecurityContext::anonymous();
        let result = resolve_ancestors(&resolver, &NoopResolutionCache, &ctx).await;
        assert!(result.is_ok());

        let inheritance = result.unwrap();
        assert_eq!(inheritance.chain_ids.len(), 1);
    }

    /// A warm entry serves the chain without a `tenant-resolver` call.
    #[tokio::test]
    async fn test_resolve_ancestors_warm_chain_skips_resolver() {
        let ctx = SecurityContext::anonymous();
        let cache = StubCache::warm(ctx.subject_tenant_id(), make_ancestor_ids());
        let resolver = MockTenantResolver::new(make_ancestors());

        let inheritance = resolve_ancestors(&resolver, &cache, &ctx)
            .await
            .expect("a warm chain resolves");

        let chain: Vec<Uuid> = inheritance.chain_ids().copied().collect();
        assert_eq!(
            chain,
            vec![ctx.subject_tenant_id(), parent_id(), grandparent_id()]
        );
        assert_eq!(
            resolver.calls(),
            0,
            "a warm chain must not call the resolver"
        );
        assert_eq!(cache.puts(), 0, "a warm chain must not rewrite the entry");
    }

    /// A cold entry calls the resolver exactly once and stores the result, so
    /// the next resolution is warm.
    #[tokio::test]
    async fn test_resolve_ancestors_cold_chain_populates_cache() {
        let ctx = SecurityContext::anonymous();
        let cache = StubCache::default();
        let resolver = MockTenantResolver::new(make_ancestors());

        let first = resolve_ancestors(&resolver, &cache, &ctx)
            .await
            .expect("a cold chain resolves");
        assert_eq!(resolver.calls(), 1);
        assert_eq!(cache.puts(), 1, "the cold chain is stored");

        let second = resolve_ancestors(&resolver, &cache, &ctx)
            .await
            .expect("the second resolution is warm");
        assert_eq!(resolver.calls(), 1, "the second resolution is served warm");
        assert_eq!(
            first.chain_ids().copied().collect::<Vec<_>>(),
            second.chain_ids().copied().collect::<Vec<_>>()
        );
    }

    /// A root tenant caches an empty ancestor list. `Option::None` is the only
    /// miss — an empty `Vec` is a hit and must not re-call the resolver.
    #[tokio::test]
    async fn test_resolve_ancestors_empty_chain_is_a_hit_not_a_miss() {
        let ctx = SecurityContext::anonymous();
        let cache = StubCache::default();
        let resolver = MockTenantResolver::new(vec![]);

        resolve_ancestors(&resolver, &cache, &ctx)
            .await
            .expect("a root tenant resolves");
        assert_eq!(resolver.calls(), 1);

        let warm = resolve_ancestors(&resolver, &cache, &ctx)
            .await
            .expect("the empty chain is served warm");
        assert_eq!(
            resolver.calls(),
            1,
            "an empty ancestor list is a hit, not a miss"
        );
        assert_eq!(warm.chain_ids().count(), 1);
    }

    /// `NoopResolutionCache` resolves correctly and never spares a call.
    #[tokio::test]
    async fn test_resolve_ancestors_noop_cache_always_calls_resolver() {
        let ctx = SecurityContext::anonymous();
        let resolver = MockTenantResolver::new(make_ancestors());

        for expected_calls in 1..=3 {
            let inheritance = resolve_ancestors(&resolver, &NoopResolutionCache, &ctx)
                .await
                .expect("the uncached path resolves");
            assert_eq!(inheritance.chain_ids().count(), 3);
            assert_eq!(resolver.calls(), expected_calls);
        }
    }

    // ── Tests: Error path (resolver failure) ───────────────────────────────

    /// A resolver that always fails.
    #[domain_model]
    struct FailingResolver;

    #[async_trait]
    impl TenantResolverClient for FailingResolver {
        async fn get_tenant(
            &self,
            _ctx: &SecurityContext,
            _id: TenantId,
        ) -> Result<TenantInfo, TenantResolverError> {
            unimplemented!("not used in tests")
        }

        async fn get_root_tenant(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<TenantInfo, TenantResolverError> {
            unimplemented!("not used in tests")
        }

        async fn get_tenants(
            &self,
            _ctx: &SecurityContext,
            _ids: &[TenantId],
            _options: &GetTenantsOptions,
        ) -> Result<Vec<TenantInfo>, TenantResolverError> {
            unimplemented!("not used in tests")
        }

        async fn get_ancestors(
            &self,
            _ctx: &SecurityContext,
            _id: TenantId,
            _options: &GetAncestorsOptions,
        ) -> Result<GetAncestorsResponse, TenantResolverError> {
            Err(TenantResolverError::Internal("resolver unavailable".into()))
        }

        async fn get_descendants(
            &self,
            _ctx: &SecurityContext,
            _id: TenantId,
            _options: &GetDescendantsOptions,
        ) -> Result<GetDescendantsResponse, TenantResolverError> {
            unimplemented!("not used in tests")
        }

        async fn is_ancestor(
            &self,
            _ctx: &SecurityContext,
            _ancestor_id: TenantId,
            _descendant_id: TenantId,
            _options: &IsAncestorOptions,
        ) -> Result<bool, TenantResolverError> {
            unimplemented!("not used in tests")
        }
    }

    #[tokio::test]
    async fn test_resolve_ancestors_failure_returns_internal_error() {
        let ctx = SecurityContext::anonymous();
        let result = resolve_ancestors(&FailingResolver, &NoopResolutionCache, &ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_str = err.to_string();
        assert!(
            err_str.contains("tenant-resolver ancestors call failed"),
            "expected internal error about resolver, got: {err_str}"
        );
        // The source chain should include the original resolver error.
        match &err {
            DomainError::Internal { source, .. } => {
                let source_msg = source.as_ref().map(ToString::to_string).unwrap_or_default();
                assert!(
                    source_msg.contains("resolver unavailable"),
                    "expected source to mention 'resolver unavailable', got: {source_msg}"
                );
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // ChainProviders — tests
    // ═════════════════════════════════════════════════════════════════════════

    use chrono::Utc;
    use gts::GtsTypeId;

    /// `tenant_id` is authoritative: [`build_chain_providers`] attributes each
    /// row by it, so a fixture that leaves it unset loses the provider to the
    /// out-of-chain filter.
    fn make_provider(id: Uuid, tenant_id: Uuid, slug: &str, status: ProviderStatus) -> ProviderV1 {
        let now = Utc::now();
        ProviderV1 {
            id,
            tenant_id,
            slug: slug.to_owned(),
            name: slug.to_owned(),
            gts_type: GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.generic.v1~"),
            status,
            managed: false,
            metadata: None,
            discovery_enabled: false,
            discovery_interval_seconds: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn single_ancestor() -> Vec<Uuid> {
        vec![parent_id()]
    }

    fn pid(tail: &str) -> Uuid {
        Uuid::parse_str(&format!("00000000-0000-0000-0000-0000000000{tail}")).unwrap()
    }

    #[test]
    fn test_chain_providers_single_tenant() {
        let ctx = InheritanceContext::new(child_id(), vec![]);
        let providers = vec![
            make_provider(pid("a0"), child_id(), "openai", ProviderStatus::Active),
            make_provider(pid("a1"), child_id(), "anthropic", ProviderStatus::Active),
        ];

        let chain = build_chain_providers(&ctx, providers);

        // Both providers should be winners and in the allow-list.
        assert_eq!(chain.by_id.len(), 2);
        assert_eq!(chain.allow_list().len(), 2);
        assert!(chain.is_allowed(pid("a0")));
        assert_eq!(chain.get(pid("a0")).map(|p| p.winner), Some(true));
    }

    #[test]
    fn test_chain_providers_child_shadows_parent() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());
        let own_id = pid("b0");
        let parent_prov_id = pid("b1");

        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(own_id, child_id(), "openai", ProviderStatus::Active),
                make_provider(
                    parent_prov_id,
                    parent_id(),
                    "openai",
                    ProviderStatus::Active,
                ),
            ],
        );

        // Child wins the "openai" slug.
        let child_prov = chain.get(own_id).expect("child provider should exist");
        assert!(child_prov.winner, "child should win the slug");
        assert_eq!(child_prov.owner_tenant, child_id());
        assert_eq!(chain.allow_list().len(), 1);
        assert!(chain.is_allowed(own_id));
        assert!(!chain.is_allowed(parent_prov_id));
    }

    /// The batch arrives in an arbitrary order, so the winner must be decided by
    /// chain distance rather than by which row came first.
    #[test]
    fn test_chain_providers_attributes_rows_by_their_own_tenant_id() {
        let ctx = InheritanceContext::new(child_id(), make_ancestor_ids());
        let gp_prov = pid("c0");
        let parent_prov = pid("c1");
        let child_prov = pid("c2");

        // Deliberately reverse chain order: root first, requestor last.
        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(gp_prov, grandparent_id(), "openai", ProviderStatus::Active),
                make_provider(parent_prov, parent_id(), "openai", ProviderStatus::Active),
                make_provider(child_prov, child_id(), "openai", ProviderStatus::Active),
            ],
        );

        assert!(
            chain.get(child_prov).expect("child provider").winner,
            "the closest tenant wins regardless of arrival order"
        );
        assert!(!chain.get(parent_prov).expect("parent provider").winner);
        assert!(!chain.get(gp_prov).expect("grandparent provider").winner);
        assert_eq!(chain.allow_list(), &[child_prov]);
    }

    #[test]
    fn test_chain_providers_parent_shadows_grandparent() {
        let ctx = InheritanceContext::new(parent_id(), vec![grandparent_id()]);
        let parent_prov_id = pid("d0");
        let gp_prov_id = pid("d1");

        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(
                    parent_prov_id,
                    parent_id(),
                    "openai",
                    ProviderStatus::Active,
                ),
                make_provider(
                    gp_prov_id,
                    grandparent_id(),
                    "openai",
                    ProviderStatus::Active,
                ),
            ],
        );

        assert!(chain.get(parent_prov_id).expect("parent provider").winner);
        assert!(
            !chain.get(gp_prov_id).expect("grandparent provider").winner,
            "grandparent should lose to parent"
        );
        assert_eq!(chain.allow_list(), &[parent_prov_id]);
    }

    #[test]
    fn test_chain_providers_unrelated_slugs_coexist() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());
        let child_only_id = pid("e0");
        let parent_only_id = pid("e1");

        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(
                    child_only_id,
                    child_id(),
                    "child-only",
                    ProviderStatus::Active,
                ),
                make_provider(
                    parent_only_id,
                    parent_id(),
                    "parent-only",
                    ProviderStatus::Active,
                ),
            ],
        );

        // Both slugs should be winners with no collisions.
        assert_eq!(chain.by_id.len(), 2);
        assert_eq!(chain.allow_list().len(), 2);
    }

    #[test]
    fn test_chain_providers_disabled_shadow() {
        // A disabled shadow must still win its slug (so the ancestor's models
        // are excluded), but be absent from the allow-list (so its own models
        // are excluded too). §3.5 rationale: folding status into `winner` hands
        // the slug back to the ancestor and re-exposes the shadowed models.
        let ctx = InheritanceContext::new(child_id(), single_ancestor());
        let child_prov_id = pid("f0");
        let parent_prov_id = pid("f1");

        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(
                    child_prov_id,
                    child_id(),
                    "openai",
                    ProviderStatus::Disabled,
                ),
                make_provider(
                    parent_prov_id,
                    parent_id(),
                    "openai",
                    ProviderStatus::Active,
                ),
            ],
        );

        // Child (disabled) must still win the slug.
        let child = chain.get(child_prov_id).expect("child provider");
        assert!(child.winner, "disabled shadow must still win the slug");
        assert_eq!(child.owner_tenant, child_id());

        // Parent must be a loser.
        assert!(
            !chain.get(parent_prov_id).expect("parent provider").winner,
            "parent must lose to disabled child"
        );

        // Neither should be in the allow-list (child is disabled, parent lost).
        assert_eq!(chain.allow_list().len(), 0, "no active winners");
        assert!(!chain.is_allowed(child_prov_id));
        assert!(!chain.is_allowed(parent_prov_id));
    }

    /// The scope already excludes them, so this only fires on a widened scope —
    /// which is exactly when silently trusting the row would leak across tenants.
    #[test]
    fn test_chain_providers_drops_a_provider_owned_outside_the_chain() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());
        let own = pid("01");
        let stranger = pid("02");

        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(own, child_id(), "openai", ProviderStatus::Active),
                make_provider(stranger, outsider_id(), "anthropic", ProviderStatus::Active),
            ],
        );

        assert_eq!(chain.by_id.len(), 1);
        assert!(chain.get(stranger).is_none());
        assert_eq!(chain.allow_list(), &[own]);
    }

    /// A malformed chain that repeats a tenant must keep it at its closest
    /// position rather than demoting it.
    #[test]
    fn test_chain_providers_survives_a_chain_that_repeats_a_tenant() {
        let ctx = InheritanceContext::new(child_id(), vec![parent_id(), child_id()]);
        let child_prov = pid("11");
        let parent_prov = pid("12");

        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(parent_prov, parent_id(), "openai", ProviderStatus::Active),
                make_provider(child_prov, child_id(), "openai", ProviderStatus::Active),
            ],
        );

        assert!(chain.get(child_prov).expect("child provider").winner);
        assert!(!chain.get(parent_prov).expect("parent provider").winner);
    }

    // ── Tests: winner_for_slug ─────────────────────────────────────────────

    #[test]
    fn test_winner_for_slug_returns_the_closest_owner() {
        let ctx = InheritanceContext::new(child_id(), make_ancestor_ids());
        let child_prov = pid("21");
        let parent_prov = pid("22");

        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(parent_prov, parent_id(), "openai", ProviderStatus::Active),
                make_provider(child_prov, child_id(), "openai", ProviderStatus::Active),
            ],
        );

        let winner = chain.winner_for_slug("openai").expect("slug resolves");
        assert_eq!(winner.id, child_prov);
        assert_eq!(winner.owner_tenant, child_id());
    }

    /// The single read needs the winner even when disabled, so the gate order
    /// can report `ProviderDisabled` only after the model is found.
    #[test]
    fn test_winner_for_slug_returns_a_disabled_winner() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());
        let child_prov = pid("31");
        let parent_prov = pid("32");

        let chain = build_chain_providers(
            &ctx,
            vec![
                make_provider(child_prov, child_id(), "openai", ProviderStatus::Disabled),
                make_provider(parent_prov, parent_id(), "openai", ProviderStatus::Active),
            ],
        );

        let winner = chain.winner_for_slug("openai").expect("slug resolves");
        assert_eq!(winner.id, child_prov);
        assert_eq!(winner.status, ProviderStatus::Disabled);
        assert!(chain.allow_list().is_empty());
    }

    #[test]
    fn test_winner_for_slug_is_none_for_an_unknown_slug() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());
        let chain = build_chain_providers(
            &ctx,
            vec![make_provider(
                pid("41"),
                child_id(),
                "openai",
                ProviderStatus::Active,
            )],
        );
        assert!(chain.winner_for_slug("anthropic").is_none());
    }

    #[test]
    fn test_chain_providers_empty_input_yields_empty_allow_list() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());
        let chain = build_chain_providers(&ctx, vec![]);
        assert!(chain.allow_list().is_empty());
        assert!(chain.winner_for_slug("openai").is_none());
    }
}
