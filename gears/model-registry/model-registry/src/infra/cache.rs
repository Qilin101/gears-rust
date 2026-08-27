//! `ResolutionCache` bound to the `cluster` gear's `ClusterCacheV1`
//! (DESIGN §3.3 "Cache").
//!
//! Resolution is lazy: the `cluster` gear registers its backends during
//! `start`, this gear builds its `Service` during `init`, and the toolkit runs
//! every `init` before any `start` — so resolving eagerly always fails.
//!
//! The cache is never load-bearing. Every `ClusterError`, encode failure, and
//! decode failure is logged and reported to the caller as a plain miss.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::{ClusterCacheV1, ClusterProfile};
use toolkit::client_hub::ClientHub;
use uuid::Uuid;

use crate::domain::cache::ResolutionCache;

/// The sub-namespace every key of this gear is written under, so the backend
/// observes `model-registry/chain/{uuid}`.
const CACHE_SCOPE: &str = "model-registry";

/// The cluster profile this gear resolves its cache from.
///
/// The name cannot come from config: [`ClusterProfile::NAME`] is a `const` and
/// `profile_scope` is crate-private in `cluster-sdk`.
#[derive(Clone, Copy)]
struct ModelRegistryProfile;

impl ClusterProfile for ModelRegistryProfile {
    const NAME: &'static str = "default";
}

/// [`ResolutionCache`] over `ClusterCacheV1`, resolved on first use.
pub struct ClusterResolutionCache {
    hub: Arc<ClientHub>,
    /// The resolved, `model-registry`-scoped facade. Empty until the first
    /// successful resolution; a failed resolution is **not** memoized, so a
    /// backend bound after the first cache use is still picked up.
    facade: OnceLock<ClusterCacheV1>,
    /// Guards the unavailable-backend warning so it is emitted once, not once
    /// per request.
    warned: AtomicBool,
    ttl: Duration,
}

impl ClusterResolutionCache {
    /// Construct the cache. Performs no resolution — see the module docs.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, ttl_seconds: u64) -> Self {
        Self {
            hub,
            facade: OnceLock::new(),
            warned: AtomicBool::new(false),
            ttl: Duration::from_secs(ttl_seconds),
        }
    }

    /// The resolved facade, resolving it on first use.
    ///
    /// Returns `None` when no backend is bound for the profile or the scope
    /// prefix is rejected, having logged the reason once.
    fn facade(&self) -> Option<&ClusterCacheV1> {
        if let Some(facade) = self.facade.get() {
            return Some(facade);
        }

        let resolved = ClusterCacheV1::resolver(&self.hub)
            // No `CacheCapability` is required: any registered backend serves.
            .profile(ModelRegistryProfile)
            .resolve()
            .and_then(|facade| facade.scoped(CACHE_SCOPE));

        match resolved {
            // A concurrent resolution may have won the race; either value is
            // equivalent, so a rejected `set` is not an error.
            Ok(facade) => Some(self.facade.get_or_init(|| facade)),
            Err(error) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        profile = ModelRegistryProfile::NAME,
                        %error,
                        "cluster cache unavailable; resolving every tenant chain through tenant-resolver"
                    );
                }
                None
            }
        }
    }
}

/// The cache key for a tenant's ancestor chain.
fn chain_key(tenant_id: Uuid) -> String {
    format!("chain/{tenant_id}")
}

#[async_trait]
impl ResolutionCache for ClusterResolutionCache {
    async fn get_chain(&self, tenant_id: Uuid) -> Option<Vec<Uuid>> {
        let facade = self.facade()?;
        let key = chain_key(tenant_id);

        match facade.get(&key).await {
            Ok(Some(entry)) => match serde_json::from_slice::<Vec<Uuid>>(&entry.value) {
                Ok(chain) => Some(chain),
                Err(error) => {
                    tracing::warn!(%tenant_id, %error, "chain cache entry did not decode; treating as a miss");
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%tenant_id, %error, "chain cache read failed; treating as a miss");
                None
            }
        }
    }

    async fn put_chain(&self, tenant_id: Uuid, chain: &[Uuid]) {
        let Some(facade) = self.facade() else {
            return;
        };
        let Ok(value) = serde_json::to_vec(chain) else {
            tracing::warn!(%tenant_id, "chain cache entry did not encode; skipping the write");
            return;
        };

        if let Err(error) = facade
            .put(PutRequest {
                key: &chain_key(tenant_id),
                value: &value,
                ttl: Ttl::Of(self.ttl),
            })
            .await
        {
            tracing::warn!(%tenant_id, %error, "chain cache write failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chain_key_shape() {
        let id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        assert_eq!(chain_key(id), "chain/11111111-1111-1111-1111-111111111111");
    }

    /// An unbound profile degrades to a miss rather than an error, and the
    /// write is a no-op.
    #[tokio::test]
    async fn test_unbound_profile_is_a_miss() {
        let cache = ClusterResolutionCache::new(Arc::new(ClientHub::new()), 30);
        let tenant_id = Uuid::new_v4();

        assert!(cache.get_chain(tenant_id).await.is_none());
        cache.put_chain(tenant_id, &[Uuid::new_v4()]).await;
        assert!(cache.get_chain(tenant_id).await.is_none());
    }
}
