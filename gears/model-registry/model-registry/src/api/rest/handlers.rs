//! REST handlers: one catalog-service call each, plus `$select` projection
//! on the serialized response (DESIGN §3.2 REST API).

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::Uri;
use toolkit::api::canonical_prelude::*;
use toolkit::api::select::page_to_projected_json;
use toolkit_odata::Page;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use model_registry_sdk::AvailableModelV1;

use super::dto::{
    AvailableModelDto, CreateModelRequestDto, CreateProviderRequestDto, ModelEntryDto, PageInfoDto,
    ProviderEntryDto, UpdateModelRequestDto, UpdateProviderRequestDto,
};
use super::parse;
use crate::domain::service::Service;

type Svc = Extension<Arc<Service>>;

/// A page of DTOs with `$select` applied to every item.
fn projected<T: serde::Serialize, U: Into<T>>(
    page: Page<U>,
    query: &OData,
) -> Json<Page<serde_json::Value>> {
    let dto_page = Page {
        items: page.items.into_iter().map(Into::into).collect::<Vec<T>>(),
        page_info: page.page_info,
    };
    Json(page_to_projected_json(&dto_page, query.selected_fields()))
}

fn selected<T: serde::Serialize>(value: T, query: Option<&OData>) -> Json<serde_json::Value> {
    Json(apply_select(value, query.and_then(|q| q.selected_fields())))
}

// ==================== Resolution ====================

/// `GET /model-registry/v1/available-models/{canonical_id}`. The full
/// `ModelV1` is returned only through the SDK.
pub async fn resolve_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(canonical_id): Path<String>,
) -> ApiResult<JsonBody<AvailableModelDto>> {
    let model = svc.resolve_model(&ctx, &canonical_id).await?;
    Ok(Json(AvailableModelDto::from(AvailableModelV1::from(model))))
}

/// `GET /model-registry/v1/available-models`
pub async fn list_available_models(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    OData(query): OData,
) -> ApiResult<JsonBody<Page<serde_json::Value>>> {
    let page = svc.list_available_models(&ctx, &query).await?;
    Ok(projected::<AvailableModelDto, _>(page, &OData(query)))
}

// ==================== Providers ====================

/// `POST /model-registry/v1/providers`
pub async fn create_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    uri: Uri,
    Json(dto): Json<CreateProviderRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let req = parse::create_provider(dto)?;
    let entry = svc.create_provider(&ctx, req).await?;
    let id = entry.provider.id.to_string();
    Ok(created_json(ProviderEntryDto::from(entry), &uri, &id))
}

/// `GET /model-registry/v1/providers/{id}`
pub async fn get_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
) -> ApiResult<JsonBody<ProviderEntryDto>> {
    let entry = svc.get_provider(&ctx, id).await?;
    Ok(Json(ProviderEntryDto::from(entry)))
}

/// `GET /model-registry/v1/providers`
pub async fn list_providers(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    OData(query): OData,
) -> ApiResult<JsonBody<Page<serde_json::Value>>> {
    let page = svc.list_providers(&ctx, &query).await?;
    Ok(projected::<ProviderEntryDto, _>(page, &OData(query)))
}

/// `PATCH /model-registry/v1/providers/{id}`
pub async fn update_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<JsonBody<ProviderEntryDto>> {
    parse::reject_immutable(&body, parse::PROVIDER_IMMUTABLE_FIELDS)?;
    let dto: UpdateProviderRequestDto = parse::body(body)?;
    let entry = svc
        .update_provider(&ctx, id, parse::update_provider(dto)?)
        .await?;
    Ok(Json(ProviderEntryDto::from(entry)))
}

/// `DELETE /model-registry/v1/providers/{id}`
pub async fn delete_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_provider(&ctx, id).await?;
    Ok(no_content())
}

// ==================== Models ====================

/// `POST /model-registry/v1/models`
pub async fn create_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    uri: Uri,
    Json(dto): Json<CreateModelRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let req = parse::create_model(dto)?;
    let entry = svc.create_model(&ctx, req).await?;
    let id = entry.model.id.to_string();
    Ok(created_json(ModelEntryDto::from(entry), &uri, &id))
}

/// `GET /model-registry/v1/models/{id}`
pub async fn get_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
) -> ApiResult<JsonBody<ModelEntryDto>> {
    let entry = svc.get_model(&ctx, id).await?;
    Ok(Json(ModelEntryDto::from(entry)))
}

/// `GET /model-registry/v1/models`
pub async fn list_models(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    OData(query): OData,
) -> ApiResult<JsonBody<Page<serde_json::Value>>> {
    let page = svc.list_models(&ctx, &query).await?;
    Ok(projected::<ModelEntryDto, _>(page, &OData(query)))
}

/// `PATCH /model-registry/v1/models/{id}`
pub async fn update_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<JsonBody<ModelEntryDto>> {
    parse::reject_immutable(&body, parse::MODEL_IMMUTABLE_FIELDS)?;
    let dto: UpdateModelRequestDto = parse::body(body)?;
    let entry = svc
        .update_model(&ctx, id, parse::update_model(dto)?)
        .await?;
    Ok(Json(ModelEntryDto::from(entry)))
}

/// `DELETE /model-registry/v1/models/{id}`
pub async fn delete_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_model(&ctx, id).await?;
    Ok(no_content())
}

// Keep the page-info DTO reachable for the OpenAPI schema of the list routes.
#[allow(dead_code)]
fn _page_info(p: toolkit_odata::PageInfo) -> PageInfoDto {
    p.into()
}

#[allow(dead_code)]
fn _selected_example(v: serde_json::Value) -> Json<serde_json::Value> {
    selected(v, None)
}
