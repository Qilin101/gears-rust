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

//! # Building a query
//!
//! ```rust,ignore
//! use model_registry_sdk::odata::{
//!     ModelSchema, MODEL_CANONICAL_ID, MODEL_LIFECYCLE_STATUS, MODEL_STREAMING,
//!     QueryBuilder, SortDir,
//! };
//! use model_registry_sdk::LifecycleStatus;
//!
//! let query = QueryBuilder::<ModelSchema>::new()
//!     .filter(MODEL_STREAMING.eq(true).and(MODEL_LIFECYCLE_STATUS.eq(LifecycleStatus::Production)))
//!     .order_by(MODEL_CANONICAL_ID, SortDir::Asc)
//!     .page_size(50)
//!     .build();
//! let page = client.list_tenant_models(&ctx, &query).await?;
//! ```
//!
//! `build()` computes the `filter_hash` cursor pagination validates, so a
//! builder-constructed query is safe to page with. Values become AST literals
//! directly — there is no `$filter` text to quote or escape.
//!
//! `QueryBuilder::select` is inert here: this gear rejects `$select`, and
//! `list_tenant_models` / `list_providers` return a validation error for a
//! query that carries one.

mod models;
mod providers;
mod values;

pub use models::{
    MODEL_APPROVAL_STATUS, MODEL_ARCHITECTURE, MODEL_CANONICAL_ID, MODEL_FAMILY, MODEL_FORMAT,
    MODEL_FUNCTION_CALLING, MODEL_GTS_TYPE, MODEL_LIFECYCLE_STATUS, MODEL_MANAGED,
    MODEL_PROVIDER_MODEL_ID, MODEL_REASONING_EFFORT, MODEL_STREAMING, MODEL_SUPPORTED_API,
    MODEL_VENDOR, MODEL_VISION, ModelFilterField, ModelQuery, ModelSchema,
};
pub use providers::{
    PROVIDER_DISCOVERY_ENABLED, PROVIDER_GTS_TYPE, PROVIDER_MANAGED, PROVIDER_NAME, PROVIDER_SLUG,
    PROVIDER_STATUS, ProviderFilterField, ProviderQuery, ProviderSchema,
};

// Re-exported so a caller needs one import path to build a query.
pub use toolkit_odata::{FieldRef, QueryBuilder, Schema, SortDir};
