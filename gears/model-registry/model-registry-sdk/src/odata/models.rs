// Created: 2026-08-06 by Constructor Tech
//! `OData` filter / order surface for [`ModelRegistryClientV1::list_tenant_models`].
//!
//! [`ModelRegistryClientV1::list_tenant_models`]: crate::ModelRegistryClientV1::list_tenant_models

use toolkit_odata_macros::ODataFilterable;

/// Filterable / orderable wire surface of the model listing.
///
/// Every field maps to a real `models` column (including the capability flags
/// and `approval_status`). Non-allowlisted fields (`provider_settings.*`,
/// `default_parameters.*`, `additional_info.*`, per-MIME array fields) are
/// absent here and therefore rejected at the parser level. Names are flat —
/// `gts_type`, not `info.gts_type`.
///
/// Never constructed at runtime: the struct exists to carry the annotations —
/// the generated [`ModelFilterField`] enum is the usable contract.
#[derive(ODataFilterable)]
#[allow(clippy::struct_excessive_bools)]
pub struct ModelQuery {
    #[odata(filter(kind = "String"))]
    pub canonical_id: String,
    #[odata(filter(kind = "String"))]
    pub lifecycle_status: String,
    #[odata(filter(kind = "String"))]
    pub approval_status: String,
    #[odata(filter(kind = "String"))]
    pub gts_type: String,
    #[odata(filter(kind = "String"))]
    pub supported_api: String,
    #[odata(filter(kind = "String"))]
    pub provider_model_id: String,
    #[odata(filter(kind = "String"))]
    pub vendor: String,
    #[odata(filter(kind = "String"))]
    pub family: String,
    #[odata(filter(kind = "Bool"))]
    pub managed: bool,
    #[odata(filter(kind = "String"))]
    pub architecture: String,
    #[odata(filter(kind = "String"))]
    pub format: String,
    #[odata(filter(kind = "Bool"))]
    pub vision: bool,
    #[odata(filter(kind = "Bool"))]
    pub function_calling: bool,
    #[odata(filter(kind = "Bool"))]
    pub streaming: bool,
    #[odata(filter(kind = "Bool"))]
    pub reasoning_effort: bool,
}

/// `OData` filter / order field enum for the model listing, generated from
/// [`ModelQuery`].
pub use ModelQueryFilterField as ModelFilterField;

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;
