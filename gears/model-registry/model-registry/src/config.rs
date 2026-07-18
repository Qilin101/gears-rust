//! Configuration for the Model Registry gear.

use serde::Deserialize;

/// Configuration for the Model Registry gear.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelRegistryConfig {
    /// TTL in seconds for tenant-owned cache entries (default: 1800 = 30 min).
    #[serde(default = "default_own_ttl")]
    pub own_ttl_seconds: u64,

    /// TTL in seconds for inherited cache entries (default: 300 = 5 min).
    #[serde(default = "default_inherited_ttl")]
    pub inherited_ttl_seconds: u64,

    /// Maximum page size for `OData` list endpoints (default: 100).
    #[serde(default = "default_max_page_size")]
    pub max_page_size: u32,
}

impl Default for ModelRegistryConfig {
    fn default() -> Self {
        Self {
            own_ttl_seconds: default_own_ttl(),
            inherited_ttl_seconds: default_inherited_ttl(),
            max_page_size: default_max_page_size(),
        }
    }
}

const fn default_own_ttl() -> u64 {
    1800
}

const fn default_inherited_ttl() -> u64 {
    300
}

const fn default_max_page_size() -> u32 {
    100
}
