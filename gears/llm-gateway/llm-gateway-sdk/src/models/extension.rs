// Created: 2026-07-14 by Constructor Tech
//! Open-extension support for the `type`-discriminated model families.
//!
//! The Open Responses wire protocol is an open set of types keyed by a
//! namespaced `type` string (`{provider_slug}:{type}`, `cf_gears:…`). Each core
//! family ([`OutputItem`](super::items::OutputItem),
//! [`Tool`](super::tools::Tool), [`StreamingEvent`](super::streaming::StreamingEvent),
//! …) is a flat enum with one variant per core-owned type plus an `Other`
//! variant holding an [`Extension`]. Any `type` the core does not own
//! deserializes into `Other` verbatim and is forwarded without interpretation
//! (per `principle-content-non-interpretation`); a consumer that has the
//! provider's crate projects it into a typed view with [`Extension::decode`].
//!
//! [`serialize_tagged`], [`tag_of`], and [`from_tagged`] are the shared pieces
//! the families' hand-written `Serialize`/`Deserialize` impls build on — derive
//! cannot express a data-carrying catch-all on an internally-tagged enum.

use serde::de::DeserializeOwned;

/// A `type`-discriminated value whose `type` is not one the core owns — a
/// provider or third-party plugin extension.
///
/// Preserved verbatim so the gateway can forward it unchanged. Decode into a
/// provider-defined type with [`Extension::decode`] after checking [`kind`](Self::kind).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Extension(pub serde_json::Value);

impl Extension {
    /// The `type` discriminator, if present.
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        self.0.get("type").and_then(serde_json::Value::as_str)
    }

    /// Project into a provider-defined typed view. The caller is expected to
    /// have matched [`kind`](Self::kind) first.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the preserved value does not conform
    /// to `T`.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        T::deserialize(&self.0)
    }
}

/// Serialize `value` as a JSON object with an injected `type` discriminator.
///
/// Used by the families' `Serialize` impls for their core-owned variants; the
/// payload structs carry no `type` field of their own.
pub(crate) fn serialize_tagged<S, T>(serializer: S, tag: &str, value: &T) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    T: serde::Serialize,
{
    let mut v = serde_json::to_value(value).map_err(<S::Error as serde::ser::Error>::custom)?;
    match v.as_object_mut() {
        Some(map) => {
            map.insert(
                String::from("type"),
                serde_json::Value::String(String::from(tag)),
            );
        }
        None => {
            return Err(<S::Error as serde::ser::Error>::custom(
                "tagged enum payload did not serialize to a JSON object",
            ));
        }
    }
    serde::Serialize::serialize(&v, serializer)
}

/// The `type` discriminator of an already-parsed value, if present.
pub(crate) fn tag_of(value: &serde_json::Value) -> Option<&str> {
    value.get("type").and_then(serde_json::Value::as_str)
}

/// Deserialize a core-owned variant payload from an already-parsed value.
pub(crate) fn from_tagged<T, E>(value: &serde_json::Value) -> Result<T, E>
where
    T: DeserializeOwned,
    E: serde::de::Error,
{
    T::deserialize(value).map_err(E::custom)
}
