//! REST route definitions for the Model Registry gear.
//!
//! Registers the gear's endpoints using the `OperationBuilder` pattern.

use std::sync::Arc;

use axum::{Extension, Router, http::StatusCode};
use model_registry_sdk::odata::{ModelFilterField, ProviderFilterField};
use toolkit::api::operation_builder::OperationBuilderODataExt;
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::dto::{self, ModelManagementListDto};
use super::handlers;
use crate::domain::service::Service;
use crate::infra::storage::model_repo::ModelRepositoryImpl;
use crate::infra::storage::provider_repo::ProviderRepositoryImpl;

/// Concrete service type used by all routes.
pub type ConcreteService = Service<ProviderRepositoryImpl, ModelRepositoryImpl>;

/// License feature identifier for model-registry endpoints.
struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        "gts.cf.core.lic.feat.v1~cf.core.global.base.v1"
    }
}

impl toolkit::api::operation_builder::LicenseFeature for License {}

/// Register all P1 REST endpoints on the given router.
///
/// Two authorization zones:
///
/// - `/model-registry/v1/…` — eval-facing reads any authenticated tenant member
///   may call. Keyed by `canonical_id`, because that is the only identifier an
///   inference caller holds, and resolved across the caller's tenant ancestor
///   chain.
/// - `/model-registry/v1/admin/…` — management surface for tenant-admin /
///   platform-admin, bounded by the PDP access scope alone. Every path is keyed
///   by the entity's `Uuid`: a `canonical_id` is chain-relative (the same string
///   resolves to different rows once a provider slug is shadowed) and a provider
///   slug is not a URL-safe stable handle, so neither can address a write.
#[allow(clippy::too_many_lines)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteService>,
) -> Router {
    // ═══════════════════════════════════════════════════════════════════════
    // Eval zone — /model-registry/v1 (any authenticated tenant member)
    // ═══════════════════════════════════════════════════════════════════════

    // GET /model-registry/v1/models — eval listing
    router = OperationBuilder::get("/model-registry/v1/models")
        .operation_id("model_registry.list_tenant_models")
        .summary("List models available for eval")
        .description(
            "List models available for eval to the caller's tenant, with OData filtering. \
             Returns only approved models on active, non-shadowed providers, and excludes \
             deprecated/sunset models; `$filter` narrows within that set and never widens it. \
             For the full catalog see GET /model-registry/v1/admin/models.",
        )
        .tag("Models")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_models)
        .json_response_with_schema::<dto::ModelListDto>(
            openapi,
            StatusCode::OK,
            "Paginated model list",
        )
        .with_odata_filter::<ModelFilterField>()
        .with_odata_orderby::<ModelFilterField>()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /model-registry/v1/models/{canonical_id} — eval read
    router = OperationBuilder::get("/model-registry/v1/models/{canonical_id}")
        .operation_id("model_registry.get_tenant_model")
        .summary("Get a model by canonical ID")
        .description(
            "Get a model by canonical ID (`{provider_slug}::{provider_model_id}`). \
             Eval-facing: returns the model only when it is approved, live, and on an active \
             winning provider; otherwise 404 (unknown or deprecated) or 403 (provider disabled, \
             or model not approved). For the ungated management read see \
             GET /model-registry/v1/admin/models/{id}.",
        )
        .tag("Models")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_tenant_model)
        .json_response_with_schema::<dto::ModelDto>(openapi, StatusCode::OK, "Model details")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // ═══════════════════════════════════════════════════════════════════════
    // Admin zone — models (/model-registry/v1/admin/models)
    // ═══════════════════════════════════════════════════════════════════════

    // GET /model-registry/v1/admin/models — management listing
    router = OperationBuilder::get("/model-registry/v1/admin/models")
        .operation_id("model_registry.list_management_models")
        .summary("List models with management flags")
        .description(
            "List every model in the caller's access scope, with no eval gates, plus the \
             provider-visibility flags `provider_disabled` and `available_for_eval`. \
             Intended for admin UIs.",
        )
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_management_models)
        .json_response_with_schema::<ModelManagementListDto>(
            openapi,
            StatusCode::OK,
            "Paginated model management list",
        )
        .with_odata_filter::<ModelFilterField>()
        .with_odata_orderby::<ModelFilterField>()
        .query_param_typed(
            "include_deprecated",
            false,
            "Include deprecated and sunset models in the response (default: false)",
            "boolean",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /model-registry/v1/admin/models — create
    router = OperationBuilder::post("/model-registry/v1/admin/models")
        .operation_id("model_registry.create_model")
        .summary("Create a model")
        .description(
            "Manually register a new model in the catalog. `provider_id` is resolved within \
             the caller's access scope; the created model belongs to that provider's tenant. \
             The server derives `canonical_id` from the resolved provider's slug and the \
             supplied `provider_model_id`.",
        )
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateModelRequestDto>(openapi, "Model creation data")
        .handler(handlers::create_model)
        .json_response_with_schema::<dto::ModelDto>(openapi, StatusCode::CREATED, "Model created")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /model-registry/v1/admin/models/{id} — management read
    router = OperationBuilder::get("/model-registry/v1/admin/models/{id}")
        .operation_id("model_registry.get_model")
        .summary("Get a model by ID")
        .description(
            "Get a model by UUID with management semantics: no approval, lifecycle, or \
             provider-status gates, resolved within the caller's access scope. The row may be \
             pending, rejected, deprecated, or on a disabled provider; check \
             GET /model-registry/v1/admin/models for `available_for_eval`.",
        )
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_model)
        .json_response_with_schema::<dto::ModelDto>(openapi, StatusCode::OK, "Model details")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PATCH /model-registry/v1/admin/models/{id} — update
    router = OperationBuilder::patch("/model-registry/v1/admin/models/{id}")
        .operation_id("model_registry.update_model")
        .summary("Update a model")
        .description("Partially update a model (PATCH semantics), including approval status.")
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpdateModelRequestDto>(openapi, "Model update data")
        .handler(handlers::update_model)
        .json_response_with_schema::<dto::ModelDto>(openapi, StatusCode::OK, "Model updated")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /model-registry/v1/admin/models/{id} — soft delete
    router = OperationBuilder::delete("/model-registry/v1/admin/models/{id}")
        .operation_id("model_registry.delete_model")
        .summary("Delete a model")
        .description("Soft-delete a model by ID (marks it `deprecated`)")
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::delete_model)
        .no_content_response(StatusCode::NO_CONTENT, "Model deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // ═══════════════════════════════════════════════════════════════════════
    // Admin zone — providers (/model-registry/v1/admin/providers)
    // ═══════════════════════════════════════════════════════════════════════

    // GET /model-registry/v1/admin/providers — list
    router = OperationBuilder::get("/model-registry/v1/admin/providers")
        .operation_id("model_registry.list_providers")
        .summary("List providers")
        .description("List the providers in the caller's access scope, with OData filtering")
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_providers)
        .json_response_with_schema::<dto::ProviderListDto>(
            openapi,
            StatusCode::OK,
            "Paginated provider list",
        )
        .with_odata_filter::<ProviderFilterField>()
        .with_odata_orderby::<ProviderFilterField>()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /model-registry/v1/admin/providers — create
    router = OperationBuilder::post("/model-registry/v1/admin/providers")
        .operation_id("model_registry.create_provider")
        .summary("Create a provider")
        .description(
            "Register a new provider for the caller's tenant. `slug` is the tenant-unique \
             natural key and is immutable after creation; every later operation addresses the \
             provider by the returned `id`.",
        )
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateProviderRequestDto>(openapi, "Provider creation data")
        .handler(handlers::create_provider)
        .json_response_with_schema::<dto::ProviderDto>(
            openapi,
            StatusCode::CREATED,
            "Provider created",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /model-registry/v1/admin/providers/{id} — get
    router = OperationBuilder::get("/model-registry/v1/admin/providers/{id}")
        .operation_id("model_registry.get_provider")
        .summary("Get a provider")
        .description("Get a provider by ID within the caller's access scope")
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_provider)
        .json_response_with_schema::<dto::ProviderDto>(openapi, StatusCode::OK, "Provider details")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PATCH /model-registry/v1/admin/providers/{id} — update
    router = OperationBuilder::patch("/model-registry/v1/admin/providers/{id}")
        .operation_id("model_registry.update_provider")
        .summary("Update a provider")
        .description(
            "Partially update a provider (PATCH semantics). `slug` is immutable and ignored \
             if present.",
        )
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpdateProviderRequestDto>(openapi, "Provider update data")
        .handler(handlers::update_provider)
        .json_response_with_schema::<dto::ProviderDto>(openapi, StatusCode::OK, "Provider updated")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /model-registry/v1/admin/providers/{id} — delete
    router = OperationBuilder::delete("/model-registry/v1/admin/providers/{id}")
        .operation_id("model_registry.delete_provider")
        .summary("Delete a provider")
        .description(
            "Delete a provider by ID. Refused with 400 `failed_precondition` while any model \
             still references it; soft-deleted (deprecated) models count.",
        )
        .tag("Admin")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::delete_provider)
        .no_content_response(StatusCode::NO_CONTENT, "Provider deleted")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // Attach the service as axum Extension so handlers can access it
    router = router.layer(Extension(service));

    router
}
