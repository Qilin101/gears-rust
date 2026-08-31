//! End-to-end integration tests for the Model Registry gear.
//!
//! These tests construct a full service stack (real `ProviderRepositoryImpl` /
//! `ModelRepositoryImpl` over in-memory `SQLite`, a stub `ResolutionCache`, real
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
//! | `warm_chain_skips_tenant_resolver` | two reads behind a counting stub cache | the chain is resolved once and served warm after |
//! | `writes_never_touch_the_cache` | provider + model writes behind a counting stub cache | no write performs a cache operation |
//!
//! All tests run against an in-memory `SQLite` database with the production
//! migration applied, giving high confidence the storage layer works
//! end-to-end without needing Postgres.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
};
use model_registry::config::ModelRegistryConfig;
use model_registry::domain::cache::{NoopResolutionCache, ResolutionCache};
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
use toolkit_odata::{CursorV1, ODataQuery, parse_filter_string};
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

/// A PDP that grants access across a fixed set of tenants, regardless of the
/// caller's own tenant — the shape a platform-admin policy produces.
struct MockAuthZTenants(Vec<Uuid>);

#[async_trait]
impl AuthZResolverClient for MockAuthZTenants {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::EvaluationResponseContext {
                constraints: vec![authz_resolver_sdk::constraints::Constraint {
                    predicates: vec![authz_resolver_sdk::constraints::Predicate::In(
                        authz_resolver_sdk::constraints::InPredicate::new(
                            "owner_tenant_id",
                            self.0.iter().map(ToString::to_string),
                        ),
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

/// Wraps a resolver and counts its `get_ancestors` calls, so a test can prove a
/// warm chain never reaches `tenant-resolver`.
struct CountingResolver<R> {
    inner: R,
    calls: Arc<AtomicUsize>,
}

impl<R> CountingResolver<R> {
    /// Returns the wrapper alongside a handle to its counter, so the counter
    /// stays readable after the resolver is moved into the `Service`.
    fn new(inner: R) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                inner,
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }
}

#[async_trait]
impl<R: TenantResolverClient + Send + Sync> TenantResolverClient for CountingResolver<R> {
    async fn get_ancestors(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.get_ancestors(ctx, id, options).await
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

/// An in-process [`ResolutionCache`] that counts every operation, standing in
/// for a live cluster backend.
#[derive(Default)]
struct StubCache {
    entries: Mutex<HashMap<Uuid, Vec<Uuid>>>,
    gets: AtomicUsize,
    puts: AtomicUsize,
}

impl StubCache {
    fn gets(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }

    fn puts(&self) -> usize {
        self.puts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ResolutionCache for StubCache {
    async fn get_chain(&self, tenant_id: Uuid) -> Option<Vec<Uuid>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&tenant_id)
            .cloned()
    }

    async fn put_chain(&self, tenant_id: Uuid, chain: &[Uuid]) {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(tenant_id, chain.to_vec());
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
    gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~")
}

fn make_create_provider_req(slug: &str, name: &str) -> CreateProviderRequestV1 {
    CreateProviderRequestV1::builder(slug, name, make_provider_gts()).build()
}

fn make_create_model_req(provider_id: Uuid, provider_model_id: &str) -> CreateModelRequestV1 {
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
        provider_id,
        lifecycle_status: LifecycleStatus::Production,
        approval_status: None,
        info,
    }
}

/// Same as [`make_create_model_req`], but the model is created `approved`.
///
/// The eval read paths gate on `approval_status = approved`, so any test whose
/// subject is something else — projection round-trips, `OData` filtering,
/// inheritance, provider status — needs an approved fixture to reach the
/// behavior it is actually asserting.
fn make_create_approved_model_req(
    provider_id: Uuid,
    provider_model_id: &str,
) -> CreateModelRequestV1 {
    CreateModelRequestV1 {
        approval_status: Some(ApprovalStatus::Approved),
        ..make_create_model_req(provider_id, provider_model_id)
    }
}

/// Build a full `Service` instance for integration testing, with the chain
/// cache disabled so every read resolves its ancestors through the resolver.
fn build_service<R: TenantResolverClient + Send + Sync + 'static>(
    db: DBProvider<DbError>,
    tenant_resolver: R,
) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl> {
    build_service_with_config(db, tenant_resolver, ModelRegistryConfig::default())
}

/// Build a `Service` whose repositories carry `cfg`'s pagination bounds, wired
/// the same way `gear.rs` does.
fn build_service_with_config<R: TenantResolverClient + Send + Sync + 'static>(
    db: DBProvider<DbError>,
    tenant_resolver: R,
    cfg: ModelRegistryConfig,
) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl> {
    build_service_with_cache(db, tenant_resolver, cfg, Arc::new(NoopResolutionCache))
}

/// Build a `Service` over an explicit [`ResolutionCache`], for the tests whose
/// subject is the cache itself.
fn build_service_with_cache<R: TenantResolverClient + Send + Sync + 'static>(
    db: DBProvider<DbError>,
    tenant_resolver: R,
    cfg: ModelRegistryConfig,
    cache: Arc<dyn ResolutionCache>,
) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl> {
    let enforcer = authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(MockAuthZ));
    let limits = cfg.page_limits();
    Service::new(
        Arc::new(db),
        Arc::new(ProviderRepositoryImpl::new(limits)),
        Arc::new(ModelRepositoryImpl::new(limits)),
        cache,
        Arc::new(tenant_resolver),
        enforcer,
        cfg,
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

/// Create an **approved** model in the given tenant via the repository directly.
///
/// Approved rather than `pending` (the request default) because the eval read
/// paths gate on `approval_status = approved`: a `pending` fixture would make an
/// inheritance / shadowing / provider-status test pass for the wrong reason.
/// Tests that exercise the approval gate itself set the status explicitly.
async fn create_model_direct(
    repo: &ModelRepositoryImpl,
    conn: &impl DBRunner,
    tenant_id: Uuid,
    provider_slug: &str,
    provider_model_id: &str,
) -> ModelV1 {
    let scope = scope_for(tenant_id);
    // The repository takes an already-resolved provider (the service owns the
    // ownership check), but a test names its provider by slug, so the fixture
    // does the lookup the production write path no longer performs.
    let provider = ProviderRepository::find_all_by_slug(
        &ProviderRepositoryImpl::default(),
        conn,
        &scope,
        provider_slug,
    )
    .await
    .expect("provider lookup succeeds in test setup")
    .pop()
    .expect("provider exists in test setup");
    let mut req = make_create_model_req(provider.id, provider_model_id);
    req.approval_status = Some(ApprovalStatus::Approved);
    ModelRepository::create(repo, conn, &scope, tenant_id, &provider, &req)
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
        .create_model(&ctx, &make_create_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model");
    // canonical_id is derived server-side from the provider's slug.
    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.provider_id, provider.id);
    assert_eq!(model.lifecycle_status, LifecycleStatus::Production);
    assert_eq!(model.approval_status, ApprovalStatus::Pending);

    // ── Step 3: a pending model is refused by the eval read ─────────────────
    let err = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect_err("a pending model must not resolve for eval");
    assert!(
        matches!(&err, DomainError::ModelNotApproved { .. }),
        "expected ModelNotApproved, got: {err:?}"
    );

    // ── Step 4: …and is absent from the eval listing, even when the caller's
    //           $filter names its status — $filter narrows, never widens ─────
    let parsed = parse_filter_string("approval_status eq 'pending'").expect("parse filter");
    let query = ODataQuery {
        filter: Some(Box::new(parsed.into_expr())),
        ..Default::default()
    };
    let page = service
        .list_tenant_models(&ctx, &query)
        .await
        .expect("list models with OData filter");
    assert!(
        page.items.is_empty(),
        "a pending model must not be listed for eval, got {} row(s)",
        page.items.len()
    );

    // ── Step 5: the management listing does return it, marked unavailable ────
    let mgmt = service
        .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
        .await
        .expect("management list");
    assert_eq!(mgmt.items.len(), 1, "management listing shows the row");
    assert!(
        !mgmt.items[0].available_for_eval,
        "a pending model is not available for eval"
    );

    // ── Step 6: Update approval to Approved ─────────────────────────────────
    let approved = service
        .update_model(
            &ctx,
            model.id,
            &UpdateModelRequestV1 {
                approval_status: Some(ApprovalStatus::Approved),
                ..Default::default()
            },
        )
        .await
        .expect("approve model");
    assert_eq!(approved.approval_status, ApprovalStatus::Approved);

    // ── Step 7: the approval opens the gate on the very next read ───────────
    let refetched = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("get approved model");
    assert_eq!(refetched.approval_status, ApprovalStatus::Approved);
    let listed = service
        .list_tenant_models(&ctx, &ODataQuery::default())
        .await
        .expect("list approved models");
    assert_eq!(
        listed.items.len(),
        1,
        "the approved model is now eval-visible"
    );

    // ── Step 8: Soft-delete the model ───────────────────────────────────────
    service
        .delete_model(&ctx, model.id)
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
        .list_tenant_models(&ctx, &ODataQuery::default())
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
    let model = service
        .create_model(&ctx_a, &make_create_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model in tenant A");

    // Tenant B should not see any providers.
    let providers_page = service
        .list_providers(&ctx_b, &ODataQuery::default())
        .await
        .expect("list providers as tenant B");
    assert!(
        providers_page.items.is_empty(),
        "tenant B should see no providers"
    );

    // Tenant B should not see any models.
    let models_page = service
        .list_tenant_models(&ctx_b, &ODataQuery::default())
        .await
        .expect("list models as tenant B");
    assert!(
        models_page.items.is_empty(),
        "tenant B should see no models"
    );

    // Tenant B should not be able to get the model. With the rewritten
    // slug-resolution path, an unresolvable slug yields ProviderNotFoundBySlug
    // (still 404 on the wire).
    let get_err = service
        .get_tenant_model(&ctx_b, "openai::gpt-4o")
        .await
        .expect_err("tenant B get model should fail");
    assert!(
        matches!(get_err, DomainError::ProviderNotFoundBySlug { .. }),
        "expected ProviderNotFoundBySlug, got {get_err:?}"
    );

    // Nor via the id-keyed management read, even holding the real UUID: the
    // access scope, not the identifier's obscurity, is what isolates tenants.
    let get_by_id_err = service
        .get_model(&ctx_b, model.id)
        .await
        .expect_err("tenant B get_model should fail");
    assert!(
        matches!(get_by_id_err, DomainError::ModelNotFoundById { .. }),
        "expected ModelNotFoundById, got {get_by_id_err:?}"
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
            model.id,
            &UpdateModelRequestV1 {
                approval_status: Some(ApprovalStatus::Approved),
                ..Default::default()
            },
        )
        .await
        .expect_err("tenant B update model should fail");
    assert!(
        matches!(update_err, DomainError::ModelNotFoundById { .. }),
        "expected ModelNotFoundById, got {update_err:?}"
    );

    // Tenant B should not be able to delete the model.
    let delete_err = service
        .delete_model(&ctx_b, model.id)
        .await
        .expect_err("tenant B delete model should fail");
    assert!(
        matches!(delete_err, DomainError::ModelNotFoundById { .. }),
        "expected ModelNotFoundById, got {delete_err:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. Inheritance: child tenant inherits provider + model from parent
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn child_inherits_provider_and_model_from_parent() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    // Create provider and model in the parent tenant (via direct repo calls
    // to isolate from the service and authz layers).
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
        .list_tenant_models(&ctx, &ODataQuery::default())
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
        ApprovalStatus::Approved,
        "inherited model should have populated approval_status"
    );

    // ── The admin provider reads stay inside the PDP scope ──────────────────
    // Inheritance is an eval-path mechanism; the parent's provider row is not
    // in the child's own-tenant scope.
    let providers_page = service
        .list_providers(&ctx, &ODataQuery::default())
        .await
        .expect("child list providers");
    assert!(
        providers_page.items.is_empty(),
        "the admin listing must not reach the parent's provider"
    );

    let err = service
        .get_provider(&ctx, provider_id)
        .await
        .expect_err("the parent's provider is outside the child's scope");
    assert!(
        matches!(&err, DomainError::ProviderNotFound { id } if *id == provider_id),
        "expected ProviderNotFound({provider_id}), got: {err:?}"
    );
}

/// Seed a parent tenant with one provider and three models, then assert the
/// child's `$top`-less listing comes back bounded to two rows by `cfg`.
///
/// The child owns no providers, so every row is inherited. The chain-wide query
/// paginates those rows like any other, so the configured bound has to hold and
/// the remaining row must stay reachable through the cursor.
async fn assert_inherited_listing_bounded_to_two(cfg: ModelRegistryConfig) {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    let (_provider_id, provider_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    for i in 0..3 {
        create_model_direct(
            &model_repo,
            &conn,
            parent_tenant(),
            &provider_slug,
            &format!("gpt-{i}"),
        )
        .await;
    }

    let service = build_service_with_config(db, OneAncestorResolver, cfg);
    let ctx = security_context(child_tenant());

    let page = service
        .list_tenant_models(&ctx, &ODataQuery::default())
        .await
        .expect("child list inherited models");
    assert_eq!(page.items.len(), 2, "merged page must be bounded by config");
    assert_eq!(page.page_info.limit, 2);
    assert!(
        page.page_info.next_cursor.is_some(),
        "the third inherited model must stay reachable rather than be truncated away"
    );
}

#[tokio::test]
async fn configured_max_page_size_bounds_inherited_listing() {
    assert_inherited_listing_bounded_to_two(ModelRegistryConfig {
        max_page_size: 2,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn configured_default_page_size_bounds_inherited_listing() {
    assert_inherited_listing_bounded_to_two(ModelRegistryConfig {
        default_page_size: 2,
        ..Default::default()
    })
    .await;
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3b. Inherited listing is one globally-ordered, fully-pageable set
// ═══════════════════════════════════════════════════════════════════════════════

/// Seed the parent with `openai::a` / `openai::c` / `openai::e` and the child
/// with `child-co::b` / `child-co::d`, so a correct global sort has to
/// interleave the two tenants rather than concatenate them.
async fn seed_interleaved_chain(db: &DBProvider<DbError>) {
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    for model_id in ["a", "c", "e"] {
        create_model_direct(&model_repo, &conn, parent_tenant(), "openai", model_id).await;
    }

    create_provider_direct(&provider_repo, &conn, child_tenant(), "child-co").await;
    for model_id in ["b", "d"] {
        create_model_direct(&model_repo, &conn, child_tenant(), "child-co", model_id).await;
    }
}

/// Own and inherited rows must come back as one sorted sequence, not as
/// own-rows-then-ancestor-rows.
#[tokio::test]
async fn inherited_listing_is_globally_ordered_not_own_first() {
    let db = setup_db().await;
    seed_interleaved_chain(&db).await;
    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    let page = service
        .list_tenant_models(&ctx, &ODataQuery::default())
        .await
        .expect("child lists the merged set");

    let ids: Vec<&str> = page.items.iter().map(|m| m.canonical_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "child-co::b",
            "child-co::d",
            "openai::a",
            "openai::c",
            "openai::e"
        ],
        "the merged set must be sorted by canonical_id across both tenants"
    );
}

/// Walk the whole inherited catalog two rows at a time. Before the chain-wide
/// query this was impossible: the cursor was suppressed as soon as any
/// inherited row appeared, so page 2 could never be requested.
#[tokio::test]
async fn inherited_listing_pages_end_to_end() {
    let db = setup_db().await;
    seed_interleaved_chain(&db).await;
    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    let mut seen: Vec<String> = Vec::new();
    let mut cursor = None;
    for page_number in 1..=4 {
        let page = service
            .list_tenant_models(
                &ctx,
                &ODataQuery {
                    limit: Some(2),
                    cursor: cursor.clone(),
                    ..ODataQuery::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("page {page_number} must resolve: {e}"));

        assert!(
            page.items.len() <= 2,
            "page {page_number} must respect $top"
        );
        seen.extend(page.items.iter().map(|m| m.canonical_id.clone()));

        match page.page_info.next_cursor {
            Some(token) => {
                cursor = Some(CursorV1::decode(&token).expect("cursor decodes"));
            }
            None => break,
        }
    }

    assert_eq!(
        seen,
        [
            "child-co::b",
            "child-co::d",
            "openai::a",
            "openai::c",
            "openai::e"
        ],
        "the walk must cover every visible row exactly once, in sort order"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. Child shadows parent: child creates model with same canonical_id as parent
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn child_shadows_parent_by_same_canonical_id() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

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
        .list_tenant_models(&ctx, &ODataQuery::default())
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
// 4b. Child shadows slug but has NO colliding model — ancestor's model must
//     be invisible from the eval listing (headline shadowing bug fix)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn child_shadows_slug_no_colliding_model_hides_ancestor_model() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    // Create provider and model in the parent tenant.
    let (_parent_provider_id, parent_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    create_model_direct(&model_repo, &conn, parent_tenant(), &parent_slug, "gpt-4o").await;

    // Create ONLY a provider in the child tenant with the SAME slug "openai",
    // but NO model: the child shadows the slug but
    // has no colliding model.
    create_provider_direct(&provider_repo, &conn, child_tenant(), "openai").await;

    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    // Child's list should show ZERO models — the parent's model is hidden
    // because the parent's provider lost the slug to the child, and the child
    // has no model of its own.
    let page = service
        .list_tenant_models(&ctx, &ODataQuery::default())
        .await
        .expect("child list models");
    assert!(
        page.items.is_empty(),
        "ancestor model must be hidden when child owns the slug but has no model, got {} items",
        page.items.len(),
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4c. Disabled provider hides models from the eval listing
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn disabled_provider_hides_models_from_eval_listing() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();
    let tenant_id = tenant_a();

    // Create provider and model.
    let (_pid, slug) = create_provider_direct(&provider_repo, &conn, tenant_id, "openai").await;
    create_model_direct(&model_repo, &conn, tenant_id, &slug, "gpt-4o").await;

    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_id);

    // Verify model is visible before disabling.
    let before = service
        .list_tenant_models(&ctx, &ODataQuery::default())
        .await
        .expect("list before disable");
    assert_eq!(
        before.items.len(),
        1,
        "model visible before provider disable"
    );

    // Disable the provider via update_model (uses service properly).
    let provider_id = before.items[0].provider_id;
    service
        .update_provider(
            &ctx,
            provider_id,
            &UpdateProviderRequestV1 {
                status: Some(model_registry::ProviderStatus::Disabled),
                ..Default::default()
            },
        )
        .await
        .expect("disable provider");

    // After disabling, the model should be hidden from the eval listing.
    let after = service
        .list_tenant_models(&ctx, &ODataQuery::default())
        .await
        .expect("list after disable");
    assert!(
        after.items.is_empty(),
        "model must be hidden after provider is disabled"
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
    let openai = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    // Create two models with different providers but same vendor/family.
    let _model1 = service
        .create_model(&ctx, &make_create_approved_model_req(openai.id, "gpt-4o"))
        .await
        .expect("create model gpt-4o");

    service
        .create_provider(&ctx, &make_create_provider_req("anthropic", "Anthropic"))
        .await
        .expect("create provider anthropic");

    // Create a second provider with different slug for second model.
    // (The `gts_type` in the info drives the `gts_type` column.)
    let mut model2_req = make_create_approved_model_req(openai.id, "gpt-4o-mini");
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

    // Both models are created approved: the eval listing only ever returns
    // approved rows, so a pending fixture would drop out of every assertion
    // below and this test would stop covering the filter surface.

    // ── Filter by approval_status eq 'approved' ─────────────────────────────
    let parsed = parse_filter_string("approval_status eq 'approved'").expect("parse");
    let page = service
        .list_tenant_models(
            &ctx,
            &ODataQuery {
                filter: Some(Box::new(parsed.into_expr())),
                ..Default::default()
            },
        )
        .await
        .expect("list filtered by approval_status");
    assert_eq!(page.items.len(), 2, "both models are approved");

    // ── Filter by vision eq true ────────────────────────────────────────────
    let parsed = parse_filter_string("vision eq true").expect("parse");
    let page = service
        .list_tenant_models(
            &ctx,
            &ODataQuery {
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
            &ODataQuery {
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
            &ODataQuery {
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
        .list_providers(&ctx, &ODataQuery::default())
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

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let mut req = make_create_approved_model_req(provider.id, "gpt-4o");
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

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let mut req = make_create_approved_model_req(provider.id, "gpt-4o");
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

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let mut req = make_create_approved_model_req(provider.id, "gpt-4o");
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

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let mut req = make_create_approved_model_req(provider.id, "gpt-4o");
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

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    let model = service
        .create_model(&ctx, &make_create_approved_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model");

    // PATCH: bump display_name and context_window.max_input_tokens. The PATCH
    // must re-project every promoted column, not just the changed ones.
    let patched = service
        .update_model(
            &ctx,
            model.id,
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

    // Read back from the DB directly to confirm the columns were written.
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

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");

    // Seed: function_calling=true, vision enabled with jpeg mime types.
    let model = service
        .create_model(&ctx, &make_create_approved_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model");

    // PATCH: flip function_calling off (preserving the rest of the
    // capabilities). `ModelCapabilities` is `#[non_exhaustive]` so we
    // round-trip through JSON.
    let original_caps = serde_json::to_value(
        &make_create_approved_model_req(provider.id, "gpt-4o")
            .info
            .capabilities,
    )
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
            model.id,
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

// ═══════════════════════════════════════════════════════════════════════════════
// 7. Shadowed provider: child shadows parent's slug, the ancestor's model
//    must NOT be served
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn child_shadows_provider_slug_blocks_ancestor_model_get() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    // Step 1: Create provider + model in the parent tenant.
    let (_parent_provider_id, parent_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    create_model_direct(&model_repo, &conn, parent_tenant(), &parent_slug, "gpt-4o").await;

    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    // Step 2: Child gets the inherited model.
    let inherited = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("child should inherit model before shadow");
    assert_eq!(inherited.canonical_id, "openai::gpt-4o");

    // Step 3: Child creates their OWN provider with the SAME slug "openai",
    // shadowing the parent's provider.
    let child_provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "Child OpenAI"))
        .await
        .expect("child creates own openai provider");

    // Step 4: Child's get_tenant_model should fail with ModelNotFound
    // (child owns the slug but has NO model for "gpt-4o"). The parent's model
    // must NOT be served even though Step 2 returned it.
    let err = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect_err("child slug winner has no model");

    assert!(
        matches!(&err, DomainError::ModelNotFound { .. }),
        "expected ModelNotFound (child wins slug but has no model), got: {err:?}"
    );

    // Step 5: Create a model under the child's provider with the same
    // canonical_id and verify the child's model IS returned. The child names
    // its OWN provider by id — with slugs this call was ambiguous between the
    // child's provider and the parent's.
    service
        .create_model(
            &ctx,
            &make_create_approved_model_req(child_provider.id, "gpt-4o"),
        )
        .await
        .expect("child creates own model");

    let childs_model = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("child's own model should now be found");
    assert_eq!(childs_model.canonical_id, "openai::gpt-4o");
    assert_eq!(
        childs_model.provider_id, child_provider.id,
        "child's model must reference child's provider, not the parent's"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 8. Provider create drops slug tombstone
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn create_provider_drops_slug_tombstone() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();

    // Create a provider in the parent tenant.
    create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;

    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    // Step 1: Try getting a model — child has no provider or model,
    // so slug resolution for "openai" hits the parent. No model exists.
    let child_can_get = service.get_tenant_model(&ctx, "openai::gpt-4o").await;
    assert!(
        matches!(&child_can_get, Err(DomainError::ModelNotFound { .. })),
        "expected ModelNotFound (no model yet), got: {child_can_get:?}"
    );

    // Step 2: Child creates their own provider with slug "openai".
    let child_provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "Child OpenAI"))
        .await
        .expect("child creates own openai provider");

    // Step 3: Create a model under the child's provider.
    service
        .create_model(
            &ctx,
            &make_create_approved_model_req(child_provider.id, "gpt-4o"),
        )
        .await
        .expect("child creates model");

    // Step 4: The new slug resolution should find child's provider and
    // serve the child's model.
    let model = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("child's model should be found after create");
    assert_eq!(model.canonical_id, "openai::gpt-4o");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 9. Disabled winning provider (gate-ordering disclosure rule)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn disabled_provider_hides_model_on_get() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    // Create provider and model.
    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");
    service
        .create_model(&ctx, &make_create_approved_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model");

    // Verify the model is accessible.
    let model = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("model should be found before disabling provider");
    assert_eq!(model.canonical_id, "openai::gpt-4o");

    // Disable the provider.
    service
        .update_provider(
            &ctx,
            provider.id,
            &UpdateProviderRequestV1 {
                status: Some(model_registry::ProviderStatus::Disabled),
                ..Default::default()
            },
        )
        .await
        .expect("disable provider");

    // A resolvable canonical_id on a disabled provider should yield ProviderDisabled.
    let err = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect_err("disabled provider should block model get");

    assert!(
        matches!(&err, DomainError::ProviderDisabled { .. }),
        "expected ProviderDisabled for model on disabled provider, got: {err:?}"
    );
}

#[tokio::test]
async fn disabled_provider_hides_model_when_no_model_exists() {
    // When the winning provider is disabled and the model does not exist,
    // the system should resolve the slug and then report ModelNotFound
    // (the model read happens before the provider status gate, per C4 ordering).
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();

    // Create a provider directly, then disable it via update.
    let scope = scope_for(tenant_a());
    let p = ProviderRepository::create(
        &provider_repo,
        &conn,
        &scope,
        tenant_a(),
        &CreateProviderRequestV1::builder("openai", "OpenAI", make_provider_gts()).build(),
    )
    .await
    .expect("create provider");

    // Disable the provider via repository update.
    ProviderRepository::update(
        &provider_repo,
        &conn,
        &scope,
        p.id,
        &UpdateProviderRequestV1 {
            status: Some(model_registry::ProviderStatus::Disabled),
            ..Default::default()
        },
    )
    .await
    .expect("disable provider");

    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    // A canonical_id whose slug resolves to a disabled provider, but no
    // matching model exists → ModelNotFound.
    let err = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect_err("non-existent model with disabled provider");

    assert!(
        matches!(&err, DomainError::ModelNotFound { .. }),
        "expected ModelNotFound for non-existent model behind disabled provider, got: {err:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Mock AuthZResolverClient — action-aware: denies `list_management`, permits all else
// ═══════════════════════════════════════════════════════════════════════════════

struct MockAuthDenyingListManagement;

#[async_trait]
impl AuthZResolverClient for MockAuthDenyingListManagement {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        // Deny list_management; allow everything else.
        if request.action.name == "list_management" {
            return Ok(EvaluationResponse {
                decision: false,
                context: authz_resolver_sdk::EvaluationResponseContext {
                    constraints: vec![],
                    deny_reason: Some(authz_resolver_sdk::DenyReason {
                        error_code: "NOT_GRANTED".to_owned(),
                        details: None,
                    }),
                },
            });
        }

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

/// Build a full `Service` instance with a custom enforcer for integration testing.
fn build_service_with_enforcer<R: TenantResolverClient + Send + Sync + 'static>(
    db: DBProvider<DbError>,
    tenant_resolver: R,
    enforcer: authz_resolver_sdk::pep::PolicyEnforcer,
) -> Service<ProviderRepositoryImpl, ModelRepositoryImpl> {
    Service::new(
        Arc::new(db),
        Arc::new(ProviderRepositoryImpl::default()),
        Arc::new(ModelRepositoryImpl::default()),
        Arc::new(NoopResolutionCache),
        Arc::new(tenant_resolver),
        enforcer,
        ModelRegistryConfig::default(),
    )
}

// ═══════════════════════════════════════════════════════════════════════════════
// 7. Management listing integration tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_management_denied_without_grant() {
    let db = setup_db().await;
    let enforcer =
        authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(MockAuthDenyingListManagement));
    let service = build_service_with_enforcer(db, NoAncestorsResolver, enforcer);
    let ctx = security_context(tenant_a());

    let err = service
        .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
        .await
        .expect_err("list_management must be denied without grant");

    assert!(
        matches!(&err, DomainError::Forbidden { .. }),
        "expected Forbidden for list_management without grant, got: {err:?}"
    );
}

#[tokio::test]
async fn management_listing_is_bounded_by_the_pdp_scope() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    // Parent: provider "openai", model "openai::gpt-4o".
    let (_parent_provider_id, parent_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    let parent_model =
        create_model_direct(&model_repo, &conn, parent_tenant(), &parent_slug, "gpt-4o").await;

    // Child: same provider slug "openai" but a DIFFERENT model.
    let (_child_provider_id, child_slug) =
        create_provider_direct(&provider_repo, &conn, child_tenant(), "openai").await;
    let child_model =
        create_model_direct(&model_repo, &conn, child_tenant(), &child_slug, "claude-4").await;

    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    // Approve the child's model so available_for_eval is correctly computed.
    service
        .update_model(
            &ctx,
            child_model.id,
            &model_registry::UpdateModelRequestV1 {
                approval_status: Some(model_registry::ApprovalStatus::Approved),
                ..Default::default()
            },
        )
        .await
        .expect("approve child model");

    // Eval listing resolves the chain: the parent's model is shadowed.
    let eval_page = service
        .list_tenant_models(&ctx, &ODataQuery::default())
        .await
        .expect("eval list");
    assert_eq!(
        eval_page.items.len(),
        1,
        "eval listing must show exactly 1 model (child's only)"
    );
    assert_eq!(
        eval_page.items[0].id, child_model.id,
        "eval listing must show child's model, not parent's"
    );

    // Management listing carries the PDP scope, which `MockAuthZ` pins to the
    // caller's own tenant. The parent's row is out of scope.
    let mgmt_page = service
        .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
        .await
        .expect("management list");

    assert_eq!(
        mgmt_page.items.len(),
        1,
        "management listing must not reach the parent tenant"
    );
    assert_eq!(mgmt_page.items[0].model.id, child_model.id);
    assert!(
        mgmt_page.items[0].available_for_eval,
        "child's own model must be available for eval"
    );
    assert!(
        !mgmt_page
            .items
            .iter()
            .any(|m| m.model.id == parent_model.id),
        "the ancestor's model must not appear in the admin listing"
    );
}

#[tokio::test]
async fn management_listing_spans_every_tenant_the_pdp_grants() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    let (_parent_provider_id, parent_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    let parent_model =
        create_model_direct(&model_repo, &conn, parent_tenant(), &parent_slug, "gpt-4o").await;

    let (_child_provider_id, child_slug) =
        create_provider_direct(&provider_repo, &conn, child_tenant(), "anthropic").await;
    let child_model =
        create_model_direct(&model_repo, &conn, child_tenant(), &child_slug, "claude-4").await;

    // No ancestor relationship: reach comes from the PDP grant alone.
    let enforcer = authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(MockAuthZTenants(vec![
        child_tenant(),
        parent_tenant(),
    ])));
    let service = build_service_with_enforcer(db, NoAncestorsResolver, enforcer);
    let ctx = security_context(child_tenant());

    let mgmt_page = service
        .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
        .await
        .expect("management list");

    let mut ids: Vec<Uuid> = mgmt_page.items.iter().map(|m| m.model.id).collect();
    ids.sort_unstable();
    let mut expected = vec![parent_model.id, child_model.id];
    expected.sort_unstable();
    assert_eq!(
        ids, expected,
        "the admin listing must cover both granted tenants"
    );
    assert!(
        mgmt_page.items.iter().all(|m| !m.provider_disabled),
        "both providers are active"
    );
}

#[tokio::test]
async fn management_listing_shows_disabled_provider_rows() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();
    let tenant_id = tenant_a();
    let _scope = scope_for(tenant_id);

    // Create provider "openai" and model "gpt-4o".
    let (pid, slug) = create_provider_direct(&provider_repo, &conn, tenant_id, "openai").await;
    let model = create_model_direct(&model_repo, &conn, tenant_id, &slug, "gpt-4o").await;

    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_id);

    // Disable the provider via service update.
    service
        .update_provider(
            &ctx,
            pid,
            &UpdateProviderRequestV1 {
                status: Some(model_registry::ProviderStatus::Disabled),
                ..Default::default()
            },
        )
        .await
        .expect("disable provider");

    // Eval listing must be empty (provider disabled hides models).
    let eval_page = service
        .list_tenant_models(&ctx, &ODataQuery::default())
        .await
        .expect("eval list after disable");
    assert!(
        eval_page.items.is_empty(),
        "eval listing must be empty when provider is disabled"
    );

    // Management listing must still show the model, with provider_disabled=true.
    let mgmt_page = service
        .list_tenant_models_management(&ctx, &ODataQuery::default(), false)
        .await
        .expect("management list after disable");
    assert_eq!(
        mgmt_page.items.len(),
        1,
        "management listing must show the disabled provider's model"
    );

    let mgmt = &mgmt_page.items[0];
    assert!(
        mgmt.provider_disabled,
        "model from disabled provider must have provider_disabled=true"
    );
    assert!(
        !mgmt.available_for_eval,
        "model from disabled provider must not be available for eval"
    );
    assert_eq!(
        mgmt.model.id, model.id,
        "management listing must reference the correct model"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 8. Resolution cache: the chain is cached, nothing else is
// ═══════════════════════════════════════════════════════════════════════════════

/// The first read resolves the chain through `tenant-resolver` and stores it;
/// every later read is served warm. A root tenant's empty ancestor list is a
/// hit, not a miss — so it must not trigger a second resolver call either.
#[tokio::test]
async fn warm_chain_skips_tenant_resolver() {
    let db = setup_db().await;
    let cache = Arc::new(StubCache::default());
    let (resolver, resolver_calls) = CountingResolver::new(NoAncestorsResolver);
    let service = build_service_with_cache(
        db,
        resolver,
        ModelRegistryConfig::default(),
        Arc::clone(&cache) as Arc<dyn ResolutionCache>,
    );
    let ctx = security_context(tenant_a());

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");
    service
        .create_model(&ctx, &make_create_approved_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model");

    // Writes resolved no chain, so the first read is the cold one.
    assert_eq!(
        resolver_calls.load(Ordering::SeqCst),
        0,
        "writes resolve no ancestor chain"
    );

    service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect("cold read");
    assert_eq!(
        resolver_calls.load(Ordering::SeqCst),
        1,
        "the cold read resolves the chain"
    );
    assert_eq!(cache.puts(), 1, "the cold read stores the chain");

    for _ in 0..3 {
        service
            .get_tenant_model(&ctx, "openai::gpt-4o")
            .await
            .expect("warm read");
    }
    assert_eq!(
        resolver_calls.load(Ordering::SeqCst),
        1,
        "a root tenant's empty ancestor list is a hit, not a miss"
    );
    assert_eq!(cache.puts(), 1, "a warm read rewrites nothing");
}

/// No write of any kind performs a cache operation — the only cached entity is
/// the ancestor chain, which no provider or model write can change.
#[tokio::test]
async fn writes_never_touch_the_cache() {
    let db = setup_db().await;
    let cache = Arc::new(StubCache::default());
    let service = build_service_with_cache(
        db,
        NoAncestorsResolver,
        ModelRegistryConfig::default(),
        Arc::clone(&cache) as Arc<dyn ResolutionCache>,
    );
    let ctx = security_context(tenant_a());

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");
    let model = service
        .create_model(&ctx, &make_create_approved_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model");
    service
        .update_model(
            &ctx,
            model.id,
            &UpdateModelRequestV1 {
                approval_status: Some(ApprovalStatus::Rejected),
                ..Default::default()
            },
        )
        .await
        .expect("update model");
    service
        .delete_model(&ctx, model.id)
        .await
        .expect("delete model");
    service
        .update_provider(
            &ctx,
            provider.id,
            &UpdateProviderRequestV1 {
                name: Some("renamed".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("update provider");

    assert_eq!(cache.gets(), 0, "no write reads a cache entry");
    assert_eq!(cache.puts(), 0, "no write writes a cache entry");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 12. get_model — the id-keyed management read
//
// The eval read (`get_tenant_model`) is a fail-closed access gate keyed by a
// chain-relative `canonical_id`. `get_model` is neither: it is keyed by a stable
// UUID and applies no eval gates, because an admin has to read a row before
// acting on it. These tests pin the difference.
// ═══════════════════════════════════════════════════════════════════════════════

/// A `pending` model is refused by the eval read and returned by the management
/// read — the same row, two answers, by design.
#[tokio::test]
async fn get_model_returns_a_pending_row_the_eval_read_refuses() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");
    // `make_create_model_req` leaves approval_status at its default (pending).
    let created = service
        .create_model(&ctx, &make_create_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model");
    assert_eq!(created.approval_status, ApprovalStatus::Pending);

    let eval_err = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect_err("the eval read gates on approval");
    assert!(
        matches!(&eval_err, DomainError::ModelNotApproved { .. }),
        "expected ModelNotApproved, got: {eval_err:?}"
    );

    let managed = service
        .get_model(&ctx, created.id)
        .await
        .expect("the management read applies no approval gate");
    assert_eq!(managed.id, created.id);
    assert_eq!(managed.approval_status, ApprovalStatus::Pending);
}

/// A soft-deleted (deprecated) model stays readable by id. Without this the
/// admin UI could not display, or un-deprecate, what it just deleted.
#[tokio::test]
async fn get_model_returns_a_deprecated_row() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("create provider");
    let created = service
        .create_model(&ctx, &make_create_approved_model_req(provider.id, "gpt-4o"))
        .await
        .expect("create model");

    service
        .delete_model(&ctx, created.id)
        .await
        .expect("soft-delete");

    let eval_err = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect_err("the eval read gates on lifecycle");
    assert!(
        matches!(&eval_err, DomainError::ModelDeprecated { .. }),
        "expected ModelDeprecated, got: {eval_err:?}"
    );

    let managed = service
        .get_model(&ctx, created.id)
        .await
        .expect("a deprecated row is still readable by id");
    assert_eq!(managed.lifecycle_status, LifecycleStatus::Deprecated);
}

/// The admin read is bounded by the PDP scope: an ancestor's row is out of
/// reach under an own-tenant scope, and reachable under a scope that names its
/// tenant — where it is writable too.
#[tokio::test]
async fn get_model_reaches_an_ancestor_row_only_when_the_pdp_grants_its_tenant() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    // Parent owns provider "openai" and a model under it.
    let (_parent_provider_id, parent_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    let parent_model =
        create_model_direct(&model_repo, &conn, parent_tenant(), &parent_slug, "gpt-4o").await;

    // Child shadows the slug with its own provider and owns no model.
    create_provider_direct(&provider_repo, &conn, child_tenant(), "openai").await;

    let db2 = db.clone();
    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    // The eval read serves nothing: the child won the slug and owns no model.
    let eval_err = service
        .get_tenant_model(&ctx, "openai::gpt-4o")
        .await
        .expect_err("the shadowed ancestor row is not eval-visible");
    assert!(
        matches!(&eval_err, DomainError::ModelNotFound { .. }),
        "expected ModelNotFound, got: {eval_err:?}"
    );

    // Under an own-tenant scope the admin read cannot reach it either.
    let read_err = service
        .get_model(&ctx, parent_model.id)
        .await
        .expect_err("an ancestor row is outside an own-tenant scope");
    assert!(
        matches!(&read_err, DomainError::ModelNotFoundById { id } if *id == parent_model.id),
        "expected ModelNotFoundById, got: {read_err:?}"
    );

    // A PDP grant covering the parent tenant makes it both readable and
    // writable — the ancestor relationship plays no part.
    let enforcer = authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(MockAuthZTenants(vec![
        child_tenant(),
        parent_tenant(),
    ])));
    let admin = build_service_with_enforcer(db2, NoAncestorsResolver, enforcer);

    let managed = admin
        .get_model(&ctx, parent_model.id)
        .await
        .expect("a granted tenant's row is readable by id");
    assert_eq!(managed.id, parent_model.id);
    assert_eq!(managed.tenant_id, parent_tenant());

    let updated = admin
        .update_model(
            &ctx,
            parent_model.id,
            &UpdateModelRequestV1 {
                approval_status: Some(ApprovalStatus::Rejected),
                ..Default::default()
            },
        )
        .await
        .expect("a granted tenant's row is writable");
    assert_eq!(updated.approval_status, ApprovalStatus::Rejected);

    admin
        .delete_model(&ctx, parent_model.id)
        .await
        .expect("a granted tenant's row is soft-deletable");
}

/// An unknown id is a plain 404 — no chain hop leaks anything.
#[tokio::test]
async fn get_model_unknown_id_is_not_found() {
    let db = setup_db().await;
    let service = build_service(db, NoAncestorsResolver);
    let ctx = security_context(tenant_a());

    let missing = Uuid::new_v4();
    let err = service
        .get_model(&ctx, missing)
        .await
        .expect_err("unknown id");
    assert!(
        matches!(&err, DomainError::ModelNotFoundById { id } if *id == missing),
        "expected ModelNotFoundById({missing}), got: {err:?}"
    );
}

/// `create_model` addresses its provider by id, so the child/parent ambiguity a
/// slug carried is gone: an out-of-scope provider is a 404, and the caller's own
/// provider of the same slug succeeds.
#[tokio::test]
async fn create_model_provider_id_disambiguates_a_shadowed_slug() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();

    let (parent_provider_id, _parent_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    let (child_provider_id, _child_slug) =
        create_provider_direct(&provider_repo, &conn, child_tenant(), "openai").await;

    let service = build_service(db, OneAncestorResolver);
    let ctx = security_context(child_tenant());

    // The parent's provider is outside the caller's scope -> 404.
    let err = service
        .create_model(
            &ctx,
            &make_create_approved_model_req(parent_provider_id, "gpt-4o"),
        )
        .await
        .expect_err("an out-of-scope provider is not a valid target");
    assert!(
        matches!(&err, DomainError::ProviderNotFound { id } if *id == parent_provider_id),
        "expected ProviderNotFound({parent_provider_id}), got: {err:?}"
    );

    // The child's own provider of the same slug: succeeds.
    let model = service
        .create_model(
            &ctx,
            &make_create_approved_model_req(child_provider_id, "gpt-4o"),
        )
        .await
        .expect("the child's own provider is a valid target");
    assert_eq!(model.provider_id, child_provider_id);
    assert_eq!(model.tenant_id, child_tenant());
    // Both tenants now own a row named `openai::gpt-4o`; the id, not the
    // canonical_id, is what distinguishes them.
    assert_eq!(model.canonical_id, "openai::gpt-4o");
}

/// The per-tenant uniqueness pre-checks must not fire against a sibling tenant's
/// row when the PDP scope spans both. Slug and `canonical_id` are unique per
/// tenant, not per scope.
#[tokio::test]
async fn uniqueness_pre_checks_do_not_collide_across_tenants_in_scope() {
    let db = setup_db().await;
    let conn = db.conn().expect("db connection");
    let provider_repo = ProviderRepositoryImpl::default();
    let model_repo = ModelRepositoryImpl::default();

    // The parent already owns provider "openai" and model "openai::gpt-4o".
    let (_parent_provider_id, parent_slug) =
        create_provider_direct(&provider_repo, &conn, parent_tenant(), "openai").await;
    create_model_direct(&model_repo, &conn, parent_tenant(), &parent_slug, "gpt-4o").await;

    // A grant spanning both tenants.
    let enforcer = authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(MockAuthZTenants(vec![
        child_tenant(),
        parent_tenant(),
    ])));
    let service = build_service_with_enforcer(db, NoAncestorsResolver, enforcer);
    let ctx = security_context(child_tenant());

    // The child may still create its own "openai" provider.
    let provider = service
        .create_provider(&ctx, &make_create_provider_req("openai", "OpenAI"))
        .await
        .expect("a sibling tenant's slug must not block this create");
    assert_eq!(provider.tenant_id, child_tenant());

    // And its own "openai::gpt-4o" model under it.
    let model = service
        .create_model(&ctx, &make_create_approved_model_req(provider.id, "gpt-4o"))
        .await
        .expect("a sibling tenant's canonical_id must not block this create");
    assert_eq!(model.canonical_id, "openai::gpt-4o");
    assert_eq!(model.tenant_id, child_tenant());
}
