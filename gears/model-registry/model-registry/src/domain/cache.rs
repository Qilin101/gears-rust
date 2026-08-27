//! Resolution cache contract (DESIGN §3.2 "`ResolutionCache`").
//!
//! One entity is cached: a tenant's ordered ancestor chain. The trait is typed
//! in domain terms rather than generic over `serde`, keeping key construction
//! in one place and byte encoding out of the domain layer.

use async_trait::async_trait;
use uuid::Uuid;

/// Cache of resolved tenant ancestor chains.
///
/// MUST stay dyn-compatible: [`Service`](crate::domain::service::Service) holds
/// it as `Arc<dyn ResolutionCache>`, so no method may be generic over its value
/// type.
///
/// There is no invalidation method — the entry expires by TTL only. A
/// `delete_chain` arrives with the P3 `tenant.reparented` handler.
#[async_trait]
pub trait ResolutionCache: Send + Sync {
    /// Return the cached ancestor chain of `tenant_id`, closest ancestor first.
    ///
    /// `None` is a miss. An empty `Vec` is a **hit** — a root tenant has no
    /// ancestors — and MUST NOT be conflated with a miss.
    async fn get_chain(&self, tenant_id: Uuid) -> Option<Vec<Uuid>>;

    /// Store the ancestor chain of `tenant_id`.
    ///
    /// Best-effort: a backend failure is logged and swallowed, never surfaced.
    async fn put_chain(&self, tenant_id: Uuid, chain: &[Uuid]);
}

/// A cache that always misses and never stores.
///
/// Installed when the cluster cache profile is unbound or `cache_enabled` is
/// `false`, and used by the unit suite for the uncached path.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopResolutionCache;

#[async_trait]
impl ResolutionCache for NoopResolutionCache {
    async fn get_chain(&self, _tenant_id: Uuid) -> Option<Vec<Uuid>> {
        None
    }

    async fn put_chain(&self, _tenant_id: Uuid, _chain: &[Uuid]) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The noop cache misses even for a tenant it was just asked to store, so a
    /// caller never observes a chain it did not resolve itself.
    #[tokio::test]
    async fn test_noop_cache_always_misses() {
        let tenant_id = Uuid::new_v4();
        let cache = NoopResolutionCache;

        cache.put_chain(tenant_id, &[Uuid::new_v4()]).await;
        assert!(cache.get_chain(tenant_id).await.is_none());

        cache.put_chain(tenant_id, &[]).await;
        assert!(
            cache.get_chain(tenant_id).await.is_none(),
            "an empty stored chain must not become a hit either"
        );
    }
}
