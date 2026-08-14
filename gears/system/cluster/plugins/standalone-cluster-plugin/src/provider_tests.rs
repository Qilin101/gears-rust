//! Tests for the standalone [`ClusterCacheProvider`] impl: the flattened-options
//! contract and the built cache's lifecycle.

use std::time::Duration;

use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::{CacheEvent, CacheWatchEvent, ClusterCacheProvider, ClusterError};

use super::StandaloneCacheProvider;

#[test]
fn provider_name_matches_the_operator_config_key() {
    // The name is the string an operator writes as `provider = "..."`; a rename
    // is a breaking config change, so pin the externally-visible value.
    assert_eq!(StandaloneCacheProvider.provider(), "standalone");
}

#[tokio::test]
async fn builds_cache_with_default_options() {
    let options = serde_json::Map::new();
    let (cache, stop) = StandaloneCacheProvider
        .build_cache(&options)
        .await
        .expect("default options build a cache");

    assert!(
        cache
            .put(PutRequest {
                key: "k",
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    let Ok(Some(entry)) = cache.get("k").await else {
        panic!("value must round-trip");
    };
    assert_eq!(entry.value, b"v");

    stop().await;
}

#[tokio::test(start_paused = true)]
async fn applies_the_flattened_sweep_interval() {
    let mut options = serde_json::Map::new();
    options.insert("sweep_interval_ms".to_owned(), serde_json::json!(50));
    let (cache, stop) = StandaloneCacheProvider
        .build_cache(&options)
        .await
        .expect("a valid sweep interval builds a cache");

    // Prove the flattened option is actually applied to the running sweeper:
    // a TTL'd entry must be actively reaped (emitting `Expired`) on the
    // configured cadence, not merely lazily on the next read.
    let Ok(mut watch) = cache.watch("k").await else {
        panic!("watch must establish");
    };
    assert!(
        cache
            .put(PutRequest {
                key: "k",
                value: b"v",
                ttl: Ttl::Of(Duration::from_millis(10)),
            })
            .await
            .is_ok()
    );
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
    ));

    // Past the 10ms entry TTL and one 50ms sweep tick.
    tokio::time::advance(Duration::from_millis(60)).await;
    tokio::task::yield_now().await;
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Expired { .. }))
    ));

    stop().await;
}

#[tokio::test]
async fn malformed_sweep_interval_is_rejected() {
    let mut options = serde_json::Map::new();
    options.insert("sweep_interval_ms".to_owned(), serde_json::json!("soon"));
    let result = StandaloneCacheProvider.build_cache(&options).await;
    assert!(
        matches!(result, Err(ClusterError::InvalidConfig { .. })),
        "a non-integer sweep interval must be rejected, not silently ignored"
    );
}

#[tokio::test]
async fn zero_sweep_interval_is_rejected() {
    let mut options = serde_json::Map::new();
    options.insert("sweep_interval_ms".to_owned(), serde_json::json!(0));
    let result = StandaloneCacheProvider.build_cache(&options).await;
    assert!(matches!(result, Err(ClusterError::InvalidConfig { .. })));
}

#[tokio::test]
async fn unknown_option_key_is_rejected() {
    // A typo'd key (missing the trailing `s`) must fail startup rather than
    // being silently dropped and the intended setting never taking effect.
    let mut options = serde_json::Map::new();
    options.insert("sweep_interval_m".to_owned(), serde_json::json!(50));
    let result = StandaloneCacheProvider.build_cache(&options).await;
    assert!(
        matches!(result, Err(ClusterError::InvalidConfig { .. })),
        "an unrecognized option key must be rejected, not silently ignored"
    );
}
