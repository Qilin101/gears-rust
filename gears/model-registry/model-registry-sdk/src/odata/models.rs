// Created: 2026-08-06 by Constructor Tech
//! `OData` filter / order surface for [`ModelRegistryClientV1::list_tenant_models`].
//!
//! [`ModelRegistryClientV1::list_tenant_models`]: crate::ModelRegistryClientV1::list_tenant_models

use toolkit_odata::filter::FilterField as _;
use toolkit_odata::{FieldRef, Schema};
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

/// Schema marker binding [`ModelFilterField`] to the typed
/// [`QueryBuilder`](toolkit_odata::QueryBuilder).
#[derive(Debug, Clone, Copy)]
pub struct ModelSchema;

impl Schema for ModelSchema {
    type Field = ModelFilterField;

    fn field_name(field: Self::Field) -> &'static str {
        field.name()
    }
}

// ---------------------------------------------------------------------------
// Typed field references
// ---------------------------------------------------------------------------
//
// The `String` / `bool` type parameter gates the string-only operators
// (`contains` / `startswith` / `endswith`) — it does NOT constrain the value
// passed to `eq` / `ne`, which accepts any `IntoODataValue` (see
// `toolkit_odata::schema`). Enum-valued fields still take their SDK enum
// directly via the `IntoODataValue` impls in `super::values`.

pub const MODEL_CANONICAL_ID: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::CanonicalId);
pub const MODEL_LIFECYCLE_STATUS: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::LifecycleStatus);
pub const MODEL_APPROVAL_STATUS: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::ApprovalStatus);
pub const MODEL_GTS_TYPE: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::GtsType);
pub const MODEL_SUPPORTED_API: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::SupportedApi);
pub const MODEL_PROVIDER_MODEL_ID: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::ProviderModelId);
pub const MODEL_VENDOR: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::Vendor);
pub const MODEL_FAMILY: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::Family);
pub const MODEL_ARCHITECTURE: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::Architecture);
pub const MODEL_FORMAT: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::Format);
pub const MODEL_MANAGED: FieldRef<ModelSchema, bool> = FieldRef::new(ModelFilterField::Managed);
pub const MODEL_VISION: FieldRef<ModelSchema, bool> = FieldRef::new(ModelFilterField::Vision);
pub const MODEL_FUNCTION_CALLING: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::FunctionCalling);
pub const MODEL_STREAMING: FieldRef<ModelSchema, bool> = FieldRef::new(ModelFilterField::Streaming);
pub const MODEL_REASONING_EFFORT: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::ReasoningEffort);
