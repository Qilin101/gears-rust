// Route-surface test for the Model Registry gear.
//
// Captures every `OperationSpec` that `register_routes` registers and asserts
// the exact (method, path) set. The identifier keying is a wire contract: a
// management path silently reverting to `{canonical_id}`, or an eval read
// drifting into the admin zone, is the regression this pins.

#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use model_registry::api::rest::routes::{ConcreteService, register_routes};
use model_registry::config::ModelRegistryConfig;
use model_registry::domain::cache::NoopResolutionCache;
use model_registry::infra::storage::model_repo::ModelRepositoryImpl;
use model_registry::infra::storage::provider_repo::ProviderRepositoryImpl;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationSpec;

// ── Registry that records what was registered ───────────────────────────

#[derive(Default)]
struct RecordingRegistry {
    ops: Mutex<Vec<(String, String, String)>>,
}

impl RecordingRegistry {
    /// `(METHOD, path, operation_id)` triples, sorted for a stable comparison.
    fn recorded(&self) -> Vec<(String, String, String)> {
        let mut v = self.ops.lock().expect("registry lock").clone();
        v.sort();
        v
    }
}

impl OpenApiRegistry for RecordingRegistry {
    fn register_operation(&self, spec: &OperationSpec) {
        self.ops.lock().expect("registry lock").push((
            spec.method.to_string(),
            spec.path.clone(),
            spec.operation_id.clone().unwrap_or_default(),
        ));
    }

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn service() -> Arc<ConcreteService> {
    let raw = toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default())
        .await
        .expect("in-memory SQLite");
    let db: toolkit_db::DBProvider<toolkit_db::DbError> = toolkit_db::DBProvider::new(raw);

    Arc::new(model_registry::domain::service::Service::new(
        Arc::new(db),
        Arc::new(ProviderRepositoryImpl::default()),
        Arc::new(ModelRepositoryImpl::default()),
        Arc::new(NoopResolutionCache),
        Arc::new(NoAncestors),
        authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(DenyAll)),
        ModelRegistryConfig::default(),
    ))
}

#[tokio::test]
async fn registered_routes_are_split_into_an_eval_and_an_admin_zone() {
    let registry = RecordingRegistry::default();
    let _router: Router = register_routes(Router::new(), &registry, service().await);

    // The eval zone is keyed by canonical_id — the only identifier an inference
    // caller holds. The admin zone is keyed by UUID throughout: no `{slug}` and
    // no `{canonical_id}` may appear under /admin.
    let expected: Vec<(String, String, String)> = [
        // ── eval zone ───────────────────────────────────────────────────
        (
            "GET",
            "/model-registry/v1/models",
            "model_registry.list_tenant_models",
        ),
        (
            "GET",
            "/model-registry/v1/models/{canonical_id}",
            "model_registry.get_tenant_model",
        ),
        // ── admin zone: models ──────────────────────────────────────────
        (
            "GET",
            "/model-registry/v1/admin/models",
            "model_registry.list_management_models",
        ),
        (
            "POST",
            "/model-registry/v1/admin/models",
            "model_registry.create_model",
        ),
        (
            "GET",
            "/model-registry/v1/admin/models/{id}",
            "model_registry.get_model",
        ),
        (
            "PATCH",
            "/model-registry/v1/admin/models/{id}",
            "model_registry.update_model",
        ),
        (
            "DELETE",
            "/model-registry/v1/admin/models/{id}",
            "model_registry.delete_model",
        ),
        // ── admin zone: providers ───────────────────────────────────────
        (
            "GET",
            "/model-registry/v1/admin/providers",
            "model_registry.list_providers",
        ),
        (
            "POST",
            "/model-registry/v1/admin/providers",
            "model_registry.create_provider",
        ),
        (
            "GET",
            "/model-registry/v1/admin/providers/{id}",
            "model_registry.get_provider",
        ),
        (
            "PATCH",
            "/model-registry/v1/admin/providers/{id}",
            "model_registry.update_provider",
        ),
        (
            "DELETE",
            "/model-registry/v1/admin/providers/{id}",
            "model_registry.delete_provider",
        ),
    ]
    .into_iter()
    .map(|(m, p, o)| (m.to_owned(), p.to_owned(), o.to_owned()))
    .collect();

    let mut expected_sorted = expected;
    expected_sorted.sort();

    assert_eq!(
        registry.recorded(),
        expected_sorted,
        "the registered (method, path, operation_id) set changed"
    );
}

/// Guard the invariant directly, independent of the exact endpoint list above:
/// nothing under `/admin` may be addressed by a slug-derived identifier.
#[tokio::test]
async fn no_admin_route_is_keyed_by_a_slug_or_canonical_id() {
    let registry = RecordingRegistry::default();
    let _router: Router = register_routes(Router::new(), &registry, service().await);

    for (method, path, op) in registry.recorded() {
        if !path.contains("/admin/") {
            continue;
        }
        assert!(
            !path.contains("{canonical_id}") && !path.contains("{slug}"),
            "{method} {path} ({op}) addresses a management resource by slug"
        );
    }
}

/// The eval read keeps its `canonical_id` key: an inference caller has no UUID,
/// so moving this to an id would break every consumer of the gateway path.
#[tokio::test]
async fn the_eval_read_stays_keyed_by_canonical_id() {
    let registry = RecordingRegistry::default();
    let _router: Router = register_routes(Router::new(), &registry, service().await);

    let eval_get = registry
        .recorded()
        .into_iter()
        .find(|(_, _, op)| op == "model_registry.get_tenant_model")
        .expect("the eval read is registered");
    assert_eq!(eval_get.1, "/model-registry/v1/models/{canonical_id}");
}

// ── Minimal stubs: the routes are registered, never invoked ─────────────

struct NoAncestors;

#[async_trait::async_trait]
impl tenant_resolver_sdk::TenantResolverClient for NoAncestors {
    async fn get_tenant(
        &self,
        _: &toolkit_security::SecurityContext,
        _: tenant_resolver_sdk::TenantId,
    ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError> {
        unimplemented!("routes are registered, not invoked")
    }
    async fn get_root_tenant(
        &self,
        _: &toolkit_security::SecurityContext,
    ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError> {
        unimplemented!("routes are registered, not invoked")
    }
    async fn get_tenants(
        &self,
        _: &toolkit_security::SecurityContext,
        _: &[tenant_resolver_sdk::TenantId],
        _: &tenant_resolver_sdk::GetTenantsOptions,
    ) -> Result<Vec<tenant_resolver_sdk::TenantInfo>, tenant_resolver_sdk::TenantResolverError>
    {
        unimplemented!("routes are registered, not invoked")
    }
    async fn get_ancestors(
        &self,
        _: &toolkit_security::SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::GetAncestorsOptions,
    ) -> Result<tenant_resolver_sdk::GetAncestorsResponse, tenant_resolver_sdk::TenantResolverError>
    {
        unimplemented!("routes are registered, not invoked")
    }
    async fn get_descendants(
        &self,
        _: &toolkit_security::SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::GetDescendantsOptions,
    ) -> Result<tenant_resolver_sdk::GetDescendantsResponse, tenant_resolver_sdk::TenantResolverError>
    {
        unimplemented!("routes are registered, not invoked")
    }
    async fn is_ancestor(
        &self,
        _: &toolkit_security::SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::IsAncestorOptions,
    ) -> Result<bool, tenant_resolver_sdk::TenantResolverError> {
        unimplemented!("routes are registered, not invoked")
    }
}

struct DenyAll;

#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverClient for DenyAll {
    async fn evaluate(
        &self,
        _: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, authz_resolver_sdk::AuthZResolverError>
    {
        unimplemented!("routes are registered, not invoked")
    }
}
