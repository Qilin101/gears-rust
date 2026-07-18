//! Application service for the Model Registry.
//!
//! Stub — implemented in Task 11 (provider operations) / Task 12-13 (model operations).

use async_trait::async_trait;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::{
    CreateModelRequestV1, CreateProviderRequestV1, ModelV1, ModelRegistryError, ProviderV1,
    UpdateModelRequestV1, UpdateProviderRequestV1,
};

/// Application service for the Model Registry.
///
/// Orchestrates provider/model CRUD with cache-first reads, tenant inheritance
/// resolution, authz checks, and approval writes.
///
/// Generic over `R` (repository), `C` (cache), `T` (tenant resolver), and
/// `E` (authz enforcer).
pub struct ModelRegistryService<R, C, T, E> {
    _repo: std::marker::PhantomData<(R, C, T, E)>,
}

#[async_trait]
pub trait ModelRegistryServiceOps: Send + Sync {
    // ── Providers ──
    async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderV1, ModelRegistryError>;

    async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: ODataQuery,
    ) -> Result<Page<ProviderV1>, ModelRegistryError>;

    async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError>;

    async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError>;

    async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), ModelRegistryError>;

    // ── Models ──
    async fn get_tenant_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, ModelRegistryError>;

    async fn list_tenant_models(
        &self,
        ctx: &SecurityContext,
        query: ODataQuery,
    ) -> Result<Page<ModelV1>, ModelRegistryError>;

    async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError>;

    async fn update_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
        req: UpdateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError>;

    async fn delete_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<(), ModelRegistryError>;
}
