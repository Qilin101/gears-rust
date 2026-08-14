//! Serde codecs local to the Usage Collector SDK.
//!
//! [`bigdecimal_str_option`] carries an `Option<BigDecimal>` as a JSON string
//! (or `null`), mirroring `rust_decimal::serde::str_option`'s strict
//! string-only contract. It exists because `bigdecimal`'s own serde impls are
//! not compiled in this workspace, its default `Deserialize` also accepts bare
//! JSON numbers (looser than our wire contract), and its `Display` can switch
//! to scientific notation past a threshold — so we serialize via
//! [`bigdecimal::BigDecimal::to_plain_string`] and deserialize strings only.

/// `serde` `with`-module for `Option<BigDecimal>` <-> JSON string / `null`.
pub mod bigdecimal_str_option {
    use std::str::FromStr;

    use bigdecimal::BigDecimal;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize `Some(v)` as a plain-decimal JSON string (never scientific
    /// notation, never a float) and `None` as JSON `null`.
    ///
    /// # Errors
    ///
    /// Forwards any error raised by the underlying `Serializer`.
    // `&Option<T>` is serde's required `with`-module signature for an
    // `Option`-typed field; `Option<&T>` is not an option here.
    #[allow(clippy::ref_option)]
    pub fn serialize<S>(value: &Option<BigDecimal>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(v) => serializer.serialize_some(&v.to_plain_string()),
            None => serializer.serialize_none(),
        }
    }

    /// Deserialize a JSON string into `Some(BigDecimal)` or `null` into `None`.
    /// A bare JSON number is rejected (the wire contract is string-only).
    ///
    /// # Errors
    ///
    /// Returns an error when the value is neither a JSON string nor `null`, or
    /// when [`BigDecimal::from_str`] fails to parse the string as a decimal.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<BigDecimal>, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Option::<String>::deserialize(deserializer)? {
            Some(s) => BigDecimal::from_str(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "serde_helpers_tests.rs"]
mod serde_helpers_tests;
