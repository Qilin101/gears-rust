// Created: 2026-08-06 by Constructor Tech
//! `OData` filter / order surface for [`ModelRegistryClientV1::list_providers`].
//!
//! [`ModelRegistryClientV1::list_providers`]: crate::ModelRegistryClientV1::list_providers

use toolkit_odata_macros::ODataFilterable;

/// Filterable / orderable wire surface of the provider listing.
///
/// Every field maps to a real `providers` column.
///
/// Never constructed at runtime: the struct exists to carry the annotations —
/// the generated [`ProviderFilterField`] enum is the usable contract.
#[derive(ODataFilterable)]
pub struct ProviderQuery {
    #[odata(filter(kind = "String"))]
    pub slug: String,
    #[odata(filter(kind = "String"))]
    pub name: String,
    #[odata(filter(kind = "String"))]
    pub status: String,
    #[odata(filter(kind = "String"))]
    pub gts_type: String,
    #[odata(filter(kind = "Bool"))]
    pub managed: bool,
    #[odata(filter(kind = "Bool"))]
    pub discovery_enabled: bool,
}

/// `OData` filter / order field enum for the provider listing, generated from
/// [`ProviderQuery`].
pub use ProviderQueryFilterField as ProviderFilterField;

#[cfg(test)]
#[path = "providers_tests.rs"]
mod providers_tests;
