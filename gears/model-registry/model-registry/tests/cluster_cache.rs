//! `ClusterResolutionCache` against a `ClusterCacheBackend`.
//!
//! Covers what a `ResolutionCache` stub cannot: that resolution is lazy — a
//! backend bound *after* the cache is constructed is still picked up, which is
//! the ordering the platform produces (`cluster` binds its backends in `start`,
//! model-registry builds its `Service` in `init`) — and that the value survives
//! a real encode/store/load/decode round-trip under the documented key and TTL.
//!
//! The backend here is a local fake, not a cluster plugin: this gear binds no
//! backend at compile time, so its tests must not name one either.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::registration::register_cache_backend;
use cluster_sdk::{
    CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, ClusterCacheBackend, ClusterError,
};
use model_registry::domain::cache::ResolutionCache;
use model_registry::infra::cache::ClusterResolutionCache;
use toolkit::client_hub::ClientHub;
use uuid::Uuid;

/// The profile `ClusterResolutionCache` resolves against.
const PROFILE: &str = "default";

/// A minimal in-process [`ClusterCacheBackend`].
///
/// Implements only the surface `ClusterResolutionCache` exercises — `get`,
/// `put`, and `scan_prefix` for the key-shape assertion. Every other method is
/// unreachable from the gear and left `unimplemented!`, so a future call into
/// one fails loudly rather than silently succeeding against a stub.
/// One stored entry: the bytes the gear wrote and the TTL it asked for.
type StoredEntry = (Vec<u8>, Ttl);

#[derive(Default)]
struct FakeBackend {
    entries: Mutex<HashMap<String, StoredEntry>>,
}

impl FakeBackend {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, StoredEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The TTL the gear passed on the write to `key`.
    fn ttl_of(&self, key: &str) -> Option<Ttl> {
        self.lock().get(key).map(|(_, ttl)| *ttl)
    }
}

#[async_trait]
impl ClusterCacheBackend for FakeBackend {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(false)
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        Ok(self.lock().get(key).map(|(value, _)| CacheEntry {
            value: value.clone(),
            version: 1,
        }))
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        self.lock()
            .insert(req.key.to_owned(), (req.value.to_vec(), req.ttl));
        Ok(())
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        let mut keys: Vec<String> = self
            .lock()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }

    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        unimplemented!("model-registry never deletes a cache entry")
    }

    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        unimplemented!("not reachable from ClusterResolutionCache")
    }

    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        unimplemented!("not reachable from ClusterResolutionCache")
    }

    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        unimplemented!("not reachable from ClusterResolutionCache")
    }

    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        unimplemented!("not reachable from ClusterResolutionCache")
    }

    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        unimplemented!("not reachable from ClusterResolutionCache")
    }
}

fn bind(hub: &Arc<ClientHub>) -> Arc<FakeBackend> {
    let backend = Arc::new(FakeBackend::default());
    register_cache_backend(hub, PROFILE, Arc::clone(&backend) as Arc<_>).expect("bind backend");
    backend
}

#[tokio::test]
async fn backend_bound_after_construction_is_still_resolved() {
    let hub = Arc::new(ClientHub::new());
    let cache = ClusterResolutionCache::new(Arc::clone(&hub), 30);
    let tenant_id = Uuid::new_v4();
    let chain = vec![Uuid::new_v4(), Uuid::new_v4()];

    // Nothing is bound yet: the read degrades to a miss and the write is a
    // no-op, exactly as during the `init` phase.
    assert!(cache.get_chain(tenant_id).await.is_none());
    cache.put_chain(tenant_id, &chain).await;
    assert!(cache.get_chain(tenant_id).await.is_none());

    // The `cluster` gear's `start` binds the backend.
    let _backend = bind(&hub);

    // The same cache instance now resolves and round-trips the chain.
    cache.put_chain(tenant_id, &chain).await;
    assert_eq!(
        cache.get_chain(tenant_id).await,
        Some(chain),
        "the chain must survive the encode/store/load/decode round-trip"
    );
}

/// A root tenant stores an empty ancestor list, which must read back as a hit
/// carrying an empty `Vec` — never as `None`.
#[tokio::test]
async fn empty_chain_round_trips_as_a_hit() {
    let hub = Arc::new(ClientHub::new());
    let _backend = bind(&hub);
    let cache = ClusterResolutionCache::new(hub, 30);
    let tenant_id = Uuid::new_v4();

    assert!(
        cache.get_chain(tenant_id).await.is_none(),
        "an absent entry is a miss"
    );
    cache.put_chain(tenant_id, &[]).await;
    assert_eq!(
        cache.get_chain(tenant_id).await,
        Some(vec![]),
        "an empty ancestor list is a hit, not a miss"
    );
}

/// The backend observes the scoped key documented in DESIGN §3.3
/// (`model-registry/chain/{tenant_id}`), carrying the configured TTL.
#[tokio::test]
async fn writes_land_under_the_scoped_chain_key_with_the_configured_ttl() {
    let hub = Arc::new(ClientHub::new());
    let backend = bind(&hub);
    let cache = ClusterResolutionCache::new(hub, 30);
    let tenant_id = Uuid::new_v4();

    cache.put_chain(tenant_id, &[Uuid::new_v4()]).await;

    let key = format!("model-registry/chain/{tenant_id}");
    assert_eq!(
        backend.scan_prefix("model-registry/").await.unwrap(),
        vec![key.clone()]
    );
    assert_eq!(
        backend.ttl_of(&key),
        Some(Ttl::Of(std::time::Duration::from_secs(30))),
        "chain_cache_ttl_seconds must reach the backend as Ttl::Of"
    );
}

/// A cache entry that does not decode is a miss, not an error.
#[tokio::test]
async fn undecodable_entry_is_a_miss() {
    let hub = Arc::new(ClientHub::new());
    let backend = bind(&hub);
    let cache = ClusterResolutionCache::new(hub, 30);
    let tenant_id = Uuid::new_v4();

    backend
        .put(PutRequest {
            key: &format!("model-registry/chain/{tenant_id}"),
            value: b"not json",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("seed a corrupt entry");

    assert!(cache.get_chain(tenant_id).await.is_none());
}
