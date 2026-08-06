//! REST handlers for the Model Registry gear.
//!
//! Each handler corresponds to an endpoint defined by
//! `ModelRegistryClientV1`. Handlers extract axum request parts, delegate to
//! the service layer, and map to the `ApiResult` / `CanonicalError` pattern.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{
    CreateModelRequestDto, CreateProviderRequestDto, ModelDto, ModelListDto, ProviderDto,
    ProviderListDto, UpdateModelRequestDto, UpdateProviderRequestDto,
};
use super::error::ModelRegistryResourceError;
use super::parse;
use crate::domain::cache::InMemoryCache;
use crate::domain::service::Service;
use crate::infra::storage::model_repo::ModelRepositoryImpl;
use crate::infra::storage::provider_repo::ProviderRepositoryImpl;

/// Concrete service type used by all handlers.
type ConcreteService = Service<ProviderRepositoryImpl, ModelRepositoryImpl, InMemoryCache>;

// ═════════════════════════════════════════════════════════════════════════════
// Provider handlers
// ═════════════════════════════════════════════════════════════════════════════

/// `GET /model-registry/v1/providers/{id}`
pub async fn get_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(id): Path<Uuid>,
) -> ApiResult<JsonBody<ProviderDto>> {
    let provider = svc.get_provider(&ctx, id).await?;
    Ok(Json(ProviderDto::from(provider)))
}

/// `GET /model-registry/v1/providers`
pub async fn list_providers(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    OData(query): OData,
) -> ApiResult<JsonBody<ProviderListDto>> {
    parse::reject_select(&query)?;
    let page = svc.list_providers(&ctx, &query).await?;

    let items: Vec<ProviderDto> = page.items.into_iter().map(ProviderDto::from).collect();

    Ok(Json(ProviderListDto {
        items,
        page_info: super::dto::PageInfoDto {
            next_cursor: page.page_info.next_cursor,
            prev_cursor: page.page_info.prev_cursor,
            limit: u32::try_from(page.page_info.limit)
                .map_err(|_| CanonicalError::internal("page limit exceeds u32 range").create())?,
        },
    }))
}

/// `POST /model-registry/v1/providers`
pub async fn create_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(dto): Json<CreateProviderRequestDto>,
) -> ApiResult<(StatusCode, JsonBody<ProviderDto>)> {
    // Parse GTS type
    let gts_type = gts::GtsTypeId::new(&dto.gts_type);

    // Build SDK request via builder
    let req = crate::CreateProviderRequestV1::builder(&dto.slug, &dto.name, gts_type)
        .managed(dto.managed)
        .discovery_enabled(dto.discovery_enabled);

    let req = if let Some(metadata) = dto.metadata {
        req.metadata(metadata)
    } else {
        req
    };

    let req = if let Some(interval) = dto.discovery_interval_seconds {
        req.discovery_interval_seconds(interval)
    } else {
        req
    };

    let provider = svc.create_provider(&ctx, &req.build()).await?;
    let dto: ProviderDto = provider.into();
    Ok((StatusCode::CREATED, Json(dto)))
}

/// `PATCH /model-registry/v1/providers/{id}`
pub async fn update_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(id): Path<Uuid>,
    Json(dto): Json<UpdateProviderRequestDto>,
) -> ApiResult<JsonBody<ProviderDto>> {
    let status = dto
        .status
        .as_deref()
        .map(parse::provider_status)
        .transpose()?;

    let sdk_req = crate::UpdateProviderRequestV1 {
        name: dto.name,
        status,
        managed: dto.managed,
        metadata: dto.metadata,
        discovery_enabled: dto.discovery_enabled,
        discovery_interval_seconds: dto.discovery_interval_seconds,
    };

    let provider = svc.update_provider(&ctx, id, &sdk_req).await?;
    let dto: ProviderDto = provider.into();
    Ok(Json(dto))
}

/// `DELETE /model-registry/v1/providers/{id}`
pub async fn delete_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_provider(&ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ═════════════════════════════════════════════════════════════════════════════
// Model handlers
// ═════════════════════════════════════════════════════════════════════════════

/// `GET /model-registry/v1/models/{canonical_id}`
pub async fn get_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(canonical_id): Path<String>,
) -> ApiResult<JsonBody<ModelDto>> {
    let model = svc.get_tenant_model(&ctx, &canonical_id).await?;
    Ok(Json(ModelDto::from(model)))
}

/// `GET /model-registry/v1/models`
pub async fn list_models(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    OData(query): OData,
) -> ApiResult<JsonBody<ModelListDto>> {
    parse::reject_select(&query)?;
    let page = svc.list_tenant_models(&ctx, &query).await?;

    let items: Vec<ModelDto> = page.items.into_iter().map(ModelDto::from).collect();

    Ok(Json(ModelListDto {
        items,
        page_info: super::dto::PageInfoDto {
            next_cursor: page.page_info.next_cursor,
            prev_cursor: page.page_info.prev_cursor,
            limit: u32::try_from(page.page_info.limit)
                .map_err(|_| CanonicalError::internal("page limit exceeds u32 range").create())?,
        },
    }))
}

/// `POST /model-registry/v1/models`
pub async fn create_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(dto): Json<CreateModelRequestDto>,
) -> ApiResult<(StatusCode, JsonBody<ModelDto>)> {
    let lifecycle_status = parse::lifecycle_status(&dto.lifecycle_status)?;
    let approval_status = dto
        .approval_status
        .as_deref()
        .map(parse::approval_status)
        .transpose()?;

    // Parse info JSON into ModelInfoV1
    let info: model_registry_sdk::models::ModelInfoV1<serde_json::Value> =
        serde_json::from_value(dto.info).map_err(|e| {
            ModelRegistryResourceError::invalid_argument()
                .with_field_violation("info", e.to_string(), "INVALID_MODEL_INFO")
                .create()
        })?;

    let req = crate::CreateModelRequestV1 {
        provider_slug: dto.provider_slug,
        lifecycle_status,
        approval_status,
        info,
    };

    let model = svc.create_model(&ctx, &req).await?;
    let dto: ModelDto = model.into();
    Ok((StatusCode::CREATED, Json(dto)))
}

/// `PATCH /model-registry/v1/models/{canonical_id}`
pub async fn update_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(canonical_id): Path<String>,
    Json(dto): Json<UpdateModelRequestDto>,
) -> ApiResult<JsonBody<ModelDto>> {
    let approval_status = dto
        .approval_status
        .as_deref()
        .map(parse::approval_status)
        .transpose()?;
    let lifecycle_status = dto
        .lifecycle_status
        .as_deref()
        .map(parse::lifecycle_status)
        .transpose()?;

    // Build SDK request from DTO fields
    let sdk_req = crate::UpdateModelRequestV1 {
        approval_status,
        lifecycle_status,
        display_name: dto.display_name,
        description: dto.description,
        family: dto.family,
        vendor: dto.vendor,
        managed: dto.managed,
        architecture: dto.architecture,
        size_bytes: dto.size_bytes,
        format: dto.format,
        region: dto.region,
        hosted_by: dto.hosted_by,
        reasoning_level: dto.reasoning_level,
        version: dto.version,
        sort_order: dto.sort_order,
        icon: dto.icon,
        multiplier_display: dto.multiplier_display,
        performance: dto
            .performance
            .map(|v| {
                serde_json::from_value(v).map_err(|e| {
                    ModelRegistryResourceError::invalid_argument()
                        .with_field_violation("performance", e.to_string(), "INVALID_PERFORMANCE")
                        .create()
                })
            })
            .transpose()?,
        capabilities: dto
            .capabilities
            .map(|v| {
                serde_json::from_value(v).map_err(|e| {
                    ModelRegistryResourceError::invalid_argument()
                        .with_field_violation("capabilities", e.to_string(), "INVALID_CAPABILITIES")
                        .create()
                })
            })
            .transpose()?,
        disabled_capabilities: dto
            .disabled_capabilities
            .map(|v| {
                serde_json::from_value(v).map_err(|e| {
                    ModelRegistryResourceError::invalid_argument()
                        .with_field_violation(
                            "disabled_capabilities",
                            e.to_string(),
                            "INVALID_DISABLED_CAPABILITIES",
                        )
                        .create()
                })
            })
            .transpose()?,
        context_window: dto
            .context_window
            .map(|v| {
                serde_json::from_value(v).map_err(|e| {
                    ModelRegistryResourceError::invalid_argument()
                        .with_field_violation(
                            "context_window",
                            e.to_string(),
                            "INVALID_CONTEXT_WINDOW",
                        )
                        .create()
                })
            })
            .transpose()?,
        default_parameters: dto
            .default_parameters
            .map(|v| {
                serde_json::from_value(v).map_err(|e| {
                    ModelRegistryResourceError::invalid_argument()
                        .with_field_violation(
                            "default_parameters",
                            e.to_string(),
                            "INVALID_DEFAULT_PARAMETERS",
                        )
                        .create()
                })
            })
            .transpose()?,
        allow_parameter_override: dto.allow_parameter_override,
        allow_extra_params: dto.allow_extra_params,
        provider_settings: dto.provider_settings,
    };

    let model = svc.update_model(&ctx, &canonical_id, &sdk_req).await?;
    let dto: ModelDto = model.into();
    Ok(Json(dto))
}

/// `DELETE /model-registry/v1/models/{canonical_id}`
pub async fn delete_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(canonical_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_model(&ctx, &canonical_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
