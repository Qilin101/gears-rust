// Created: 2026-08-06 by Constructor Tech
//! `OData` filter / order surface for [`ModelRegistryClientV1::list_providers`].
//!
//! [`ModelRegistryClientV1::list_providers`]: crate::ModelRegistryClientV1::list_providers

use toolkit_odata::filter::FilterField as _;
use toolkit_odata::{FieldRef, Schema};
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

/// Schema marker binding [`ProviderFilterField`] to the typed
/// [`QueryBuilder`](toolkit_odata::QueryBuilder).
#[derive(Debug, Clone, Copy)]
pub struct ProviderSchema;

impl Schema for ProviderSchema {
    type Field = ProviderFilterField;

    fn field_name(field: Self::Field) -> &'static str {
        field.name()
    }
}

// ---------------------------------------------------------------------------
// Typed field references
// ---------------------------------------------------------------------------

pub const PROVIDER_SLUG: FieldRef<ProviderSchema, String> =
    FieldRef::new(ProviderFilterField::Slug);
pub const PROVIDER_NAME: FieldRef<ProviderSchema, String> =
    FieldRef::new(ProviderFilterField::Name);
pub const PROVIDER_STATUS: FieldRef<ProviderSchema, String> =
    FieldRef::new(ProviderFilterField::Status);
pub const PROVIDER_GTS_TYPE: FieldRef<ProviderSchema, String> =
    FieldRef::new(ProviderFilterField::GtsType);
pub const PROVIDER_MANAGED: FieldRef<ProviderSchema, bool> =
    FieldRef::new(ProviderFilterField::Managed);
pub const PROVIDER_DISCOVERY_ENABLED: FieldRef<ProviderSchema, bool> =
    FieldRef::new(ProviderFilterField::DiscoveryEnabled);
