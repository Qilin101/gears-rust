//! The twelve routes of DESIGN §3.3, one per SDK method, registered with
//! `OperationBuilder`.

use std::sync::Arc;

use axum::{Extension, Router, http::StatusCode};
use model_registry_sdk::odata::{ModelFilterField, ProviderFilterField};
use toolkit::api::operation_builder::OperationBuilderODataExt;
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::dto;
use super::handlers;
use crate::domain::service::Service;

/// Platform base license; the gear has no feature of its own.
struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        "gts.cf.core.lic.feat.v1~cf.core.global.base.v1"
    }
}

impl toolkit::api::operation_builder::LicenseFeature for License {}

#[allow(clippy::too_many_lines)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<Service>,
) -> Router {
    // ==================== Resolution ====================

    router = OperationBuilder::get("/model-registry/v1/available-models/{canonical_id}")
        .operation_id("model_registry.resolve_model")
        .summary("Resolve a model for evaluation")
        .description(
            "Resolve a canonical id (`provider_slug::provider_model_id`, percent-encoded as one path \
             segment) in the caller's effective catalog. Succeeds only when the model is visible, its \
             provider is enabled, it is approved, and the caller may evaluate it. The record omits \
             `provider_settings`, `base_url`, and `secret_ref`.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::resolve_model)
        .json_response_with_schema::<dto::AvailableModelDto>(openapi, StatusCode::OK, "The model")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/available-models")
        .operation_id("model_registry.list_available_models")
        .summary("List the models available for evaluation")
        .description(
            "Exactly the models the caller could resolve, without `provider_settings`, `base_url`, \
             and `secret_ref`.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_available_models)
        .json_response_with_schema::<dto::AvailableModelPageDto>(
            openapi,
            StatusCode::OK,
            "A page of models",
        )
        .with_odata_filter::<ModelFilterField>()
        .with_odata_orderby::<ModelFilterField>()
        .with_odata_select()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // ==================== Providers ====================

    router = OperationBuilder::post("/model-registry/v1/providers")
        .operation_id("model_registry.create_provider")
        .summary("Create a provider")
        .description(
            "Create a provider in the caller's tenant. `slug` and `gts_type` are immutable.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateProviderRequestDto>(openapi, "Provider to create")
        .handler(handlers::create_provider)
        .json_response_with_schema::<dto::ProviderEntryDto>(
            openapi,
            StatusCode::CREATED,
            "Provider created",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.get_provider")
        .summary("Get an own or inherited provider")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_provider)
        .json_response_with_schema::<dto::ProviderEntryDto>(openapi, StatusCode::OK, "The provider")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/providers")
        .operation_id("model_registry.list_providers")
        .summary("List own and inherited providers")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_providers)
        .json_response_with_schema::<dto::ProviderEntryPageDto>(
            openapi,
            StatusCode::OK,
            "A page of providers",
        )
        .with_odata_filter::<ProviderFilterField>()
        .with_odata_orderby::<ProviderFilterField>()
        .with_odata_select()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::patch("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.update_provider")
        .summary("Update an own provider")
        .description("Present fields replace the stored values. Immutable fields are rejected.")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpdateProviderRequestDto>(openapi, "Fields to change")
        .handler(handlers::update_provider)
        .json_response_with_schema::<dto::ProviderEntryDto>(
            openapi,
            StatusCode::OK,
            "Provider updated",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.delete_provider")
        .summary("Delete an own provider and its models")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::delete_provider)
        .no_content_response(StatusCode::NO_CONTENT, "Provider deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // ==================== Models ====================

    router = OperationBuilder::post("/model-registry/v1/models")
        .operation_id("model_registry.create_model")
        .summary("Create a model under an own provider")
        .description(
            "The gear builds `canonical_id`, copies `gts_type` from the provider, validates \
             `provider_settings` against the provider type schema, and records the approval decision.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateModelRequestDto>(openapi, "Model to create")
        .handler(handlers::create_model)
        .json_response_with_schema::<dto::ModelEntryDto>(openapi, StatusCode::CREATED, "Model created")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/models/{id}")
        .operation_id("model_registry.get_model")
        .summary("Get an own or inherited model, in any state")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_model)
        .json_response_with_schema::<dto::ModelEntryDto>(openapi, StatusCode::OK, "The model")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/models")
        .operation_id("model_registry.list_models")
        .summary("List own and inherited models in every state")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_models)
        .json_response_with_schema::<dto::ModelEntryPageDto>(
            openapi,
            StatusCode::OK,
            "A page of models",
        )
        .with_odata_filter::<ModelFilterField>()
        .with_odata_orderby::<ModelFilterField>()
        .with_odata_select()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::patch("/model-registry/v1/models/{id}")
        .operation_id("model_registry.update_model")
        .summary("Update an own model")
        .description(
            "Present fields replace the stored values; a present `approval_state` records a decision; \
             a present `provider_settings` is validated. Immutable fields are rejected.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpdateModelRequestDto>(openapi, "Fields to change")
        .handler(handlers::update_model)
        .json_response_with_schema::<dto::ModelEntryDto>(openapi, StatusCode::OK, "Model updated")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/model-registry/v1/models/{id}")
        .operation_id("model_registry.delete_model")
        .summary("Delete an own model")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::delete_model)
        .no_content_response(StatusCode::NO_CONTENT, "Model deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router.layer(Extension(service))
}
