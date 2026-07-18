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

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, TenantId, TenantRef, TenantResolverClient,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::error::DomainError;
use crate::config::ModelRegistryConfig;

// ---------------------------------------------------------------------------
// Ownership
// ---------------------------------------------------------------------------

/// Classification of a resource's ownership relative to the requesting tenant.
///
/// Used to select the cache TTL for a cached entry (own entries have a longer
/// TTL because they change less frequently from the requestor's perspective).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
// InheritanceContext
// ---------------------------------------------------------------------------

/// Resolved ancestor chain for a tenant, providing helper methods to
/// classify ownership and compute additive visibility with child-shadowing.
#[derive(Debug, Clone)]
pub struct InheritanceContext {
    /// Ancestor tenant chain from direct parent to root.
    pub ancestors: Vec<TenantRef>,
    /// Set of ancestor tenant IDs for O(1) membership checks.
    ancestor_ids: HashSet<Uuid>,
    /// The requesting tenant's ID.
    tenant_id: Uuid,
    /// Full chain from closest (self) to root: `[self, parent, grandparent, ...]`.
    chain_ids: Vec<Uuid>,
}

impl InheritanceContext {
    /// Build a new context from the requesting tenant's ID and its ancestor chain.
    ///
    /// `ancestors` should be ordered from direct parent to root (as returned by
    /// [`TenantResolverClient::get_ancestors`]).
    #[must_use]
    pub fn new(tenant_id: Uuid, ancestors: Vec<TenantRef>) -> Self {
        let ancestor_ids: HashSet<Uuid> = ancestors.iter().map(|a| a.id.0).collect();
        let mut chain_ids = Vec::with_capacity(1 + ancestors.len());
        chain_ids.push(tenant_id);
        chain_ids.extend(ancestors.iter().map(|a| a.id.0));

        Self {
            ancestors,
            ancestor_ids,
            tenant_id,
            chain_ids,
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
    #[must_use]
    pub fn is_ancestor(&self, candidate_id: Uuid) -> bool {
        self.ancestor_ids.contains(&candidate_id)
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
        // Build a position map: tenant_id → index in chain (closer = lower).
        let pos_in_chain: HashMap<Uuid, usize> = self
            .chain_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();

        // Index items by chain position, filter out tenants not in chain.
        let mut indexed: Vec<(usize, Uuid, T)> = items
            .into_iter()
            .filter_map(|(tid, item)| pos_in_chain.get(&tid).map(|&pos| (pos, tid, item)))
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
    use tenant_resolver_sdk::{
        GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse, GetTenantsOptions,
        IsAncestorOptions, TenantInfo, TenantResolverError, TenantStatus,
    };

    // ── Mock TenantResolverClient ─────────────────────────────────────────

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
}
