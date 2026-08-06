// Created: 2026-08-06 by Constructor Tech
//! `OData` filter / order surface for the `model-registry` listing methods.
//!
//! The filterable wire surface is declared once per resource, as an annotated
//! query struct; `#[derive(ODataFilterable)]` generates the `FilterField` enum
//! (`<Struct>FilterField`) with its `FIELDS` / `name()` / `kind()` impl. The
//! enums are the public contract shared by three consumers: the `$filter` /
//! `$orderby` allowlist on [`crate::ModelRegistryClientV1`]'s [`ODataQuery`]
//! arguments, the `OpenAPI` query-parameter documentation, and the gear's
//! `FieldToColumn` binding to real database columns.
//!
//! Each filter field maps to a real column — the toolkit `OData` layer
//! (`toolkit-db` / `sea_orm_filter`) supports neither JSONB-path filtering nor
//! joins — but that binding stays in the gear crate, since it names storage
//! types the SDK does not expose.
//!
//! [`ODataQuery`]: toolkit_odata::ODataQuery

mod models;
mod providers;

pub use models::{ModelFilterField, ModelQuery};
pub use providers::{ProviderFilterField, ProviderQuery};
