use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx};
use tracing::info;

#[toolkit::gear(
    name = "model-registry",
    deps = ["tenant-resolver", "authz-resolver"],
    capabilities = [rest, db]
)]
pub struct ModelRegistryGear {
    service: OnceLock<Arc<()>>,
}

impl Default for ModelRegistryGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl toolkit::contracts::DatabaseCapability for ModelRegistryGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        info!("Providing model-registry database migrations");
        Vec::new()
    }
}

#[async_trait]
impl Gear for ModelRegistryGear {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        info!("Initializing model-registry gear");

        self.service
            .set(Arc::new(()))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        Ok(())
    }
}

#[async_trait]
impl toolkit::contracts::RestApiCapability for ModelRegistryGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        info!("model-registry gear: register_rest called");
        let _service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        info!("model-registry gear: REST routes registered successfully");
        Ok(router)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_registry_gear_default() {
        let gear = ModelRegistryGear::default();
        assert!(gear.service.get().is_none());
    }

    #[test]
    fn test_model_registry_gear_module_name() {
        assert_eq!(ModelRegistryGear::MODULE_NAME, "model-registry");
    }
}
