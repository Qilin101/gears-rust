//! The [`ClusterCacheProvider`] implementation for the standalone backend.
//!
//! This is the production glue the wiring crate dispatches to when an operator
//! binds a cache to `provider: standalone`. It implements the SDK trait — so this
//! crate depends on `cluster-sdk` only, never on the wiring crate — and builds
//! the native in-process cache, owning its TTL sweeper via the returned
//! [`StopHook`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::{ClusterCacheBackend, ClusterCacheProvider, ClusterError, StopHook};
use serde::Deserialize;

use crate::plugin::StandaloneClusterPlugin;

/// The operator config `provider` name that selects the standalone backend.
pub const PROVIDER_NAME: &str = "standalone";

/// The standalone provider's recognized options (Design A — flattened into the
/// backend binding). `deny_unknown_fields` turns an operator typo (e.g.
/// `sweep_interval_m` instead of `sweep_interval_ms`) into a startup
/// `ClusterError::InvalidConfig` instead of a silently-ignored key.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StandaloneOptions {
    /// The cache TTL-sweep cadence in milliseconds. Omitted → the plugin
    /// default. Zero is rejected by the builder.
    #[serde(default)]
    sweep_interval_ms: Option<u64>,
}

/// Builds the standalone in-process cache backend from operator config.
///
/// Recognized options (Design A — flattened into the backend binding):
/// - `sweep_interval_ms` (integer): the cache TTL-sweep cadence in milliseconds.
///   Omitted → the plugin default. Zero is rejected by the builder.
pub struct StandaloneCacheProvider;

#[async_trait]
impl ClusterCacheProvider for StandaloneCacheProvider {
    fn provider(&self) -> &'static str {
        PROVIDER_NAME
    }

    async fn build_cache(
        &self,
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError> {
        // Deserialized as a whole (rather than read key-by-key) so an unknown
        // key is a startup error, not a silently-dropped typo.
        let opts: StandaloneOptions =
            serde_json::from_value(serde_json::Value::Object(options.clone())).map_err(|err| {
                ClusterError::InvalidConfig {
                    reason: format!("standalone: invalid options: {err}"),
                }
            })?;

        let mut builder = StandaloneClusterPlugin::builder();
        if let Some(ms) = opts.sweep_interval_ms {
            builder = builder.sweep_interval(Duration::from_millis(ms));
        }

        let handle = builder.build_and_start()?;
        let cache = handle.cache();
        let stop: StopHook = Box::new(move || Box::pin(async move { handle.stop().await }));
        Ok((cache, stop))
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
