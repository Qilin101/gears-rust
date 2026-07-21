use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx};
use toolkit_db::DBProvider;
use toolkit_db::DbError;
use tracing::info;

use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use tenant_resolver_sdk::TenantResolverClient;

use model_registry_sdk::ModelRegistryClientV1;

use crate::api::rest::routes;
use crate::config::ModelRegistryConfig;
use crate::domain::cache::InMemoryCache;
use crate::domain::local_client::LocalClient;
use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::SeaOrmRepository;

/// Concrete service type used by the gear.
type ConcreteService = Service<SeaOrmRepository, SeaOrmRepository, InMemoryCache>;

#[toolkit::gear(
    name = "model-registry",
    deps = ["tenant-resolver", "authz-resolver"],
    capabilities = [rest, db]
)]
pub struct ModelRegistryGear {
    service: OnceLock<Arc<ConcreteService>>,
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
        use sea_orm_migration::MigratorTrait;
        info!("Providing model-registry database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

#[async_trait]
impl Gear for ModelRegistryGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: ModelRegistryConfig = ctx.config_or_default()?;

        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        // Repository is stateless — uses &impl DBRunner per-method
        let provider_repo = Arc::new(SeaOrmRepository::new());
        let model_repo = Arc::new(SeaOrmRepository::new());

        // In-memory cache (Redis is a feature-gated follow-up)
        let cache = Arc::new(InMemoryCache::new());

        // Fetch TenantResolver from ClientHub
        let tenant_resolver = ctx
            .client_hub()
            .get::<dyn TenantResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get TenantResolverClient: {e}"))?;

        // Fetch AuthZ resolver from ClientHub
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;
        let policy_enforcer = PolicyEnforcer::new(authz);

        let service = Arc::new(Service::new(
            db,
            provider_repo,
            model_repo,
            cache,
            tenant_resolver,
            policy_enforcer,
            cfg,
        ));

        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register LocalClient as ModelRegistryClientV1 in ClientHub
        let local_client: Arc<dyn ModelRegistryClientV1> = Arc::new(LocalClient::new(service));
        ctx.client_hub().register(local_client);

        info!("model-registry gear initialized successfully");
        Ok(())
    }
}

#[async_trait]
impl toolkit::contracts::RestApiCapability for ModelRegistryGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        info!("model-registry gear: register_rest called");
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        let router = routes::register_routes(router, openapi, service);
        info!("model-registry gear: REST routes registered successfully");
        Ok(router)
    }
}

