//! Cache service trait and in-memory backend.
//!
//! Defines the [`CacheService`] trait for cache-first model/provider reads
//! (DESIGN §2.1). Ships the [`InMemoryCache`] backend.

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use toolkit_macros::domain_model;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Cache key helpers
// ---------------------------------------------------------------------------

/// Build a cache key following the `mr:{tenant_id}:{entity}:{id}` format.
#[must_use]
pub fn cache_key(tenant_id: &Uuid, entity: &str, id: &str) -> String {
    format!("mr:{tenant_id}:{entity}:{id}")
}

/// Return the key prefix for all entries belonging to `tenant_id`.
#[must_use]
pub fn tenant_prefix(tenant_id: &Uuid) -> String {
    format!("mr:{tenant_id}:")
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Cache service for model-registry gear.
///
/// All backends must be `Send + Sync` so they can be shared via `Arc`.
#[async_trait]
pub trait CacheService: Send + Sync {
    /// Retrieve a deserialized value from cache.
    ///
    /// Returns `None` when the key is absent or the cached entry has expired.
    async fn get<T: DeserializeOwned + Send>(&self, key: &str) -> Option<T>;

    /// Store a serializable value with the given TTL (in seconds).
    ///
    /// A `ttl_seconds` of 0 means the entry expires immediately (effectively
    /// a no-op store — the next `get` will always miss).
    async fn set<T: Serialize + Send + Sync>(&self, key: &str, value: &T, ttl_seconds: u64);

    /// Remove a single entry from cache.
    async fn delete(&self, key: &str);

    /// Remove **all** entries whose key starts with `mr:{tenant_id}:`.
    async fn invalidate_tenant(&self, tenant_id: Uuid);
}

// ---------------------------------------------------------------------------
// InMemoryCache
// ---------------------------------------------------------------------------

#[domain_model]
struct CacheEntry {
    data: Vec<u8>,
    expires_at: Instant,
}

/// TTL-aware, tenant-prefixed in-memory cache.
///
/// Uses a `HashMap<String, CacheEntry>` behind `Arc<RwLock<...>>`. Values are
/// stored as serialized JSON. Expired entries are lazily evicted (checked on
/// `get` and periodically considered stale).
///
/// # Panics
///
/// `set` panics if serialization fails (should never happen with the types
/// we store — plain SDK structs with serde derives).
#[derive(Clone, Default)]
#[domain_model]
pub struct InMemoryCache {
    data: Arc<RwLock<HashMap<String, CacheEntry>>>,
}

impl InMemoryCache {
    /// Create a new empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl CacheService for InMemoryCache {
    async fn get<T: DeserializeOwned + Send>(&self, key: &str) -> Option<T> {
        let map = self.data.read().await;
        let entry = map.get(key)?;
        if Instant::now() >= entry.expires_at {
            // Entry expired — drop the read lock, acquire write lock, remove.
            drop(map);
            self.delete(key).await;
            return None;
        }
        serde_json::from_slice(&entry.data).ok()
    }

    async fn set<T: Serialize + Send + Sync>(&self, key: &str, value: &T, ttl_seconds: u64) {
        let Ok(data) = serde_json::to_vec(value) else {
            tracing::warn!("InMemoryCache::set: serialization failed, skipping cache write");
            return;
        };
        let expires_at = if ttl_seconds == 0 {
            Instant::now()
        } else {
            Instant::now() + Duration::from_secs(ttl_seconds)
        };
        let mut map = self.data.write().await;
        map.insert(key.to_owned(), CacheEntry { data, expires_at });
    }

    async fn delete(&self, key: &str) {
        let mut map = self.data.write().await;
        map.remove(key);
    }

    async fn invalidate_tenant(&self, tenant_id: Uuid) {
        let prefix = tenant_prefix(&tenant_id);
        let mut map = self.data.write().await;
        map.retain(|k, _| !k.starts_with(&prefix));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::time::Duration;

    /// Simple value type for cache tests.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[domain_model]
    struct TestValue {
        name: String,
        count: u32,
    }

    fn test_value() -> TestValue {
        TestValue {
            name: "hello".into(),
            count: 42,
        }
    }

    #[tokio::test]
    async fn test_set_get_hit() {
        let cache = InMemoryCache::new();
        let key = "mr:t1:model:m1";
        cache.set(key, &test_value(), 60).await;

        let got: TestValue = cache.get(key).await.expect("value should be present");
        assert_eq!(got, test_value());
    }

    #[tokio::test]
    async fn test_ttl_expiry() {
        let cache = InMemoryCache::new();
        let key = "mr:t1:model:m1";
        // TTL=0 means immediate expiry
        cache.set(key, &test_value(), 0).await;

        // Should have expired immediately
        let got: Option<TestValue> = cache.get(key).await;
        assert!(got.is_none(), "entry with TTL=0 should be expired");
    }

    #[tokio::test]
    async fn test_ttl_short_expiry() {
        let cache = InMemoryCache::new();
        let key = "mr:t1:model:m1";
        // TTL=1 second
        cache.set(key, &test_value(), 1).await;

        // Should be present immediately
        let got: Option<TestValue> = cache.get(key).await;
        assert!(got.is_some());

        // Wait for expiry
        tokio::time::sleep(Duration::from_millis(1100)).await;

        let got: Option<TestValue> = cache.get(key).await;
        assert!(got.is_none(), "entry should have expired after 1 second");
    }

    #[tokio::test]
    async fn test_delete() {
        let cache = InMemoryCache::new();
        let key = "mr:t1:model:m1";
        cache.set(key, &test_value(), 60).await;
        assert!(cache.get::<TestValue>(key).await.is_some());

        cache.delete(key).await;
        let got: Option<TestValue> = cache.get(key).await;
        assert!(got.is_none(), "entry should be deleted");
    }

    #[tokio::test]
    async fn test_invalidate_tenant_isolated() {
        let cache = InMemoryCache::new();
        let t1 = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let t2 = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();

        let k1 = cache_key(&t1, "model", "m1");
        let k2 = cache_key(&t1, "provider", "p1");
        let k3 = cache_key(&t2, "model", "m2");

        cache.set(&k1, &test_value(), 60).await;
        cache.set(&k2, &test_value(), 60).await;
        cache.set(&k3, &test_value(), 60).await;

        // Invalidate tenant t1
        cache.invalidate_tenant(t1).await;

        // t1 keys should be gone
        assert!(cache.get::<TestValue>(&k1).await.is_none());
        assert!(cache.get::<TestValue>(&k2).await.is_none());

        // t2 keys should still be present
        assert!(cache.get::<TestValue>(&k3).await.is_some());
    }

    #[tokio::test]
    async fn test_invalidate_tenant_empty() {
        let cache = InMemoryCache::new();
        let t1 = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        // Should not panic on an empty cache
        cache.invalidate_tenant(t1).await;
    }

    #[tokio::test]
    async fn test_get_wrong_type() {
        let cache = InMemoryCache::new();
        let key = "mr:t1:model:m1";
        cache.set(key, &42_u32, 60).await;

        // Trying to deserialize a u32 as TestValue should fail → None
        let got: Option<TestValue> = cache.get(key).await;
        assert!(got.is_none(), "type mismatch should return None");
    }
}
