//! Tenant inheritance resolver helper.
//!
//! Implements DESIGN §2.1 "Additive Inheritance": providers/models are visible
//! to a tenant if they are owned by that tenant **or** any of its ancestors.
//! When a parent and child both have a resource with the same key (provider
//! slug or model `canonical_id`), the child's version **shadows** the parent's.
//!
//! Ownership classification drives cache TTL selection — own entries are
//! cached longer (`own_ttl_seconds`, default 30 min) because they change
//! less frequently than inherited entries, which may change at any time
//! in the ancestor tenant (`inherited_ttl_seconds`, default 5 min).
//!
//! [`merge_inherited_page`] applies the same rule to paged list reads, so
//! every list endpoint shares one implementation of the merge.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::Hash;

use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, TenantId, TenantRef, TenantResolverClient,
};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::error::DomainError;
use crate::ProviderV1;
use crate::config::ModelRegistryConfig;
use model_registry_sdk::models::ProviderStatus;

// ---------------------------------------------------------------------------
// Ownership
// ---------------------------------------------------------------------------

/// Classification of a resource's ownership relative to the requesting tenant.
///
/// Used to select the cache TTL for a cached entry (own entries have a longer
/// TTL because they change less frequently from the requestor's perspective).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[domain_model]
pub enum Ownership {
    /// Resource is directly owned by the requesting tenant.
    Own,
    /// Resource is inherited from an ancestor tenant.
    Inherited,
}

impl Ownership {
    /// Returns `true` when the resource is owned directly.
    #[must_use]
    pub const fn is_own(self) -> bool {
        matches!(self, Self::Own)
    }

    /// Returns `true` when the resource is inherited from an ancestor.
    #[must_use]
    pub const fn is_inherited(self) -> bool {
        matches!(self, Self::Inherited)
    }
}

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
    #[must_use]
    pub fn allow_list(&self) -> &[Uuid] {
        &self.allow_list
    }

    /// Return the slice of `allow_list` belonging to a specific tenant.
    ///
    /// Used by the listing path to pass only the relevant ids to each chain
    /// tenant's repository query. Returns an empty slice when the tenant has
    /// no winning active providers.
    #[must_use]
    pub fn allow_slice_for(&self, tenant_id: Uuid) -> Vec<Uuid> {
        self.by_id
            .values()
            .filter(|p| {
                p.owner_tenant == tenant_id && p.winner && p.status == ProviderStatus::Active
            })
            .map(|p| p.id)
            .collect()
    }

    /// Returns `true` when `provider_id` is in the eval-path allow-list.
    #[must_use]
    pub fn is_allowed(&self, provider_id: Uuid) -> bool {
        self.allow_list.contains(&provider_id)
    }
}

/// Build the complete [`ChainProviders`] for the given tenant chain.
///
/// `list_all` is invoked once per chain tenant (closest first), each call
/// returning that tenant's **complete** provider set. Any query error fails
/// the whole construction closed — a skipped ancestor provider query would
/// silently un-shadow an ancestor (B5).
///
/// The winner is the closest chain tenant owning a given slug — ownership
/// only, status is not a factor.
pub async fn build_chain_providers<L, Fut>(
    inheritance: &InheritanceContext,
    list_all: L,
) -> Result<ChainProviders, DomainError>
where
    L: Fn(AccessScope) -> Fut,
    Fut: Future<Output = Result<Vec<ProviderV1>, DomainError>>,
{
    // Collect all providers from every tenant in the chain, closest first.
    let mut all_providers: Vec<(Uuid, ProviderV1)> = Vec::new();

    // Own tenant first.
    {
        let own_scope = AccessScope::for_tenant(inheritance.tenant_id());
        let providers = list_all(own_scope).await?;
        all_providers.extend(providers.into_iter().map(|p| (inheritance.tenant_id(), p)));
    }

    // Ancestors in chain order.
    for ancestor in &inheritance.ancestors {
        let ancestor_id = ancestor.id.0;
        let scope = AccessScope::for_tenant(ancestor_id);
        let providers = list_all(scope).await?;
        all_providers.extend(providers.into_iter().map(|p| (ancestor_id, p)));
    }

    // Determine winners: closest tenant owning a slug wins (ownership only).
    let mut winners: HashMap<String, Uuid> = HashMap::new();
    // Since all_providers is in chain order (closest first), the first
    // occurrence of each slug wins.
    for (tenant_id, provider) in &all_providers {
        winners.entry(provider.slug.clone()).or_insert(*tenant_id);
    }

    // Build the by_id map, tagging each provider with its winner status.
    let mut by_id: HashMap<Uuid, ChainProvider> = HashMap::with_capacity(all_providers.len());
    for (tenant_id, provider) in all_providers {
        let winner = winners.get(&provider.slug) == Some(&tenant_id);
        by_id.insert(
            provider.id,
            ChainProvider {
                id: provider.id,
                owner_tenant: tenant_id,
                slug: provider.slug.clone(),
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

    Ok(ChainProviders { by_id, allow_list })
}

/// Resolved ancestor chain for a tenant, providing helper methods to
/// classify ownership and compute additive visibility with child-shadowing.
#[derive(Debug, Clone)]
#[domain_model]
pub struct InheritanceContext {
    /// Ancestor tenant chain from direct parent to root.
    pub ancestors: Vec<TenantRef>,
    /// The requesting tenant's ID.
    tenant_id: Uuid,
    /// Full chain from closest (self) to root: `[self, parent, grandparent, ...]`.
    chain_ids: Vec<Uuid>,
    /// Distance of each chain tenant from the requestor: `0` is self, `1` the
    /// direct parent, and so on. Drives both membership checks and the
    /// closest-wins ordering in [`Self::apply_additive_visibility`].
    pos_in_chain: HashMap<Uuid, usize>,
}

impl InheritanceContext {
    /// Build a new context from the requesting tenant's ID and its ancestor chain.
    ///
    /// `ancestors` should be ordered from direct parent to root (as returned by
    /// [`TenantResolverClient::get_ancestors`]).
    #[must_use]
    pub fn new(tenant_id: Uuid, ancestors: Vec<TenantRef>) -> Self {
        let mut chain_ids = Vec::with_capacity(1 + ancestors.len());
        chain_ids.push(tenant_id);
        chain_ids.extend(ancestors.iter().map(|a| a.id.0));

        // Keep the *first* (closest) position for each tenant, so a malformed
        // chain that repeats a tenant cannot demote it to a farther position.
        let mut pos_in_chain: HashMap<Uuid, usize> = HashMap::with_capacity(chain_ids.len());
        for (pos, id) in chain_ids.iter().enumerate() {
            pos_in_chain.entry(*id).or_insert(pos);
        }

        Self {
            ancestors,
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

    /// Returns `true` when the given `TenantRef` is an ancestor of the
    /// requesting tenant.
    ///
    /// Position `0` in the chain is the requestor itself, so only positions
    /// greater than zero are ancestors.
    #[must_use]
    pub fn is_ancestor(&self, candidate_id: Uuid) -> bool {
        self.pos_in_chain
            .get(&candidate_id)
            .is_some_and(|&pos| pos > 0)
    }

    /// Classify a resource by its owning tenant ID.
    ///
    /// Returns [`Ownership::Own`] when `owner_tenant_id` matches the
    /// requesting tenant, [`Ownership::Inherited`] when it matches an
    /// ancestor, and [`Ownership::Inherited`] for unknown tenants (they
    /// would not normally appear in additive-visibility results but are
    /// treated as inherited for safety).
    #[must_use]
    pub fn classify(&self, owner_tenant_id: Uuid) -> Ownership {
        if owner_tenant_id == self.tenant_id {
            Ownership::Own
        } else {
            Ownership::Inherited
        }
    }

    /// Apply additive visibility with child-shadowing.
    ///
    /// Given items tagged with their owning tenant ID, this function:
    /// 1. Takes the **additive union** of all items (self + ancestors).
    /// 2. Where two items share the same key (as produced by `key_fn`),
    ///    the **closer tenant** (child over parent) wins — i.e., child
    ///    shadowing.
    ///
    /// Items whose owning tenant is neither the requestor nor an ancestor
    /// are silently excluded.
    ///
    /// # Returns
    ///
    /// A `Vec` of `(Ownership, T)` pairs, with child-shadowing applied.
    /// Items are returned in the order of the chain (own items first, then
    /// parent, then grandparent, etc.).
    pub fn apply_additive_visibility<T, F, K>(
        &self,
        items: Vec<(Uuid, T)>,
        key_fn: F,
    ) -> Vec<(Ownership, T)>
    where
        F: Fn(&T) -> K,
        K: Eq + Hash,
    {
        // Index items by chain position, filter out tenants not in chain.
        let mut indexed: Vec<(usize, Uuid, T)> = items
            .into_iter()
            .filter_map(|(tid, item)| self.pos_in_chain.get(&tid).map(|&pos| (pos, tid, item)))
            .collect();

        // Sort by chain position: closest tenant first.
        indexed.sort_by_key(|(pos, _, _)| *pos);

        // Apply child-shadowing: keep only the first (closest) occurrence of
        // each key.
        let mut seen = HashSet::new();
        let mut result = Vec::with_capacity(indexed.len());

        for (_pos, tid, item) in indexed {
            let key = key_fn(&item);
            if seen.insert(key) {
                let ownership = self.classify(tid);
                result.push((ownership, item));
            }
        }

        result
    }
}

// ---------------------------------------------------------------------------
// find_in_chain
// ---------------------------------------------------------------------------

/// Search the tenant chain closest-first for a single resource, returning the
/// tenant that owns the first match.
///
/// `fetch` is invoked with the caller's own scope first, then with a scope
/// narrowed to each ancestor in turn, stopping at the first hit — so the closest
/// tenant shadows the rest. An error satisfying `is_not_found` advances the
/// search to the next tenant; any other error aborts it.
///
/// `Ok(None)` means no tenant in the chain holds the resource. The caller
/// raises its own not-found error, since the identifying key differs per
/// resource type.
///
/// Ancestor scopes are built here rather than widening the caller's own scope,
/// per the tenant-isolation principle (DESIGN §2.1).
///
/// # Errors
///
/// Propagates any error from `fetch` that `is_not_found` rejects.
pub async fn find_in_chain<T, L, Fut>(
    inheritance: &InheritanceContext,
    own_scope: &AccessScope,
    is_not_found: fn(&DomainError) -> bool,
    fetch: L,
) -> Result<Option<(Uuid, T)>, DomainError>
where
    L: Fn(AccessScope) -> Fut,
    Fut: Future<Output = Result<T, DomainError>>,
{
    let candidates = std::iter::once((inheritance.tenant_id(), own_scope.clone())).chain(
        inheritance
            .ancestors
            .iter()
            .map(|a| (a.id.0, AccessScope::for_tenant(a.id.0))),
    );

    for (tenant_id, scope) in candidates {
        match fetch(scope).await {
            Ok(item) => return Ok(Some((tenant_id, item))),
            Err(e) if is_not_found(&e) => {}
            Err(e) => return Err(e),
        }
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// AncestorFailure — error propagation policy for ancestor queries
// ---------------------------------------------------------------------------

/// Controls how ancestor query failures are handled in
/// [`merge_inherited_page`].
///
/// The two modes reflect an intentional asymmetry (DESIGN §3.5 sub-decision 4):
/// dropping ancestor **model** rows only narrows what the caller sees, while
/// skipping an ancestor **provider** row widens visibility by silently
/// un-shadowing an earlier ancestor (B5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[domain_model]
pub enum AncestorFailure {
    /// Log the error and continue with partial results. Use for model queries
    /// where a skipped ancestor only narrows the result set.
    Skip,
    /// Propagate the error immediately. Use for provider queries where a
    /// skipped ancestor would un-shadow an ancestor provider.
    FailClosed,
}

// ---------------------------------------------------------------------------
// merge_inherited_page
// ---------------------------------------------------------------------------

/// Merge a tenant's own page of results with the inherited set from every
/// ancestor, applying child-shadowing by `key_fn`.
///
/// `own_page` is the result of the caller's own-tenant query, which carries the
/// full `OData` query including pagination. `list_for_scope` is invoked once per
/// ancestor with an [`AccessScope`] narrowed to that ancestor and a copy of the
/// caller's query with pagination removed — shadowing must be computed over the
/// ancestor's complete matching set, not its first page. The merged result is
/// then truncated back to the caller's limit.
///
/// `ancestor_failure` controls the error-propagation policy:
/// - [`AncestorFailure::Skip`]: log and skip a failing ancestor, yielding
///   partial results (correct for model queries — a skipped ancestor only
///   narrows the caller's view).
/// - [`AncestorFailure::FailClosed`]: propagate the error immediately (correct
///   for provider queries — a skipped ancestor would un-shadow an ancestor and
///   *widen* the caller's view).
///
/// Ancestor scopes are built here rather than widening the caller's own scope,
/// per the tenant-isolation principle (DESIGN §2.1).
pub async fn merge_inherited_page<T, K, F, L, Fut>(
    inheritance: &InheritanceContext,
    own_page: Page<T>,
    query: &ODataQuery,
    key_fn: F,
    ancestor_failure: AncestorFailure,
    list_for_scope: L,
) -> Result<Page<T>, DomainError>
where
    F: Fn(&T) -> K,
    K: Eq + Hash,
    L: Fn(Uuid, AccessScope, ODataQuery) -> Fut,
    Fut: Future<Output = Option<Result<Page<T>, DomainError>>>,
{
    let Page { items, page_info } = own_page;

    // Tag every row with the tenant whose scope produced it, so shadowing can
    // resolve collisions by chain distance.
    let own_tenant_id = inheritance.tenant_id();
    let mut tagged: Vec<(Uuid, T)> = items.into_iter().map(|it| (own_tenant_id, it)).collect();

    for ancestor in &inheritance.ancestors {
        let ancestor_id = ancestor.id.0;
        // Same filter/order/select as the caller, without pagination.
        let ancestor_query = ODataQuery {
            filter: query.filter.clone(),
            filter_hash: query.filter_hash.clone(),
            order: query.order.clone(),
            select: query.select.clone(),
            ..ODataQuery::default()
        };

        match list_for_scope(
            ancestor_id,
            AccessScope::for_tenant(ancestor_id),
            ancestor_query,
        )
        .await
        {
            Some(Ok(ancestor_page)) => {
                tagged.extend(ancestor_page.items.into_iter().map(|it| (ancestor_id, it)));
            }
            Some(Err(e)) => match ancestor_failure {
                AncestorFailure::Skip => {
                    tracing::warn!(
                        error = %e,
                        ancestor_tenant_id = %ancestor_id,
                        "ancestor list query failed, continuing with partial results"
                    );
                }
                AncestorFailure::FailClosed => return Err(e),
            },
            None => {
                // Caller returned None — skip this ancestor (e.g. empty
                // allow-list slice). No merge, no error.
            }
        }
    }

    let mut merged: Vec<T> = inheritance
        .apply_additive_visibility(tagged, key_fn)
        .into_iter()
        .map(|(_ownership, item)| item)
        .collect();

    if let Some(limit) = query.limit {
        merged.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }

    Ok(Page {
        items: merged,
        page_info,
    })
}

// ---------------------------------------------------------------------------
// resolve_ancestors
// ---------------------------------------------------------------------------

/// Resolve the ancestor chain for the tenant identified by
/// `ctx.subject_tenant_id()` using the given `resolver`.
///
/// Respects barrier boundaries (self-managed tenants). The returned
/// [`InheritanceContext`] can be used to classify resources and apply
/// additive visibility with child-shadowing.
///
/// # Errors
///
/// Returns [`DomainError::Internal`] when the tenant-resolver call fails
/// (network error, invalid response, etc.).
pub async fn resolve_ancestors<T: TenantResolverClient + ?Sized>(
    resolver: &T,
    ctx: &SecurityContext,
) -> Result<InheritanceContext, DomainError> {
    let tenant_id = ctx.subject_tenant_id();
    let tenant_id_sdk = TenantId(tenant_id);

    let response = resolver
        .get_ancestors(
            ctx,
            tenant_id_sdk,
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Respect,
            },
        )
        .await
        .map_err(|e| DomainError::internal_from("tenant-resolver ancestors call failed", e))?;

    Ok(InheritanceContext::new(tenant_id, response.ancestors))
}

// ---------------------------------------------------------------------------
// cache_ttl_seconds
// ---------------------------------------------------------------------------

/// Select the appropriate cache TTL based on ownership.
///
/// Own entries use the config's `own_ttl_seconds` (default 30 min);
/// inherited entries use `inherited_ttl_seconds` (default 5 min) because
/// they can change at any time in the ancestor tenant.
#[must_use]
pub fn cache_ttl_seconds(ownership: Ownership, config: &ModelRegistryConfig) -> u64 {
    match ownership {
        Ownership::Own => config.own_ttl_seconds,
        Ownership::Inherited => config.inherited_ttl_seconds,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tenant_resolver_sdk::{
        GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse, GetTenantsOptions,
        IsAncestorOptions, TenantInfo, TenantResolverError, TenantStatus,
    };

    // ── Mock TenantResolverClient ─────────────────────────────────────────

    #[domain_model]
    struct MockTenantResolver {
        ancestors: Vec<TenantRef>,
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
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
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
        assert!(ctx.ancestors.is_empty());
    }

    #[test]
    fn test_is_ancestor_returns_true_for_ancestors() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        assert!(ctx.is_ancestor(parent_id()));
        assert!(ctx.is_ancestor(grandparent_id()));
    }

    #[test]
    fn test_is_ancestor_returns_false_for_self() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        assert!(!ctx.is_ancestor(child_id()));
    }

    #[test]
    fn test_is_ancestor_returns_false_for_unknown() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        assert!(!ctx.is_ancestor(Uuid::nil()));
    }

    // ── Tests: Ownership classification ────────────────────────────────────

    #[test]
    fn test_classify_own_when_tenant_matches() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        assert_eq!(ctx.classify(child_id()), Ownership::Own);
    }

    #[test]
    fn test_classify_inherited_when_ancestor() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        assert_eq!(ctx.classify(parent_id()), Ownership::Inherited);
        assert_eq!(ctx.classify(grandparent_id()), Ownership::Inherited);
    }

    #[test]
    fn test_classify_inherited_when_unknown() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        // Unknown tenants default to inherited for safety.
        assert_eq!(ctx.classify(Uuid::nil()), Ownership::Inherited);
    }

    // ── Tests: Ownership predicates ────────────────────────────────────────

    #[test]
    fn test_ownership_is_own() {
        assert!(Ownership::Own.is_own());
        assert!(!Ownership::Inherited.is_own());
    }

    #[test]
    fn test_ownership_is_inherited() {
        assert!(Ownership::Inherited.is_inherited());
        assert!(!Ownership::Own.is_inherited());
    }

    // ── Tests: Additive visibility with child-shadowing (providers by slug) ─

    #[test]
    fn test_additive_visibility_empty() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        let result = ctx.apply_additive_visibility::<&str, _, _>(vec![], |s| *s);
        assert!(result.is_empty());
    }

    #[test]
    fn test_additive_visibility_own_only() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        let items = vec![(child_id(), "openai"), (child_id(), "anthropic")];
        let result = ctx.apply_additive_visibility(items, |s| *s);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, "openai");
        assert_eq!(result[0].0, Ownership::Own);
        assert_eq!(result[1].1, "anthropic");
        assert_eq!(result[1].0, Ownership::Own);
    }

    #[test]
    fn test_additive_visibility_union_with_ancestors() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        let items = vec![
            // Child's own providers
            (child_id(), "child-only"),
            // Parent's providers
            (parent_id(), "parent-only"),
            // Grandparent's providers
            (grandparent_id(), "grandparent-only"),
        ];
        let result = ctx.apply_additive_visibility(items, |s| *s);
        // All three should be visible (additive union).
        let slugs: Vec<&str> = result.iter().map(|(_, s)| *s).collect();
        assert!(slugs.contains(&"child-only"));
        assert!(slugs.contains(&"parent-only"));
        assert!(slugs.contains(&"grandparent-only"));
    }

    #[test]
    fn test_additive_visibility_child_shadows_parent_by_slug() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        let items = vec![
            // Parent has "openai"
            (parent_id(), "openai"),
            // Child has "openai" with different name — child should shadow parent
            (child_id(), "openai"),
            (child_id(), "anthropic"),
            // Grandparent also has "openai" — should be fully shadowed
            (grandparent_id(), "openai"),
        ];
        let result = ctx.apply_additive_visibility(items, |s| *s);
        // "openai" should appear exactly once, and it should be Own (child's version)
        assert_eq!(result.len(), 2);
        let openai_entry = result.iter().find(|(_, s)| *s == "openai").unwrap();
        assert_eq!(openai_entry.0, Ownership::Own);
        let anthropic_entry = result.iter().find(|(_, s)| *s == "anthropic").unwrap();
        assert_eq!(anthropic_entry.0, Ownership::Own);
    }

    #[test]
    fn test_additive_visibility_child_cannot_expand_beyond_parent() {
        // Setup: child has only one ancestor (parent).
        let parent = TenantRef {
            id: TenantId(parent_id()),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: None,
            self_managed: false,
        };
        let ctx = InheritanceContext::new(child_id(), vec![parent]);

        // Parent has "openai", child has nothing.
        let items = vec![(parent_id(), "openai")];
        let result = ctx.apply_additive_visibility(items, |s| *s);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "openai");
        assert_eq!(result[0].0, Ownership::Inherited);

        // Try to inject an item from a non-existent ancestor — should be excluded.
        let items = vec![
            (parent_id(), "parent-only"),
            (Uuid::nil(), "unknown-tenant-item"),
        ];
        let result = ctx.apply_additive_visibility(items, |s| *s);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "parent-only");
    }

    #[test]
    fn test_additive_visibility_child_cannot_see_parent_of_parent_when_missing() {
        // Single-tenant case: no ancestors at all.
        let ctx = InheritanceContext::new(child_id(), vec![]);
        let items = vec![
            (child_id(), "own-provider"),
            // Grandparent items should be excluded since grandparent is not in chain.
            (grandparent_id(), "gp-provider"),
        ];
        let result = ctx.apply_additive_visibility(items, |s| *s);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "own-provider");
        assert_eq!(result[0].0, Ownership::Own);
    }

    // ── Tests: find_in_chain ───────────────────────────────────────────────

    fn provider_missing() -> DomainError {
        DomainError::provider_not_found(Uuid::nil())
    }

    fn is_provider_missing(e: &DomainError) -> bool {
        matches!(e, DomainError::ProviderNotFound { .. })
    }

    #[tokio::test]
    async fn test_find_in_chain_stops_at_own_tenant() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        let calls = AtomicUsize::new(0);

        let found = find_in_chain(
            &ctx,
            &AccessScope::for_tenant(child_id()),
            is_provider_missing,
            |_scope| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok("hit") }
            },
        )
        .await
        .unwrap();

        assert_eq!(found, Some((child_id(), "hit")));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "ancestors must not be queried once the own tenant matches"
        );
    }

    #[tokio::test]
    async fn test_find_in_chain_falls_through_to_closest_ancestor() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());

        let found = find_in_chain(
            &ctx,
            &AccessScope::for_tenant(child_id()),
            is_provider_missing,
            |scope| async move {
                if scope_targets(&scope, parent_id()) {
                    Ok("from-parent")
                } else if scope_targets(&scope, grandparent_id()) {
                    Ok("from-grandparent")
                } else {
                    Err(provider_missing())
                }
            },
        )
        .await
        .unwrap();

        // Both ancestors hold the resource; the closer one wins.
        assert_eq!(found, Some((parent_id(), "from-parent")));
    }

    #[tokio::test]
    async fn test_find_in_chain_returns_none_when_chain_is_exhausted() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        let calls = AtomicUsize::new(0);

        let found = find_in_chain(
            &ctx,
            &AccessScope::for_tenant(child_id()),
            is_provider_missing,
            |_scope| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err::<&str, _>(provider_missing()) }
            },
        )
        .await
        .unwrap();

        assert_eq!(found, None);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "every tenant in the chain should have been tried"
        );
    }

    #[tokio::test]
    async fn test_find_in_chain_aborts_on_unrelated_error() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        let calls = AtomicUsize::new(0);

        let result = find_in_chain(
            &ctx,
            &AccessScope::for_tenant(child_id()),
            is_provider_missing,
            |_scope| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err::<&str, _>(DomainError::internal("database unavailable")) }
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a non-not-found error must abort the walk, not skip the tenant"
        );
    }

    #[tokio::test]
    async fn test_find_in_chain_uses_caller_scope_for_own_tenant() {
        let ctx = InheritanceContext::new(child_id(), vec![]);
        // The caller's PDP scope may be broader than a single tenant; it must
        // reach the fetch untouched rather than be rebuilt from the chain.
        let own_scope = AccessScope::for_tenants(vec![child_id(), parent_id()]);
        let expected = own_scope.clone();

        let found = find_in_chain(&ctx, &own_scope, is_provider_missing, move |scope| {
            let is_caller_scope = scope == expected;
            async move {
                if is_caller_scope {
                    Ok("hit")
                } else {
                    Err(provider_missing())
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(found, Some((child_id(), "hit")));
    }

    // ── Tests: merge_inherited_page ────────────────────────────────────────

    fn page_of(items: &[&str]) -> Page<String> {
        Page {
            items: items.iter().map(|s| (*s).to_owned()).collect(),
            page_info: toolkit_odata::PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 50,
            },
        }
    }

    fn scope_targets(scope: &AccessScope, tenant: Uuid) -> bool {
        scope.contains_uuid(toolkit_security::pep_properties::OWNER_TENANT_ID, tenant)
    }

    #[tokio::test]
    async fn test_merge_inherited_page_skips_ancestor_queries_when_no_ancestors() {
        let ctx = InheritanceContext::new(child_id(), vec![]);
        let calls = AtomicUsize::new(0);

        let result = merge_inherited_page(
            &ctx,
            page_of(&["a", "b"]),
            &ODataQuery::default(),
            |s: &String| s.clone(),
            AncestorFailure::Skip,
            |_tenant_id, _scope, _q| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Some(Ok(page_of(&[]))) }
            },
        )
        .await
        .expect("merge should succeed");

        assert_eq!(result.items, ["a", "b"]);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_merge_inherited_page_unions_with_closest_wins() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());

        let result = merge_inherited_page(
            &ctx,
            page_of(&["own-only", "shared"]),
            &ODataQuery::default(),
            |s: &String| s.clone(),
            AncestorFailure::Skip,
            |_tenant_id, scope, _q| async move {
                if scope_targets(&scope, parent_id()) {
                    Some(Ok(page_of(&["shared", "from-parent"])))
                } else if scope_targets(&scope, grandparent_id()) {
                    Some(Ok(page_of(&["shared", "from-parent", "gp-only"])))
                } else {
                    panic!("ancestor scope must target exactly one chain tenant")
                }
            },
        )
        .await
        .expect("merge should succeed");

        // `shared` resolves to the child, `from-parent` to the parent rather
        // than the grandparent, and each key appears exactly once.
        assert_eq!(
            result.items,
            ["own-only", "shared", "from-parent", "gp-only"]
        );
    }

    #[tokio::test]
    async fn test_merge_inherited_page_keeps_partial_results_on_ancestor_failure() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());

        let result = merge_inherited_page(
            &ctx,
            page_of(&["own"]),
            &ODataQuery::default(),
            |s: &String| s.clone(),
            AncestorFailure::Skip,
            |_tenant_id, scope, _q| async move {
                if scope_targets(&scope, parent_id()) {
                    Some(Err(DomainError::internal("parent unavailable")))
                } else {
                    Some(Ok(page_of(&["gp"])))
                }
            },
        )
        .await
        .expect("merge with Skip should return partial results");

        assert_eq!(result.items, ["own", "gp"]);
    }

    #[tokio::test]
    async fn test_merge_inherited_page_fail_closed_propagates_ancestor_error() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());

        let result = merge_inherited_page(
            &ctx,
            page_of(&["own"]),
            &ODataQuery::default(),
            |s: &String| s.clone(),
            AncestorFailure::FailClosed,
            |_tenant_id, scope, _q| async move {
                if scope_targets(&scope, parent_id()) {
                    Some(Err(DomainError::internal("provider query failed")))
                } else {
                    Some(Ok(page_of(&["gp"])))
                }
            },
        )
        .await;

        let err = result.expect_err("FailClosed should propagate the error");
        assert!(
            err.to_string().contains("provider query failed"),
            "expected error about provider query, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_merge_inherited_page_fail_closed_stops_at_first_failure() {
        // When FailClosed is used, only the first ancestor failure should be
        // propagated — the grandparent should never be queried.
        let ctx = InheritanceContext::new(child_id(), make_ancestors());

        let parent_queried = Arc::new(AtomicUsize::new(0));
        let grandparent_queried = Arc::new(AtomicUsize::new(0));

        let pq = Arc::clone(&parent_queried);
        let gq = Arc::clone(&grandparent_queried);

        let result = merge_inherited_page(
            &ctx,
            page_of(&["own"]),
            &ODataQuery::default(),
            |s: &String| s.clone(),
            AncestorFailure::FailClosed,
            move |_tenant_id, scope, _q| {
                let pq = Arc::clone(&pq);
                let gq = Arc::clone(&gq);
                async move {
                    if scope_targets(&scope, parent_id()) {
                        pq.fetch_add(1, Ordering::SeqCst);
                        Some(Err(DomainError::internal("parent query failed")))
                    } else if scope_targets(&scope, grandparent_id()) {
                        gq.fetch_add(1, Ordering::SeqCst);
                        Some(Ok(page_of(&["gp"])))
                    } else {
                        panic!("unexpected scope")
                    }
                }
            },
        )
        .await;

        assert!(result.is_err(), "FailClosed should propagate error");
        assert_eq!(
            parent_queried.load(Ordering::SeqCst),
            1,
            "parent should have been queried"
        );
        assert_eq!(
            grandparent_queried.load(Ordering::SeqCst),
            0,
            "grandparent should NOT be queried after parent failure with FailClosed"
        );
    }

    #[tokio::test]
    async fn test_merge_inherited_page_skip_logs_and_continues_after_failure() {
        // With Skip, a failing parent does not stop the grandparent from being
        // queried.
        let ctx = InheritanceContext::new(child_id(), make_ancestors());

        let result = merge_inherited_page(
            &ctx,
            page_of(&["own"]),
            &ODataQuery::default(),
            |s: &String| s.clone(),
            AncestorFailure::Skip,
            |_tenant_id, scope, _q| async move {
                if scope_targets(&scope, parent_id()) {
                    Some(Err(DomainError::internal("parent unavailable")))
                } else {
                    Some(Ok(page_of(&["gp"])))
                }
            },
        )
        .await
        .expect("Skip should return partial results");

        // own + grandparent (parent skipped)
        assert_eq!(result.items, ["own", "gp"]);
    }

    #[tokio::test]
    async fn test_merge_inherited_page_strips_ancestor_pagination_then_truncates() {
        let ctx = InheritanceContext::new(child_id(), make_ancestors());
        let query = ODataQuery {
            limit: Some(3),
            cursor: None,
            select: Some(vec!["slug".to_owned()]),
            ..ODataQuery::default()
        };

        let result = merge_inherited_page(
            &ctx,
            page_of(&["a", "b"]),
            &query,
            |s: &String| s.clone(),
            AncestorFailure::Skip,
            |_tenant_id, _scope, ancestor_query| async move {
                // Ancestors must be queried without pagination so shadowing
                // sees the complete inherited set, but keep projection.
                assert!(ancestor_query.limit.is_none());
                assert!(ancestor_query.cursor.is_none());
                assert_eq!(ancestor_query.select, Some(vec!["slug".to_owned()]));
                Some(Ok(page_of(&["c", "d"])))
            },
        )
        .await
        .expect("merge should succeed");

        // 2 own + 2 from each of 2 ancestors, deduped to 4, truncated to 3.
        assert_eq!(result.items, ["a", "b", "c"]);
    }

    // ── Tests: Single-tenant case (no ancestors) ──────────────────────────

    #[test]
    fn test_single_tenant_no_ancestors_resolve_correctly() {
        let ctx = InheritanceContext::new(child_id(), vec![]);
        assert!(ctx.ancestors.is_empty());
        assert_eq!(ctx.chain_ids.len(), 1);
        assert_eq!(ctx.chain_ids[0], child_id());
        assert!(!ctx.is_ancestor(parent_id()));
        assert!(!ctx.is_ancestor(grandparent_id()));
    }

    // ── Tests: resolve_ancestors ──────────────────────────────────────────

    #[tokio::test]
    async fn test_resolve_ancestors_with_mock() {
        let resolver = MockTenantResolver {
            ancestors: make_ancestors(),
        };
        let ctx = SecurityContext::anonymous();
        let result = resolve_ancestors(&resolver, &ctx).await;
        assert!(result.is_ok());

        let inheritance = result.unwrap();
        assert_eq!(inheritance.ancestors.len(), 2);
        // ancestors are ordered direct-parent-first
        assert_eq!(inheritance.ancestors[0].id.0, parent_id());
        assert_eq!(inheritance.ancestors[1].id.0, grandparent_id());
    }

    #[tokio::test]
    async fn test_resolve_ancestors_no_ancestors() {
        let resolver = MockTenantResolver { ancestors: vec![] };
        let ctx = SecurityContext::anonymous();
        let result = resolve_ancestors(&resolver, &ctx).await;
        assert!(result.is_ok());

        let inheritance = result.unwrap();
        assert!(inheritance.ancestors.is_empty());
        assert_eq!(inheritance.chain_ids.len(), 1);
    }

    // ── Tests: cache_ttl_seconds ──────────────────────────────────────────

    #[test]
    fn test_cache_ttl_own_uses_config() {
        let config = ModelRegistryConfig {
            own_ttl_seconds: 1800,
            inherited_ttl_seconds: 300,
            max_page_size: 100,
        };
        assert_eq!(cache_ttl_seconds(Ownership::Own, &config), 1800);
    }

    #[test]
    fn test_cache_ttl_inherited_uses_config() {
        let config = ModelRegistryConfig {
            own_ttl_seconds: 1800,
            inherited_ttl_seconds: 300,
            max_page_size: 100,
        };
        assert_eq!(cache_ttl_seconds(Ownership::Inherited, &config), 300);
    }

    #[test]
    fn test_cache_ttl_custom_config_values() {
        let config = ModelRegistryConfig {
            own_ttl_seconds: 3600,
            inherited_ttl_seconds: 600,
            max_page_size: 50,
        };
        assert_eq!(cache_ttl_seconds(Ownership::Own, &config), 3600);
        assert_eq!(cache_ttl_seconds(Ownership::Inherited, &config), 600);
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
        let result = resolve_ancestors(&FailingResolver, &ctx).await;
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

    fn make_provider(id: Uuid, slug: &str, status: ProviderStatus) -> ProviderV1 {
        let now = Utc::now();
        ProviderV1 {
            id,
            slug: slug.to_owned(),
            name: slug.to_owned(),
            gts_type: GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.generic.v1~"),
            status,
            managed: false,
            metadata: None,
            discovery_enabled: false,
            discovery_interval_seconds: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn single_ancestor() -> Vec<TenantRef> {
        vec![TenantRef {
            id: TenantId(parent_id()),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: None,
            self_managed: false,
        }]
    }

    #[tokio::test]
    async fn test_chain_providers_single_tenant() {
        let ctx = InheritanceContext::new(child_id(), vec![]);
        let providers = vec![
            make_provider(Uuid::nil(), "openai", ProviderStatus::Active),
            make_provider(
                Uuid::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap(),
                "anthropic",
                ProviderStatus::Active,
            ),
        ];

        let chain = build_chain_providers(&ctx, |_scope| {
            let ps = providers.clone();
            async move { Ok(ps) }
        })
        .await
        .expect("build should succeed");

        // Both providers should be winners and in the allow-list.
        assert_eq!(chain.by_id.len(), 2);
        assert_eq!(chain.allow_list().len(), 2);
        assert!(chain.is_allowed(Uuid::nil()));
        assert_eq!(chain.get(Uuid::nil()).map(|p| p.winner), Some(true));
    }

    #[tokio::test]
    async fn test_chain_providers_child_shadows_parent() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());

        let own_id = Uuid::nil();
        let parent_prov_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000b1").unwrap();
        let parent_prov = make_provider(parent_prov_id, "openai", ProviderStatus::Active);

        let chain = build_chain_providers(&ctx, |scope| {
            let is_child = scope_targets(&scope, child_id());
            let pp = parent_prov.clone();
            async move {
                if is_child {
                    Ok(vec![make_provider(
                        own_id,
                        "openai",
                        ProviderStatus::Active,
                    )])
                } else {
                    Ok(vec![pp])
                }
            }
        })
        .await
        .expect("build should succeed");

        // Child wins the "openai" slug.
        let child_prov = chain.get(own_id).expect("child provider should exist");
        assert!(child_prov.winner, "child should win the slug");
        assert_eq!(child_prov.owner_tenant, child_id());
        assert_eq!(chain.allow_list().len(), 1);
        assert!(chain.is_allowed(own_id));
        assert!(!chain.is_allowed(parent_prov_id));
    }

    #[tokio::test]
    async fn test_chain_providers_parent_shadows_grandparent() {
        let parent_id = parent_id();
        let grandparent_id = grandparent_id();
        let ctx = InheritanceContext::new(
            parent_id,
            vec![TenantRef {
                id: TenantId(grandparent_id),
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            }],
        );

        let parent_prov_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000c1").unwrap();
        let gp_prov_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000c2").unwrap();

        let chain = build_chain_providers(&ctx, |scope| {
            let is_parent = scope_targets(&scope, parent_id);
            async move {
                if is_parent {
                    Ok(vec![make_provider(
                        parent_prov_id,
                        "openai",
                        ProviderStatus::Active,
                    )])
                } else {
                    Ok(vec![make_provider(
                        gp_prov_id,
                        "openai",
                        ProviderStatus::Active,
                    )])
                }
            }
        })
        .await
        .expect("build should succeed");

        // Parent wins the "openai" slug.
        let parent_prov = chain.get(parent_prov_id).expect("parent provider");
        assert!(parent_prov.winner);
        let gp_prov = chain.get(gp_prov_id).expect("grandparent provider");
        assert!(!gp_prov.winner, "grandparent should lose to parent");

        assert_eq!(chain.allow_list().len(), 1);
        assert!(chain.is_allowed(parent_prov_id));
        assert!(!chain.is_allowed(gp_prov_id));
    }

    #[tokio::test]
    async fn test_chain_providers_unrelated_slugs_coexist() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());

        let child_only_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000d1").unwrap();
        let parent_only_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000d2").unwrap();

        let chain = build_chain_providers(&ctx, |scope| {
            let is_child = scope_targets(&scope, child_id());
            async move {
                if is_child {
                    Ok(vec![make_provider(
                        child_only_id,
                        "child-only",
                        ProviderStatus::Active,
                    )])
                } else {
                    Ok(vec![make_provider(
                        parent_only_id,
                        "parent-only",
                        ProviderStatus::Active,
                    )])
                }
            }
        })
        .await
        .expect("build should succeed");

        // Both slugs should be winners with no collisions.
        assert_eq!(chain.by_id.len(), 2);
        assert_eq!(chain.allow_list().len(), 2);
    }

    #[tokio::test]
    async fn test_chain_providers_disabled_shadow() {
        // A disabled shadow must still win its slug (so the ancestor's models
        // are excluded), but be absent from the allow-list (so its own models
        // are excluded too). §3.5 rationale: folding status into `winner` hands
        // the slug back to the ancestor and re-exposes the shadowed models.
        let ctx = InheritanceContext::new(child_id(), single_ancestor());

        let child_prov_id = Uuid::nil();
        let parent_prov_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000e1").unwrap();

        let chain = build_chain_providers(&ctx, |scope| {
            let is_child = scope_targets(&scope, child_id());
            async move {
                if is_child {
                    Ok(vec![make_provider(
                        child_prov_id,
                        "openai",
                        ProviderStatus::Disabled,
                    )])
                } else {
                    Ok(vec![make_provider(
                        parent_prov_id,
                        "openai",
                        ProviderStatus::Active,
                    )])
                }
            }
        })
        .await
        .expect("build should succeed");

        // Child (disabled) must still win the slug.
        let child = chain.get(child_prov_id).expect("child provider");
        assert!(child.winner, "disabled shadow must still win the slug");
        assert_eq!(child.owner_tenant, child_id());

        // Parent must be a loser.
        let parent = chain.get(parent_prov_id).expect("parent provider");
        assert!(!parent.winner, "parent must lose to disabled child");

        // Neither should be in the allow-list (child is disabled, parent lost).
        assert_eq!(chain.allow_list().len(), 0, "no active winners");
        assert!(!chain.is_allowed(child_prov_id));
        assert!(!chain.is_allowed(parent_prov_id));
    }

    #[tokio::test]
    async fn test_chain_providers_fails_closed_on_query_error() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());

        let err = build_chain_providers(&ctx, |_scope| async move {
            Err(DomainError::internal("database unavailable"))
        })
        .await
        .expect_err("build should fail on query error");

        assert!(
            err.to_string().contains("database unavailable"),
            "expected internal error with cause, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_chain_providers_allow_slice_empty_when_no_active_winners() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());

        let child_prov_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000f1").unwrap();
        let parent_prov_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000f2").unwrap();

        // Child has a provider but it's disabled; parent has one that's also disabled.
        let chain = build_chain_providers(&ctx, |scope| {
            let is_child = scope_targets(&scope, child_id());
            async move {
                if is_child {
                    Ok(vec![make_provider(
                        child_prov_id,
                        "openai",
                        ProviderStatus::Disabled,
                    )])
                } else {
                    Ok(vec![make_provider(
                        parent_prov_id,
                        "openai",
                        ProviderStatus::Disabled,
                    )])
                }
            }
        })
        .await
        .expect("build should succeed");

        // Child's allow slice should be empty (disabled provider).
        let child_slice = chain.allow_slice_for(child_id());
        assert!(
            child_slice.is_empty(),
            "child has no active winning providers"
        );

        // Parent's allow slice should also be empty (lost to child).
        let parent_slice = chain.allow_slice_for(parent_id());
        assert!(
            parent_slice.is_empty(),
            "parent has no active winning providers (lost)"
        );
    }

    #[tokio::test]
    async fn test_chain_providers_allow_slice_returns_only_active_winners_for_tenant() {
        let ctx = InheritanceContext::new(child_id(), single_ancestor());

        let child_id_v = child_id();
        let parent_id_v = parent_id();

        let child_prov = Uuid::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap();
        let child_prov2 = Uuid::parse_str("00000000-0000-0000-0000-0000000000a2").unwrap();
        let parent_prov = Uuid::parse_str("00000000-0000-0000-0000-0000000000a3").unwrap();

        let chain = build_chain_providers(&ctx, |scope| {
            let is_child = scope_targets(&scope, child_id_v);
            async move {
                if is_child {
                    Ok(vec![
                        make_provider(child_prov, "child-openai", ProviderStatus::Active),
                        make_provider(child_prov2, "child-anthropic", ProviderStatus::Active),
                    ])
                } else {
                    Ok(vec![make_provider(
                        parent_prov,
                        "parent-slug",
                        ProviderStatus::Active,
                    )])
                }
            }
        })
        .await
        .expect("build should succeed");

        // Child's allow slice should have both child providers.
        let child_slice = chain.allow_slice_for(child_id_v);
        assert_eq!(child_slice.len(), 2);
        assert!(child_slice.contains(&child_prov));
        assert!(child_slice.contains(&child_prov2));

        // Parent's allow slice should have the parent provider.
        let parent_slice = chain.allow_slice_for(parent_id_v);
        assert_eq!(parent_slice.len(), 1);
        assert!(parent_slice.contains(&parent_prov));
    }
}
