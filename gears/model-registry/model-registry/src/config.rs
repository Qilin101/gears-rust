//! Configuration for the Model Registry gear.

use serde::{Deserialize, Serialize};
use toolkit_db::odata::sea_orm_filter::LimitCfg;

/// Configuration for the Model Registry gear.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRegistryConfig {
    /// TTL in seconds for tenant-owned cache entries (default: 1800 = 30 min).
    #[serde(default = "default_own_ttl")]
    pub own_ttl_seconds: u64,

    /// TTL in seconds for inherited cache entries (default: 300 = 5 min).
    #[serde(default = "default_inherited_ttl")]
    pub inherited_ttl_seconds: u64,

    /// Page size for `OData` list endpoints when the request omits `$top`
    /// (default: 20).
    #[serde(default = "default_page_size")]
    pub default_page_size: u32,

    /// Maximum page size for `OData` list endpoints (default: 100).
    #[serde(default = "default_max_page_size")]
    pub max_page_size: u32,
}

impl Default for ModelRegistryConfig {
    fn default() -> Self {
        Self {
            own_ttl_seconds: default_own_ttl(),
            inherited_ttl_seconds: default_inherited_ttl(),
            default_page_size: default_page_size(),
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

const fn default_page_size() -> u32 {
    20
}

const fn default_max_page_size() -> u32 {
    100
}

/// Pagination bounds for the gear's `OData` list endpoints, resolved from
/// [`ModelRegistryConfig`] by [`ModelRegistryConfig::page_limits`].
///
/// Both bounds are guaranteed `>= 1`, and `default <= max`, so the two
/// accessors below always agree with each other.
#[derive(Debug, Clone, Copy)]
pub struct PageLimits {
    default: u64,
    max: u64,
}

impl PageLimits {
    /// The bounds in the shape `toolkit-db`'s pagination helpers take.
    #[must_use]
    pub fn limit_cfg(self) -> LimitCfg {
        LimitCfg {
            default: self.default,
            max: self.max,
        }
    }

    /// The effective page limit for a requested `$top`, mirroring the clamp the
    /// `toolkit-db` pagination helpers apply internally: fall back to the
    /// default when `$top` is absent, floor at 1, cap at the maximum.
    ///
    /// Needed wherever a page is synthesized without running a query and has to
    /// report the same limit a real query would have.
    #[must_use]
    pub fn clamp(self, requested: Option<u64>) -> u64 {
        requested.unwrap_or(self.default).clamp(1, self.max)
    }
}

impl ModelRegistryConfig {
    /// Pagination bounds handed to every `OData` list query.
    ///
    /// Both keys are floored at 1 and the default is capped at the maximum: a
    /// configured `max_page_size: 0` would otherwise clamp every limit to zero
    /// and serve permanently empty pages.
    #[must_use]
    pub fn page_limits(&self) -> PageLimits {
        let max = u64::from(self.max_page_size).max(1);
        PageLimits {
            default: u64::from(self.default_page_size).clamp(1, max),
            max,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ModelRegistryConfig::default();
        assert_eq!(config.own_ttl_seconds, 1800);
        assert_eq!(config.inherited_ttl_seconds, 300);
        assert_eq!(config.default_page_size, 20);
        assert_eq!(config.max_page_size, 100);
    }

    #[test]
    fn test_deserialize_empty_json() {
        let json = "{}";
        let config: ModelRegistryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.own_ttl_seconds, 1800);
        assert_eq!(config.inherited_ttl_seconds, 300);
        assert_eq!(config.default_page_size, 20);
        assert_eq!(config.max_page_size, 100);
    }

    #[test]
    fn test_deserialize_partial_json() {
        let json = r#"{"own_ttl_seconds": 3600}"#;
        let config: ModelRegistryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.own_ttl_seconds, 3600);
        assert_eq!(config.inherited_ttl_seconds, 300);
        assert_eq!(config.default_page_size, 20);
        assert_eq!(config.max_page_size, 100);
    }

    #[test]
    fn test_deserialize_full_json() {
        let json = r#"{
            "own_ttl_seconds": 600,
            "inherited_ttl_seconds": 120,
            "default_page_size": 10,
            "max_page_size": 50
        }"#;
        let config: ModelRegistryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.own_ttl_seconds, 600);
        assert_eq!(config.inherited_ttl_seconds, 120);
        assert_eq!(config.default_page_size, 10);
        assert_eq!(config.max_page_size, 50);
    }

    #[test]
    fn test_serde_round_trip() {
        let config = ModelRegistryConfig {
            own_ttl_seconds: 7200,
            inherited_ttl_seconds: 600,
            default_page_size: 25,
            max_page_size: 200,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ModelRegistryConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.own_ttl_seconds, 7200);
        assert_eq!(deserialized.inherited_ttl_seconds, 600);
        assert_eq!(deserialized.default_page_size, 25);
        assert_eq!(deserialized.max_page_size, 200);
    }

    #[test]
    fn test_page_limits_uses_configured_keys() {
        let cfg = ModelRegistryConfig {
            default_page_size: 3,
            max_page_size: 7,
            ..Default::default()
        };
        let limits = cfg.page_limits();
        assert_eq!(limits.limit_cfg().default, 3);
        assert_eq!(limits.limit_cfg().max, 7);
        assert_eq!(limits.clamp(None), 3, "no $top falls back to the default");
        assert_eq!(limits.clamp(Some(5)), 5, "$top within bounds is honoured");
        assert_eq!(
            limits.clamp(Some(99)),
            7,
            "$top above the maximum is capped"
        );
        assert_eq!(limits.clamp(Some(0)), 1, "$top=0 is floored at one row");
    }

    #[test]
    fn test_page_limits_floors_zeroes_at_one() {
        let cfg = ModelRegistryConfig {
            default_page_size: 0,
            max_page_size: 0,
            ..Default::default()
        };
        let limits = cfg.page_limits();
        assert_eq!(limits.limit_cfg().default, 1);
        assert_eq!(limits.limit_cfg().max, 1);
        assert_eq!(limits.clamp(None), 1);
    }

    #[test]
    fn test_page_limits_caps_default_at_max() {
        let cfg = ModelRegistryConfig {
            default_page_size: 50,
            max_page_size: 10,
            ..Default::default()
        };
        let limits = cfg.page_limits();
        assert_eq!(limits.limit_cfg().default, 10);
        assert_eq!(limits.clamp(None), 10);
    }
}
