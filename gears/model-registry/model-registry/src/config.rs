//! Configuration for the Model Registry gear.

use serde::{Deserialize, Serialize};

/// Configuration for the Model Registry gear.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ModelRegistryConfig::default();
        assert_eq!(config.own_ttl_seconds, 1800);
        assert_eq!(config.inherited_ttl_seconds, 300);
        assert_eq!(config.max_page_size, 100);
    }

    #[test]
    fn test_deserialize_empty_json() {
        let json = "{}";
        let config: ModelRegistryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.own_ttl_seconds, 1800);
        assert_eq!(config.inherited_ttl_seconds, 300);
        assert_eq!(config.max_page_size, 100);
    }

    #[test]
    fn test_deserialize_partial_json() {
        let json = r#"{"own_ttl_seconds": 3600}"#;
        let config: ModelRegistryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.own_ttl_seconds, 3600);
        assert_eq!(config.inherited_ttl_seconds, 300);
        assert_eq!(config.max_page_size, 100);
    }

    #[test]
    fn test_deserialize_full_json() {
        let json = r#"{
            "own_ttl_seconds": 600,
            "inherited_ttl_seconds": 120,
            "max_page_size": 50
        }"#;
        let config: ModelRegistryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.own_ttl_seconds, 600);
        assert_eq!(config.inherited_ttl_seconds, 120);
        assert_eq!(config.max_page_size, 50);
    }

    #[test]
    fn test_serde_round_trip() {
        let config = ModelRegistryConfig {
            own_ttl_seconds: 7200,
            inherited_ttl_seconds: 600,
            max_page_size: 200,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ModelRegistryConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.own_ttl_seconds, 7200);
        assert_eq!(deserialized.inherited_ttl_seconds, 600);
        assert_eq!(deserialized.max_page_size, 200);
    }
}
