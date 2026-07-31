//! End-to-end integration tests for the Model Registry gear.
//!
//! These tests construct a full service stack (real `ProviderRepositoryImpl` /
//! `ModelRepositoryImpl` over in-memory `SQLite`, real `InMemoryCache`, real
//! `PolicyEnforcer` backed by a
//! mock `AuthZResolverClient`, and configurable mock `TenantResolverClient`)
//! and drive the complete provider→model→approval→soft-delete lifecycle.
//!
//! ## Test matrix
//!
//! | Test | Flow | What it verifies |
//! |------|------|------------------|
//! | `full_lifecycle_single_tenant` | create provider → create model → get → list with `OData` → update approval → soft-delete | P1 happy path end-to-end |
//! | `tenant_isolation` | two tenants, data created in A only | B cannot see A's data |
//! | `inheritance_parent_child` | parent owns provider + model; child has no data | child inherits via ancestor chain |
//! | `child_shadows_parent` | parent + child own same `canonical_id` | child shadows parent |
//! | `cache_first_get` | second read hits cache | cache-first read behaviour |
//!
//! All tests run against an in-memory `SQLite` database with the production
//! migration applied, giving high confidence the storage layer works
//! end-to-end without needing Postgres.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
};
use model_registry::config::ModelRegistryConfig;
use model_registry::domain::cache::InMemoryCache;
use model_registry::domain::error::DomainError;
use model_registry::domain::repo::{ModelRepository, ProviderRepository};
use model_registry::domain::service::Service;
use model_registry::infra::storage::migrations::Migrator;
use model_registry::infra::storage::model_repo::ModelRepositoryImpl;
use model_registry::infra::storage::provider_repo::ProviderRepositoryImpl;
use model_registry::{
    ApprovalStatus, CreateModelRequestV1, CreateProviderRequestV1, LifecycleStatus, ModelV1,
    UpdateModelRequestV1, UpdateProviderRequestV1,
};
use model_registry_sdk::models::{
    ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities, DisabledMediaCapability,
    DisabledReasoningCapability, DisabledWebSearchCapability, MediaCapability, ModelCapabilities,
    ModelInfoV1 as SdkModelInfoV1, ModelPerformance, ReasoningCapability, SupportedApi,
    WebSearchCapability,
};
use sea_orm_migration::MigratorTrait;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef, TenantResolverClient,
    TenantResolverError, TenantStatus,
};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::DBRunner;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_odata::{ODataQuery, parse_filter_string};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

// ═══════════════════════════════════════════════════════════════════════════════
// Test tenant IDs
// ═══════════════════════════════════════════════════════════════════════════════

fn tenant_a() -> Uuid {
    Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap()
}

fn tenant_b() -> Uuid {
    Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap()
}

fn child_tenant() -> Uuid {
    Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap()
}

fn parent_tenant() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Mock AuthZResolverClient — always permissive with Eq constraint
// ═══════════════════════════════════════════════════════════════════════════════

struct MockAuthZ;

#[async_trait]
impl AuthZResolverClient for MockAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let tenant_id = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::nil);

        Ok(EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::EvaluationResponseContext {
                constraints: vec![authz_resolver_sdk::constraints::Constraint {
                    predicates: vec![authz_resolver_sdk::constraints::Predicate::Eq(
                        authz_resolver_sdk::constraints::EqPredicate {
                            property: "owner_tenant_id".to_owned(),
                            value: serde_json::json!(tenant_id.to_string()),
                        },
                    )],
                }],
                deny_reason: None,
            },
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Mock TenantResolverClient variants
// ═══════════════════════════════════════════════════════════════════════════════

/// Returns no ancestors (single-tenant scenario).
struct NoAncestorsResolver;

#[async_trait]
impl TenantResolverClient for NoAncestorsResolver {
    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        Ok(GetAncestorsResponse {
            tenant: TenantRef {
                id,
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            },
            ancestors: vec![],
        })
    }

    async fn get_tenant(
        &self,
        _: &SecurityContext,
        _: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        unimplemented!("not used in tests")
    }
    async fn get_root_tenant(
        &self,
        _: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        unimplemented!("not used in tests")
    }
    async fn get_tenants(
        &self,
        _: &SecurityContext,
        _: &[TenantId],
        _: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        unimplemented!("not used in tests")
    }
    async fn get_descendants(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unimplemented!("not used in tests")
    }
    async fn is_ancestor(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: TenantId,
        _: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        unimplemented!("not used in tests")
    }
}

/// Returns a fixed parent-child ancestor chain: child → parent.
struct OneAncestorResolver;

#[async_trait]
impl TenantResolverClient for OneAncestorResolver {
    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        Ok(GetAncestorsResponse {
            tenant: TenantRef {
                id,
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: Some(TenantId(parent_tenant())),
                self_managed: false,
            },
            ancestors: vec![TenantRef {
                id: TenantId(parent_tenant()),
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            }],
        })
    }

    async fn get_tenant(
        &self,
        _: &SecurityContext,
        _: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        unimplemented!("not used in tests")
    }
    async fn get_root_tenant(
        &self,
        _: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        unimplemented!("not used in tests")
    }
    async fn get_tenants(
        &self,
        _: &SecurityContext,
        _: &[TenantId],
        _: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        unimplemented!("not used in tests")
    }
    async fn get_descendants(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unimplemented!("not used in tests")
    }
    async fn is_ancestor(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: TenantId,
        _: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        unimplemented!("not used in tests")
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// Set up an in-memory `SQLite` database with all migrations applied.
async fn setup_db() -> DBProvider<DbError> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("in-memory SQLite connection");

    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("apply initial migration");

    DBProvider::<DbError>::new(db)
}

fn scope_for(tenant_id: Uuid) -> AccessScope {
    AccessScope::for_tenants(vec![tenant_id])
}

fn security_context(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("SecurityContext")
}

fn make_provider_gts() -> gts::GtsTypeId {
    gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~")
}

fn make_create_provider_req(slug: &str, name: &str) -> CreateProviderRequestV1 {
    CreateProviderRequestV1::builder(slug, name, make_provider_gts()).build()
}

fn make_create_model_req(provider_slug: &str, provider_model_id: &str) -> CreateModelRequestV1 {
    let gts_leaf = "cf.genai._.openai.v1~";
    let gts_type = format!("gts.cf.genai.model.info.v1~{gts_leaf}");

    let info = SdkModelInfoV1 {
        gts_type: gts::GtsTypeId::new(&gts_type),
        display_name: format!("Test {provider_model_id}"),
        description: None,
        family: Some("test-family".to_owned()),
        vendor: Some("TestVendor".to_owned()),
        managed: false,
        architecture: Some("transformer".to_owned()),
        size_bytes: None,
        format: Some("api-only".to_owned()),
        region: None,
        hosted_by: None,
        last_release_at: None,
        reasoning_level: None,
        version: None,
        sort_order: None,
        icon: None,
        multiplier_display: None,
        performance: ModelPerformance {
            response_latency_ms: None,
            tokens_per_second: None,
        },
        additional_info: HashMap::new(),
        supported_api: HashSet::from([SupportedApi::Completion]),
        provider_model_id: provider_model_id.to_owned(),
        capabilities: ModelCapabilities {
            vision: MediaCapability {
                enabled: true,
                supported_mime_types: vec!["image/jpeg".to_owned()],
            },
            reasoning: ReasoningCapability {
                effort: false,
                toggle: false,
                resume: false,
                budget: false,
            },
            function_calling: true,
            response_schema: false,
            streaming: true,
            file_input: MediaCapability::default(),
            image_generation: MediaCapability::default(),
            audio_input: MediaCapability::default(),
            audio_output: MediaCapability::default(),
            code_interpreter: false,
            web_search: WebSearchCapability {
                enabled: false,
                allowed_domains: false,
                excluded_domains: false,
            },
        },
        disabled_capabilities: DisabledCapabilities {
            vision: DisabledMediaCapability::default(),
            reasoning: DisabledReasoningCapability::default(),
            function_calling: false,
            response_schema: false,
            streaming: false,
            file_input: DisabledMediaCapability::default(),
            image_generation: DisabledMediaCapability::default(),
            audio_input: DisabledMediaCapability::default(),
            audio_output: DisabledMediaCapability::default(),
            code_interpreter: false,
            web_search: DisabledWebSearchCapability::default(),
        },
        context_window: ContextWindow {
            max_input_tokens: 8192,
            max_output_tokens: Some(4096),
            output_vector_size: None,
        },
        default_parameters: DefaultInferenceParametersV1::default(),
        allow_parameter_override: false,
        allow_extra_params: Vec::new(),
        provider_settings: serde_json::Value::Object(serde_json::Map::new()),
    };

    CreateModelRequestV1 {
        provider_slug: provider_slug.to_owned(),
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    }
}

/// Build a full `Service` instance for integration testing.
fn build_service<R: TenantResolverClient + Send + Sync + 'static>(
    db: DBProvider<DbError>,
    tenant_resolver: R,
) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl, InMemoryCache> {
    let enforcer = authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(MockAuthZ));
    Service::new(
        Arc::new(db),
        Arc::new(ProviderRepositoryImpl::new()),
        Arc::new(ModelRepositoryImpl::new()),
        Arc::new(InMemoryCache::new()),
        Arc::new(tenant_resolver),
        enforcer,
        ModelRegistryConfig::default(),
    )
}

/// Create a provider in the given tenant via the repository directly (bypassing
/// the service/authz layer for test setup purposes).
async fn create_provider_direct(
    repo: &ProviderRepositoryImpl,
    conn: &impl DBRunner,
    tenant_id: Uuid,
    slug: &str,
) -> (Uuid, String) {
    let scope = scope_for(tenant_id);
    let p = ProviderRepository::create(
        repo,
        conn,
        &scope,
        tenant_id,
        &make_create_provider_req(slug, slug),
    )
    .await
    .expect("create provider in test setup");
    (p.id, p.slug)
}

/// Create a model in the given tenant via the repository directly.
async fn create_model_direct(
    repo: &ModelRepositoryImpl,
    conn: &impl DBRunner,
    tenant_id: Uuid,
    provider_slug: &str,
    provider_model_id: &str,
) -> ModelV1 {
    let scope = scope_for(tenant_id);
    ModelRepository::create(
        repo,
        conn,
        &scope,
        tenant_id,
        &make_create_model_req(provider_slug, provider_model_id),
    )
    .await
    .expect("create model in test setup")
}

// ═══════════════════════════════════════════════════════════════════════════════
// 1. Full lifecycle: provider → model → get → list (OData) → approval → delete
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn full_lifecycle_single_tenant() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let tenant_id = tenant_a();
    let ctx = security_context(tenant_id);

    // ── Step 1: Create a provider ───────────────────────────────────────────
    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");
    assert_eq!(provider.slug, "openai");
    assert_eq!(provider.name, "OpenAI");

    // ── Step 2: Create a model ──────────────────────────────────────────────
    let model = service
        .create_model(&ctx, &make_create_model_req("openai", "gpt-4o"))
        .await
        .expect("create model");
    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.lifecycle_status, LifecycleStatus::Production);
    assert_eq!(model.approval_status, ApprovalStatus::Pending);

    // ── Step 3: get_tenant_model (cache miss → DB populate) ─────────────────
    let fetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("get model");
    assert_eq!(fetched.canonical_id, "openai::gpt-4o");
    assert_eq!(fetched.approval_status, ApprovalStatus::Pending);

    // ── Step 4: get_tenant_model again (cache hit) ──────────────────────────
    let cached = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("get model from cache");
    assert_eq!(cached.canonical_id, "openai::gpt-4o");

    // ── Step 5: List with OData filter ──────────────────────────────────────
    let parsed = parse_filter_string("approval_status eq 'pending'").expect("parse filter");
    let query = ODataQuery {
        filter: Some(Box::new(parsed.into_expr())),
        ..Default::default()
    };
    let page = service
        .list_tenant_models(&ctx, query)
        .await
        .expect("list models with OData filter");
    assert_eq!(page.items.len(), 1, "one pending model should be listed");
    assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");

    // ── Step 6: Update approval to Approved ─────────────────────────────────
    let approved = service
        .update_model(
            &ctx,
            "openai::gpt-4o",
            &UpdateModelRequestV1 {
                approval_status: Some(ApprovalStatus::Approved),
                ..Default::default()
            },
        )
        .await
        .expect("approve model");
    assert_eq!(approved.approval_status, ApprovalStatus::Approved);

    // ── Step 7: Verify the approval persisted by reading back ───────────────
    let refetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("get approved model");
    // After cache invalidation from update, the read comes from DB.
    assert_eq!(refetched.approval_status, ApprovalStatus::Approved);

    // ── Step 8: Soft-delete the model ───────────────────────────────────────
    service
        .delete_model(&ctx, "openai::gpt-4o")
        .await
        .expect("soft-delete model");

    // ── Step 9: Verify deprecated model returns ModelDeprecated on direct get
    let deprecated_err = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect_err("deprecated model should error");
    assert!(
        matches!(deprecated_err, DomainError::ModelDeprecated { .. }),
        "expected ModelDeprecated, got {deprecated_err:?}"
    );

    // ── Step 10: Verify deprecated model is hidden from default list ────────
    let list_after = service
        .list_tenant_models(&ctx, ODataQuery::default())
        .await
        .expect("list models after soft-delete");
    assert!(
        list_after.items.is_empty(),
        "deprecated model should be hidden from list"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. Tenant isolation: data from tenant A is invisible to tenant B
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn tenant_isolation() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx_a = security_context(tenant_a());
    let ctx_b = security_context(tenant_b());

    // Create provider and model in tenant A.
    let provider = service
        .create_provider(&ctx_a, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider in tenant A");
    let _model = service
        .create_model(&ctx_a, &make_create_model_req("openai", "gpt-4o"))
        .await
        .expect("create model in tenant A");

    // Tenant B should not see any providers.
    let providers_page = service
        .list_providers(&ctx_b, ODataQuery::default())
        .await
        .expect("list providers as tenant B");
    assert!(
        providers_page.items.is_empty(),
        "tenant B should see no providers"
    );

    // Tenant B should not see any models.
    let models_page = service
        .list_tenant_models(&ctx_b, ODataQuery::default())
        .await
        .expect("list models as tenant B");
    assert!(
        models_page.items.is_empty(),
        "tenant B should see no models"
    );

    // Tenant B should not be able to get the model.
    let get_err = service
        .get_tenant_model(&ctx_b, "openai::gpt-4o")
        .await
        .expect_err("tenant B get model should fail");
    assert!(
        matches!(get_err, DomainError::ModelNotFound { .. }),
        "expected ModelNotFound, got {get_err:?}"
    );

    // Tenant B should not be able to get the provider.
    let get_provider_err = service
        .get_provider(&ctx_b, provider.id)
        .await
        .expect_err("tenant B get provider should fail");
    assert!(
        matches!(get_provider_err, DomainError::ProviderNotFound { .. }),
        "expected ProviderNotFound, got {get_provider_err:?}"
    );

    // Tenant B should not be able to update the model.
    let update_err = service
        .update_model(
            &ctx_b,
            "openai::gpt-4o",
            &UpdateModelRequestV1 {
                approval_status: Some(ApprovalStatus::Approved),
                ..Default::default()
            },
        )
        .await
        .expect_err("tenant B update model should fail");
    assert!(
        matches!(update_err, DomainError::ModelNotFound { .. }),
        "expected ModelNotFound, got {update_err:?}"
    );

    // Tenant B should not be able to delete the model.
    let delete_err = service
        .delete_model(&ctx_b, "openai::gpt-4o")
        .await
        .expect_err("tenant B delete model should fail");
    assert!(
        matches!(delete_err, DomainError::ModelNotFound { .. }),
        "expected ModelNotFound, got {delete_err:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. Inheritance: child tenant inherits provider + model from parent
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn child_inherits_provider_and_model_from_parent() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::new();
    let model_repo = ModelRepositoryImpl::new();

    // Create provider and model in the parent tenant (via direct repo calls
    // to isolate from service-layer cache interactions).
    let (provider_id, provider_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    create_model_direct(
        &model_repo,
        &conn,
        parent_tenant(),
        &provider_slug,
        "gpt-4o",
    )
    .await;

    // Child tenant uses the service with the ancestor-chain resolver.
    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    // ── Child can list the inherited model ──────────────────────────────────
    let models_page = service
        .list_tenant_models(&ctx, ODataQuery::default())
        .await
        .expect("child list inherited models");
    assert_eq!(
        models_page.items.len(),
        1,
        "child should inherit exactly 1 model"
    );
    assert_eq!(
        models_page.items[0].canonical_id, "openai::gpt-4o",
        "inherited model canonical_id must match"
    );

    // ── Child can get the inherited model directly ──────────────────────────
    let inherited = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("child get inherited model");
    assert_eq!(inherited.canonical_id, "openai::gpt-4o");
    assert_eq!(
        inherited.approval_status,
        ApprovalStatus::Pending,
        "inherited model should have populated approval_status"
    );

    // ── Child can list inherited providers ──────────────────────────────────
    let providers_page = service
        .list_providers(&ctx, ODataQuery::default())
        .await
        .expect("child list inherited providers");
    assert_eq!(
        providers_page.items.len(),
        1,
        "child should inherit exactly 1 provider"
    );
    assert_eq!(
        providers_page.items[0].slug, "openai",
        "inherited provider slug must match"
    );

    // ── Child can get the inherited provider directly ───────────────────────
    let inherited_provider = service
        .get_provider(&ctx, provider_id)
        .await
        .expect("child get inherited provider");
    assert_eq!(inherited_provider.slug, "openai");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. Child shadows parent: child creates model with same canonical_id as parent
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn child_shadows_parent_by_same_canonical_id() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::new();
    let model_repo = ModelRepositoryImpl::new();

    // Create provider and model in the parent tenant.
    let (_parent_provider_id, parent_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    create_model_direct(&model_repo, &conn, parent_tenant(), &parent_slug, "gpt-4o").await;

    // Create the SAME provider slug AND model canonical_id in the child tenant.
    let (_child_provider_id, child_slug) =
        create_provider_direct(&provider_repo, &conn, child_tenant(), "openai").await;
    create_model_direct(&model_repo, &conn, child_tenant(), &child_slug, "gpt-4o").await;

    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    // Child's list should show exactly 1 model (child shadows parent).
    let page = service
        .list_tenant_models(&ctx, ODataQuery::default())
        .await
        .expect("child list models");
    assert_eq!(
        page.items.len(),
        1,
        "shadowing should produce exactly 1 model"
    );
    assert_eq!(
        page.items[0].canonical_id, "openai::gpt-4o",
        "model canonical_id should match"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 5. Cache-first read: second get_tenant_model hits cache
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn cache_first_get_returns_cached_model() {
    // Clone the DBProvider so we can get a raw connection for direct repo
    // mutations after the service is built (Arc<Db> behind the scenes).
    let db = setup_db().await;
    let db2 = db.clone();
    let provider_repo = ProviderRepositoryImpl::new();
    let model_repo = ModelRepositoryImpl::new();

    // Create data directly via repo connection.
    {
        let conn = db2.conn().expect("db connection");
        let (_pid, slug) =
            create_provider_direct(&provider_repo, &conn, tenant_a(), "openai").await;
        let _original = create_model_direct(&model_repo, &conn, tenant_a(), &slug, "gpt-4o").await;
    }

    // Build service (consumes original db, stored in Arc inside).
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    // First read: cache miss, populates cache.
    let first = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("first get (cache miss)");
    assert_eq!(first.canonical_id, "openai::gpt-4o");
    assert_eq!(first.lifecycle_status, LifecycleStatus::Production);

    // Update the model via repo directly to bypass cache invalidation.
    // We use the cloned DBProvider to get a fresh connection.
    let conn2 = db2.conn().expect("db connection");
    ModelRepository::update(
        &model_repo,
        &conn2,
        &scope_for(tenant_a()),
        "openai::gpt-4o",
        &UpdateModelRequestV1 {
            lifecycle_status: Some(LifecycleStatus::Preview),
            ..Default::default()
        },
    )
    .await
    .expect("direct update bypassing cache invalidation");

    // Second read: cache hit, returns stale data (not the updated Preview).
    let second = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("second get (cache hit)");
    assert_eq!(
        second.lifecycle_status,
        LifecycleStatus::Production,
        "cache hit should return stale (Production) value, not updated Preview"
    );
    assert_eq!(
        second.canonical_id, "openai::gpt-4o",
        "canonical_id should still match"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 6. Full OData filtering on models
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn odata_filters_work_on_filterable_columns() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let tenant_id = tenant_a();
    let ctx = security_context(tenant_id);

    // Create provider.
    service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    // Create two models with different providers but same vendor/family.
    let _model1 = service
        .create_model(&ctx, &make_create_model_req("openai", "gpt-4o"))
        .await
        .expect("create model gpt-4o");

    service
        .create_provider(&ctx, &make_create_provider_req("anthropic", "Anthropic"))
        .await
        .expect("create provider anthropic");

    // Create a second provider with different slug for second model.
    // (The model's provider settings reference the provider slug;
    //  the `gts_type` in the info drives the `gts_type` column.)
    let mut model2_req = make_create_model_req("openai", "gpt-4o-mini");
    model2_req.info = {
        let mut info_val = serde_json::to_value(&model2_req.info).expect("serialize info");
        if let Some(obj) = info_val.as_object_mut() {
            obj.insert("provider_model_id".into(), serde_json::json!("gpt-4o-mini"));
        }
        serde_json::from_value(info_val).expect("deserialize modified info")
    };
    let _model2 = service
        .create_model(&ctx, &model2_req)
        .await
        .expect("create model gpt-4o-mini");

    // Approve model1.
    service
        .update_model(
            &ctx,
            "openai::gpt-4o",
            &UpdateModelRequestV1 {
                approval_status: Some(ApprovalStatus::Approved),
                ..Default::default()
            },
        )
        .await
        .expect("approve gpt-4o");

    // ── Filter by approval_status eq 'approved' ─────────────────────────────
    let parsed = parse_filter_string("approval_status eq 'approved'").expect("parse");
    let page = service
        .list_tenant_models(
            &ctx,
            ODataQuery {
                filter: Some(Box::new(parsed.into_expr())),
                ..Default::default()
            },
        )
        .await
        .expect("list filtered by approval_status");
    assert_eq!(page.items.len(), 1, "only gpt-4o is approved");
    assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");

    // ── Filter by vision eq true ────────────────────────────────────────────
    let parsed = parse_filter_string("vision eq true").expect("parse");
    let page = service
        .list_tenant_models(
            &ctx,
            ODataQuery {
                filter: Some(Box::new(parsed.into_expr())),
                ..Default::default()
            },
        )
        .await
        .expect("list filtered by vision capability");
    // Both models have vision=true in the test data.
    assert_eq!(page.items.len(), 2, "both models have vision");

    // ── Filter by function_calling eq true ──────────────────────────────────
    let parsed = parse_filter_string("function_calling eq true").expect("parse");
    let page = service
        .list_tenant_models(
            &ctx,
            ODataQuery {
                filter: Some(Box::new(parsed.into_expr())),
                ..Default::default()
            },
        )
        .await
        .expect("list filtered by function_calling");
    assert_eq!(page.items.len(), 2, "both models have function_calling");

    // ── Filter by streaming eq true ─────────────────────────────────────────
    let parsed = parse_filter_string("streaming eq true").expect("parse");
    let page = service
        .list_tenant_models(
            &ctx,
            ODataQuery {
                filter: Some(Box::new(parsed.into_expr())),
                ..Default::default()
            },
        )
        .await
        .expect("list filtered by streaming");
    assert_eq!(page.items.len(), 2, "both models have streaming");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 7. Providers CRUD via service layer
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn provider_crud_through_service() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let tenant_id = tenant_a();
    let ctx = security_context(tenant_id);

    // ── Create ──────────────────────────────────────────────────────────────
    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");
    let provider_id = provider.id;

    // ── Get ─────────────────────────────────────────────────────────────────
    let fetched = service
        .get_provider(&ctx, provider_id)
        .await
        .expect("get provider");
    assert_eq!(fetched.id, provider_id);
    assert_eq!(fetched.slug, "openai");

    // ── List ────────────────────────────────────────────────────────────────
    let page = service
        .list_providers(&ctx, ODataQuery::default())
        .await
        .expect("list providers");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].slug, "openai");

    // ── Update ──────────────────────────────────────────────────────────────
    let updated = service
        .update_provider(
            &ctx,
            provider_id,
            &UpdateProviderRequestV1 {
                name: Some("OpenAI Updated".into()),
                managed: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("update provider");
    assert_eq!(updated.name, "OpenAI Updated");
    assert!(updated.managed);
    assert_eq!(updated.slug, "openai", "slug should be immutable");

    // ── Delete ──────────────────────────────────────────────────────────────
    service
        .delete_provider(&ctx, provider_id)
        .await
        .expect("delete provider");

    let get_after_delete = service
        .get_provider(&ctx, provider_id)
        .await
        .expect_err("get deleted provider should fail");
    assert!(
        matches!(get_after_delete, DomainError::ProviderNotFound { .. }),
        "expected ProviderNotFound, got {get_after_delete:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 8. Storage layout: verify the column layout round-trips through the full
//    create → read → patch → re-read pipeline.
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify the create path produces a model whose every field round-trips
/// through the read path. Every `ModelInfoV1` field is reconstructed from the
/// promoted scalar columns + the JSONB sub-objects (`capabilities_full`,
/// `default_parameters`, `additional_info`, `disabled_capabilities_full`,
/// `allow_extra_params`) + the polymorphic `provider_settings`. The test covers
/// the full create → read round-trip end-to-end through the service layer.
#[tokio::test]
async fn create_and_read_round_trip_full_model_info() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let mut req = make_create_model_req("openai", "gpt-4o");
    // Populate the fields that ride inside the JSONB sub-objects (rather than
    // in a promoted scalar column) so the whole-payload assertion at the end of
    // this test covers them too.
    req.info.capabilities.response_schema = true;
    req.info.capabilities.file_input = MediaCapability {
        enabled: true,
        supported_mime_types: vec!["application/pdf".to_owned()],
    };
    req.info.capabilities.web_search = WebSearchCapability {
        enabled: true,
        allowed_domains: false,
        excluded_domains: true,
    };
    req.info.disabled_capabilities.vision = DisabledMediaCapability {
        disabled: false,
        disabled_mime_types: vec!["image/gif".to_owned()],
    };
    req.info.default_parameters.temperature = Some(0.5);
    let info_in = req.info.clone();
    service
        .create_model(&ctx, &req)
        .await
        .expect("create model");

    let fetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("get model");

    // ── Identity fields survive the round-trip ─────────────────────────────
    assert_eq!(fetched.info.gts_type, info_in.gts_type);
    assert_eq!(fetched.info.display_name, info_in.display_name);
    assert_eq!(fetched.info.provider_model_id, info_in.provider_model_id);
    assert_eq!(fetched.info.vendor, info_in.vendor);
    assert_eq!(fetched.info.family, info_in.family);
    assert_eq!(fetched.info.architecture, info_in.architecture);
    assert_eq!(fetched.info.format, info_in.format);
    assert_eq!(fetched.info.managed, info_in.managed);
    assert_eq!(fetched.info.region, info_in.region);
    assert_eq!(fetched.info.hosted_by, info_in.hosted_by);
    assert_eq!(fetched.info.reasoning_level, info_in.reasoning_level);
    assert_eq!(fetched.info.version, info_in.version);
    assert_eq!(fetched.info.sort_order, info_in.sort_order);
    assert_eq!(fetched.info.multiplier_display, info_in.multiplier_display);

    // ── Capability scalar columns (the 4 OData booleans) round-trip ────────
    assert_eq!(
        fetched.info.capabilities.vision.enabled, info_in.capabilities.vision.enabled,
        "vision.enabled round-trips through cap_vision scalar column"
    );
    assert_eq!(
        fetched.info.capabilities.function_calling, info_in.capabilities.function_calling,
        "function_calling round-trips through cap_function_calling"
    );
    assert_eq!(
        fetched.info.capabilities.streaming, info_in.capabilities.streaming,
        "streaming round-trips through cap_streaming"
    );
    assert_eq!(
        fetched.info.capabilities.reasoning.effort, info_in.capabilities.reasoning.effort,
        "reasoning.effort round-trips through cap_reasoning_effort"
    );

    // ── Context window round-trips through scalar columns ──────────────────
    assert_eq!(
        fetched.info.context_window.max_input_tokens,
        info_in.context_window.max_input_tokens,
    );
    assert_eq!(
        fetched.info.context_window.max_output_tokens,
        info_in.context_window.max_output_tokens,
    );

    // ── Allow-override bool round-trips ────────────────────────────────────
    assert_eq!(
        fetched.info.allow_parameter_override,
        info_in.allow_parameter_override,
    );

    // ── The whole payload round-trips verbatim ─────────────────────────────
    // Field-for-field equality of the SDK type in and out: the column layout
    // is lossless, including the capability fields that have no scalar column
    // and ride inside `capabilities_full` / `disabled_capabilities_full`.
    assert_eq!(fetched.info, info_in);
}

/// Verify that non-empty `additional_info` and `allow_extra_params` (the
/// forward-compat JSONB sub-objects) survive the full `SeaORM` `create` -> `DB` -> `read`
/// pipeline. The default fixture uses empty values; this test seeds non-empty
/// payloads to guard against regressions where empty-default handling would
/// accidentally coerce non-empty values to empty.
#[tokio::test]
async fn additional_info_and_allow_extra_params_round_trip_non_empty() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let mut req = make_create_model_req("openai", "gpt-4o");
    req.info.additional_info = serde_json::from_value(serde_json::json!({
        "team": "alpha",
        "trace_id": true,
        "experiment_id": 42,
    }))
    .expect("additional_info from JSON");
    req.info.allow_extra_params = vec!["trace_id".to_owned(), "session_id".to_owned()];

    service
        .create_model(&ctx, &req)
        .await
        .expect("create model with non-empty sub-objects");

    let fetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("get model");

    // `additional_info` JSONB sub-object round-trip preserves every key
    // (string, bool, integer values).
    assert_eq!(
        fetched.info.additional_info.get("team"),
        Some(&serde_json::json!("alpha")),
        "additional_info[string] round-trips"
    );
    assert_eq!(
        fetched.info.additional_info.get("trace_id"),
        Some(&serde_json::json!(true)),
        "additional_info[bool] round-trips"
    );
    assert_eq!(
        fetched.info.additional_info.get("experiment_id"),
        Some(&serde_json::json!(42)),
        "additional_info[int] round-trips"
    );
    assert_eq!(
        fetched.info.additional_info.len(),
        3,
        "additional_info key count must be preserved"
    );

    // `allow_extra_params` JSONB sub-object round-trip preserves the order.
    assert_eq!(
        fetched.info.allow_extra_params,
        vec!["trace_id".to_owned(), "session_id".to_owned()],
        "allow_extra_params list must round-trip with original order"
    );
}

/// Regression test: `size_bytes` was originally an `INTEGER` column on
/// `PostgreSQL` (max 2,147,483,647 ≈ 2 GiB). Any real model — even a 7B in fp16
/// (≈14 GB) — overflows that. The column is now `BIGINT`, so a realistic
/// model size must round-trip without truncation or overflow.
#[tokio::test]
async fn size_bytes_over_2gib_round_trips() {
    // 70B-parameter model in fp16 ≈ 140 GB.
    const LARGE_MODEL_BYTES: u64 = 140 * 1024 * 1024 * 1024;

    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let mut req = make_create_model_req("openai", "gpt-4o");
    req.info.size_bytes = Some(LARGE_MODEL_BYTES);

    service
        .create_model(&ctx, &req)
        .await
        .expect("create model with 140 GB size_bytes");

    let fetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("get model");

    assert_eq!(
        fetched.info.size_bytes,
        Some(LARGE_MODEL_BYTES),
        "size_bytes > i32::MAX must round-trip exactly through BIGINT column"
    );
}

/// Regression test: `context_window.max_input_tokens` was originally cast via
/// `i32::try_from(...).unwrap_or(i32::MAX)`, silently clamping any value
/// ≥ 2,147,483,648 to `i32::MAX`. The mapper now uses `i64::try_from(...).ok()`
/// so values up to `i64::MAX` survive the round-trip.
#[tokio::test]
async fn context_window_max_input_tokens_over_2gib_round_trips() {
    // 4B tokens > i32::MAX.
    const LARGE_CTX: u32 = 4_000_000_000;

    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let mut req = make_create_model_req("openai", "gpt-4o");
    req.info.context_window.max_input_tokens = LARGE_CTX;

    service
        .create_model(&ctx, &req)
        .await
        .expect("create model with 4B-token context window");

    let fetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("get model");

    assert_eq!(
        fetched.info.context_window.max_input_tokens, LARGE_CTX,
        "max_input_tokens > i32::MAX must round-trip exactly through i64 column"
    );
}

/// Verify PATCH on a single `info.*` field re-projects every promoted column
/// correctly: the changed field is updated on read; unrelated columns are
/// preserved. This exercises `model_update_active_model`, which re-projects
/// every scalar and JSONB column on every PATCH that touches `info.*`.
#[tokio::test]
async fn patch_reprojects_all_promoted_columns() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    service
        .create_model(&ctx, &make_create_model_req("openai", "gpt-4o"))
        .await
        .expect("create model");

    // PATCH: bump display_name and context_window.max_input_tokens. The PATCH
    // must re-project every promoted column, not just the changed ones.
    let patched = service
        .update_model(
            &ctx,
            "openai::gpt-4o",
            &UpdateModelRequestV1 {
                display_name: Some("GPT-4o (Updated)".to_owned()),
                context_window: Some(model_registry_sdk::models::ContextWindow {
                    max_input_tokens: 200_000,
                    max_output_tokens: Some(32_768),
                    output_vector_size: None,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("patch model");

    assert_eq!(patched.info.display_name, "GPT-4o (Updated)");
    assert_eq!(patched.info.context_window.max_input_tokens, 200_000);
    assert_eq!(patched.info.context_window.max_output_tokens, Some(32_768),);

    // Read back from DB (bypass cache) to confirm the columns were written.
    let refetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("refetch after patch");
    assert_eq!(refetched.info.display_name, "GPT-4o (Updated)");
    assert_eq!(refetched.info.context_window.max_input_tokens, 200_000);
    assert_eq!(
        refetched.info.context_window.max_output_tokens,
        Some(32_768),
    );

    // Unrelated fields preserved through the re-projection.
    assert_eq!(refetched.info.vendor.as_deref(), Some("TestVendor"),);
    assert_eq!(refetched.info.family.as_deref(), Some("test-family"));
    assert!(
        refetched.info.capabilities.function_calling,
        "function_calling should be preserved through the PATCH re-projection",
    );
    assert!(
        refetched.info.capabilities.vision.enabled,
        "vision should be preserved through the PATCH re-projection",
    );
}

/// Verify the capability merge: when the request flips a capability flag
/// (e.g. `function_calling=true` → false) the read path reflects the change
/// AND the unchanged JSONB sub-object fields (e.g. vision mime types) stay
/// intact — i.e. the columns and JSONB sub-object columns are not stomping
/// each other.
#[tokio::test]
async fn capability_flip_round_trips_with_jsonb_intact() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    // Seed: function_calling=true, vision enabled with jpeg mime types.
    service
        .create_model(&ctx, &make_create_model_req("openai", "gpt-4o"))
        .await
        .expect("create model");

    // PATCH: flip function_calling off (preserving the rest of the
    // capabilities). `ModelCapabilities` is `#[non_exhaustive]` so we
    // round-trip through JSON.
    let original_caps =
        serde_json::to_value(&make_create_model_req("openai", "gpt-4o").info.capabilities)
            .expect("serialize caps");
    let mut caps_value = original_caps;
    if let Some(obj) = caps_value.as_object_mut() {
        obj.insert("function_calling".into(), serde_json::Value::Bool(false));
    }
    let patched_caps: model_registry_sdk::models::ModelCapabilities =
        serde_json::from_value(caps_value).expect("deserialize caps");

    let patched = service
        .update_model(
            &ctx,
            "openai::gpt-4o",
            &UpdateModelRequestV1 {
                capabilities: Some(patched_caps),
                ..Default::default()
            },
        )
        .await
        .expect("flip function_calling off");

    // The promoted scalar column `cap_function_calling` was updated.
    assert!(
        !patched.info.capabilities.function_calling,
        "function_calling must be flipped off via the scalar column"
    );
    // Unchanged JSONB-side capability fields (vision mime types) are
    // preserved by `capabilities_full`.
    assert!(
        patched.info.capabilities.vision.enabled,
        "vision.enabled must be preserved when only function_calling is patched"
    );
    // JSONB-only sub-fields inside `capabilities_full` (e.g. mime types)
    // must survive the PATCH round-trip — this is what `capabilities_full`
    // stores and the read path must re-hydrate intact.
    assert_eq!(
        patched.info.capabilities.vision.supported_mime_types,
        vec!["image/jpeg".to_owned()],
        "vision.supported_mime_types must survive PATCH round-trip via capabilities_full"
    );

    // Re-read confirms the change is durable.
    let refetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("refetch after capability flip");
    assert!(
        !refetched.info.capabilities.function_calling,
        "function_calling must remain off after re-read"
    );
    assert!(
        refetched.info.capabilities.vision.enabled,
        "vision.enabled must remain on after re-read (JSONB content preserved)"
    );
    assert_eq!(
        refetched.info.capabilities.vision.supported_mime_types,
        vec!["image/jpeg".to_owned()],
        "vision.supported_mime_types must survive re-read via capabilities_full"
    );
}
