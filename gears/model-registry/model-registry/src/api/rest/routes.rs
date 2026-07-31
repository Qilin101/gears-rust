//! REST route definitions for the Model Registry gear.
//!
//! Registers the gear's endpoints using the `OperationBuilder` pattern.

use std::sync::Arc;

use axum::{Extension, Router, http::StatusCode};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::dto;
use super::handlers;
use crate::domain::cache::InMemoryCache;
use crate::domain::service::Service;
use crate::infra::storage::model_repo::ModelRepositoryImpl;
use crate::infra::storage::provider_repo::ProviderRepositoryImpl;

/// Concrete service type used by all routes.
pub type ConcreteService = Service<ProviderRepositoryImpl, ModelRepositoryImpl, InMemoryCache>;

/// License feature identifier for model-registry endpoints.
struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        "gts.cf.core.lic.feat.v1~cf.core.global.base.v1"
    }
}

impl toolkit::api::operation_builder::LicenseFeature for License {}

/// Register all P1 REST endpoints on the given router.
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteService>,
) -> Router {
    // ═══════════════════════════════════════════════════════════════════════
    // Provider endpoints (5)
    // ═══════════════════════════════════════════════════════════════════════

    // GET /model-registry/v1/providers — list
    router = OperationBuilder::get("/model-registry/v1/providers")
        .operation_id("model_registry.list_providers")
        .summary("List providers")
        .description("List providers visible to the caller's tenant with OData filtering")
        .tag("Providers")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_providers)
        .json_response_with_schema::<dto::ProviderListDto>(
            openapi,
            StatusCode::OK,
            "Paginated provider list",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /model-registry/v1/providers — create
    router = OperationBuilder::post("/model-registry/v1/providers")
        .operation_id("model_registry.create_provider")
        .summary("Create a provider")
        .description("Register a new provider for the caller's tenant")
        .tag("Providers")
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

    // GET /model-registry/v1/providers/{id} — get
    router = OperationBuilder::get("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.get_provider")
        .summary("Get a provider")
        .description("Get a provider by ID with cache-first lookup")
        .tag("Providers")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_provider)
        .json_response_with_schema::<dto::ProviderDto>(openapi, StatusCode::OK, "Provider details")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PATCH /model-registry/v1/providers/{id} — update
    router = OperationBuilder::patch("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.update_provider")
        .summary("Update a provider")
        .description("Partially update a provider (PATCH semantics)")
        .tag("Providers")
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

    // DELETE /model-registry/v1/providers/{id} — delete
    router = OperationBuilder::delete("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.delete_provider")
        .summary("Delete a provider")
        .description("Delete a provider by ID")
        .tag("Providers")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::delete_provider)
        .no_content_response(StatusCode::NO_CONTENT, "Provider deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // ═══════════════════════════════════════════════════════════════════════
    // Model endpoints (5)
    // ═══════════════════════════════════════════════════════════════════════

    // GET /model-registry/v1/models — list
    router = OperationBuilder::get("/model-registry/v1/models")
        .operation_id("model_registry.list_models")
        .summary("List models")
        .description("List models visible to the caller's tenant with OData filtering")
        .tag("Models")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_models)
        .json_response_with_schema::<dto::ModelListDto>(
            openapi,
            StatusCode::OK,
            "Paginated model list",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /model-registry/v1/models — create
    router = OperationBuilder::post("/model-registry/v1/models")
        .operation_id("model_registry.create_model")
        .summary("Create a model")
        .description("Manually register a new model in the catalog")
        .tag("Models")
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

    // GET /model-registry/v1/models/{canonical_id} — get
    router = OperationBuilder::get("/model-registry/v1/models/{canonical_id}")
        .operation_id("model_registry.get_model")
        .summary("Get a model")
        .description("Get a model by canonical ID with cache-first lookup")
        .tag("Models")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_model)
        .json_response_with_schema::<dto::ModelDto>(openapi, StatusCode::OK, "Model details")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PATCH /model-registry/v1/models/{canonical_id} — update
    router = OperationBuilder::patch("/model-registry/v1/models/{canonical_id}")
        .operation_id("model_registry.update_model")
        .summary("Update a model")
        .description("Partially update a model (PATCH semantics), including approval status")
        .tag("Models")
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

    // DELETE /model-registry/v1/models/{canonical_id} — delete
    router = OperationBuilder::delete("/model-registry/v1/models/{canonical_id}")
        .operation_id("model_registry.delete_model")
        .summary("Delete a model")
        .description("Soft-delete a model by canonical ID")
        .tag("Models")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::delete_model)
        .no_content_response(StatusCode::NO_CONTENT, "Model deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // Attach the service as axum Extension so handlers can access it
    router = router.layer(Extension(service));

    router
}
