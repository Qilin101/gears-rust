//! Service-level integration tests on in-memory SQLite with the production
//! migration and the dependency doubles of `model_registry::testing`
//! (DESIGN §4 Testability, PRD §9 acceptance criteria).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::cognitive_complexity
)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::constraints::{Constraint, EqPredicate, InPredicate, Predicate};
use authz_resolver_sdk::models::Capability;
use model_registry::config::ModelRegistryConfig;
use model_registry::domain::access_guard::AccessGuard;
use model_registry::domain::clock::ManualClock;
use model_registry::domain::error::{Dependency, DomainError};
use model_registry::domain::service::Service;
use model_registry::infra::chain_cache::ChainCache;
use model_registry::infra::storage::migrations::Migrator;
use model_registry::testing::{
    StaticAuthz, StaticSchemaSource, StaticTenantResolver, deny, own_tenant_constraint,
    permit_with, security_context, security_context_for, subject_tenant,
};
use model_registry_sdk::odata::{
    MODEL_APPROVAL_STATE, MODEL_CANONICAL_ID, MODEL_VENDOR, ModelSchema, QueryBuilder, SortDir,
};
use model_registry_sdk::{
    ApprovalState, ContextWindow, CreateModelRequestV1, CreateProviderRequestV1, ModelEntryV1,
    ModelV1, SharingMode, UpdateModelRequestV1, UpdateProviderRequestV1, field,
};
use sea_orm_migration::MigratorTrait;
use tenant_resolver_sdk::BarrierMode;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const OPENAI: &str = "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~";
const STRICT: &str = "gts.cf.genai.model.provider.v1~cf.genai._.strict.v1~";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    service: Service,
    authz: Arc<StaticAuthz>,
    resolver: Arc<StaticTenantResolver>,
    schemas: Arc<StaticSchemaSource>,
    clock: Arc<ManualClock>,
}

fn schemas() -> StaticSchemaSource {
    StaticSchemaSource::new().with_open_type(OPENAI).with_type(
        STRICT,
        &serde_json::json!({
            "type": "object",
            "required": ["oagw_alias"],
            "properties": {
                "oagw_alias": { "type": "string" },
                "cost": { "type": "object", "properties": { "input": { "type": "number" } } }
            },
            "additionalProperties": false
        }),
    )
}

async fn harness_with(
    resolver: StaticTenantResolver,
    authz: StaticAuthz,
    schemas: StaticSchemaSource,
    cfg: ModelRegistryConfig,
) -> Harness {
    let raw = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("in-memory SQLite");
    run_migrations_for_testing(&raw, Migrator::migrations())
        .await
        .expect("migrations");
    let db = Arc::new(DBProvider::<DomainError>::new(raw));
    let authz = Arc::new(authz);
    let resolver = Arc::new(resolver);
    let schemas = Arc::new(schemas);
    let clock = Arc::new(ManualClock::fixed());
    let chain_cache = Arc::new(ChainCache::new(
        resolver.clone(),
        clock.clone(),
        cfg.chain_cache_ttl(),
        cfg.chain_cache_max_entries,
        cfg.tenant_resolver_deadline(),
    ));
    let guard = AccessGuard::new(
        PolicyEnforcer::new(authz.clone())
            .with_deadline(cfg.pdp_deadline())
            .with_capabilities(vec![Capability::TenantHierarchy]),
        cfg.max_pdp_constraint_values,
    );
    let service = Service::new(db, chain_cache, guard, schemas.clone(), clock.clone(), &cfg);
    Harness {
        service,
        authz,
        resolver,
        schemas,
        clock,
    }
}

async fn harness(resolver: StaticTenantResolver) -> Harness {
    harness_with(
        resolver,
        StaticAuthz::permit_own_tenant(),
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await
}

/// DESIGN §3.6 hierarchy: Root → Partner → Customer → Team, Root → Other.
struct Tenants {
    root: Uuid,
    partner: Uuid,
    other: Uuid,
    customer: Uuid,
    team: Uuid,
}

impl Tenants {
    fn new() -> Self {
        Self {
            root: Uuid::new_v4(),
            partner: Uuid::new_v4(),
            other: Uuid::new_v4(),
            customer: Uuid::new_v4(),
            team: Uuid::new_v4(),
        }
    }

    fn resolver(&self) -> StaticTenantResolver {
        StaticTenantResolver::new()
            .with_chain(self.root, vec![])
            .with_chain(self.partner, vec![self.root])
            .with_chain(self.other, vec![self.root])
            .with_chain(self.customer, vec![self.partner, self.root])
            .with_chain(self.team, vec![self.customer, self.partner, self.root])
    }

    fn all(&self) -> [Uuid; 5] {
        [
            self.root,
            self.partner,
            self.other,
            self.customer,
            self.team,
        ]
    }
}

fn ctx(tenant: Uuid) -> SecurityContext {
    security_context(tenant)
}

fn provider_req(slug: &str, mode: SharingMode, enabled: bool) -> CreateProviderRequestV1 {
    CreateProviderRequestV1::builder(slug, slug.to_uppercase(), gts::GtsTypeId::new(OPENAI))
        .sharing_mode(mode)
        .enabled(enabled)
        .build()
}

fn model_req(
    provider_id: Uuid,
    pmid: &str,
    mode: SharingMode,
    state: ApprovalState,
) -> CreateModelRequestV1 {
    let mut req = CreateModelRequestV1::new(
        provider_id,
        pmid,
        pmid,
        ContextWindow {
            max_input_tokens: 8,
            max_output_tokens: Some(4),
            output_vector_size: None,
        },
    );
    req.sharing_mode = Some(mode);
    req.approval_state = Some(state);
    req
}

impl Harness {
    async fn provider(&self, tenant: Uuid, slug: &str, mode: SharingMode, enabled: bool) -> Uuid {
        self.service
            .create_provider(&ctx(tenant), provider_req(slug, mode, enabled))
            .await
            .unwrap_or_else(|e| panic!("create provider {slug}: {e:?}"))
            .provider
            .id
    }

    async fn model(
        &self,
        tenant: Uuid,
        provider: Uuid,
        pmid: &str,
        mode: SharingMode,
        state: ApprovalState,
    ) -> Uuid {
        self.service
            .create_model(&ctx(tenant), model_req(provider, pmid, mode, state))
            .await
            .unwrap_or_else(|e| panic!("create model {pmid}: {e:?}"))
            .model
            .id
    }

    async fn resolve(&self, tenant: Uuid, canonical_id: &str) -> Result<ModelV1, DomainError> {
        self.service.resolve_model(&ctx(tenant), canonical_id).await
    }

    async fn available(&self, tenant: Uuid) -> BTreeSet<String> {
        self.service
            .list_available_models(&ctx(tenant), &ODataQuery::default())
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|m| m.canonical_id)
            .collect()
    }

    async fn managed_models(&self, tenant: Uuid) -> Vec<ModelEntryV1> {
        self.service
            .list_models(&ctx(tenant), &ODataQuery::default())
            .await
            .unwrap()
            .items
    }
}

/// The example catalog of DESIGN §3.6.
struct Catalog {
    root_openai: Uuid,
    partner_openai: Uuid,
    customer_openai: Uuid,
    root_gpt4o: Uuid,
    root_mini: Uuid,
    root_o1: Uuid,
    partner_gpt4o: Uuid,
    customer_gpt4o: Uuid,
}

async fn seed(h: &Harness, t: &Tenants, customer_enabled: bool) -> Catalog {
    let root_openai = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let partner_openai = h
        .provider(t.partner, "openai", SharingMode::Tenant, true)
        .await;
    let customer_openai = h
        .provider(t.customer, "openai", SharingMode::Shared, customer_enabled)
        .await;
    Catalog {
        root_openai,
        partner_openai,
        customer_openai,
        root_gpt4o: h
            .model(
                t.root,
                root_openai,
                "gpt-4o",
                SharingMode::Shared,
                ApprovalState::Approved,
            )
            .await,
        root_mini: h
            .model(
                t.root,
                root_openai,
                "gpt-4o-mini",
                SharingMode::Shared,
                ApprovalState::Approved,
            )
            .await,
        root_o1: h
            .model(
                t.root,
                root_openai,
                "o1-preview",
                SharingMode::Tenant,
                ApprovalState::Approved,
            )
            .await,
        partner_gpt4o: h
            .model(
                t.partner,
                partner_openai,
                "gpt-4o",
                SharingMode::Shared,
                ApprovalState::Approved,
            )
            .await,
        customer_gpt4o: h
            .model(
                t.customer,
                customer_openai,
                "gpt-4o",
                SharingMode::Shared,
                ApprovalState::Approved,
            )
            .await,
    }
}

fn ids(set: &[&str]) -> BTreeSet<String> {
    set.iter().map(|s| (*s).to_owned()).collect()
}

// ---------------------------------------------------------------------------
// Effective catalog and resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolution_follows_the_design_hierarchy_table() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;

    let root = h.resolve(t.root, "openai::gpt-4o").await.unwrap();
    assert_eq!(root.id, c.root_gpt4o);
    assert_eq!(
        h.resolve(t.root, "openai::gpt-4o-mini").await.unwrap().id,
        c.root_mini
    );
    assert_eq!(
        h.resolve(t.root, "openai::o1-preview").await.unwrap().id,
        c.root_o1
    );
    assert_eq!(
        h.available(t.root).await,
        ids(&[
            "openai::gpt-4o",
            "openai::gpt-4o-mini",
            "openai::o1-preview"
        ])
    );

    let partner = h.resolve(t.partner, "openai::gpt-4o").await.unwrap();
    assert_eq!(
        partner.id, c.partner_gpt4o,
        "own provider wins even in tenant mode"
    );
    assert!(matches!(
        h.resolve(t.partner, "openai::gpt-4o-mini").await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert_eq!(h.available(t.partner).await, ids(&["openai::gpt-4o"]));

    let other = h.resolve(t.other, "openai::gpt-4o").await.unwrap();
    assert_eq!(other.id, c.root_gpt4o);
    assert_eq!(other.info.provider_settings, serde_json::json!({}));
    assert!(
        matches!(
            h.resolve(t.other, "openai::o1-preview").await,
            Err(DomainError::ModelNotFound { .. })
        ),
        "tenant-mode model"
    );
    assert_eq!(
        h.available(t.other).await,
        ids(&["openai::gpt-4o", "openai::gpt-4o-mini"])
    );

    let customer = h.resolve(t.customer, "openai::gpt-4o").await.unwrap();
    assert_eq!(
        customer.id, c.customer_gpt4o,
        "customer's own provider is nearer than root's"
    );
    assert!(
        matches!(
            h.resolve(t.customer, "openai::gpt-4o-mini").await,
            Err(DomainError::ModelNotFound { .. })
        ),
        "no canonical_id dedupe leak"
    );
    assert_eq!(h.available(t.customer).await, ids(&["openai::gpt-4o"]));

    let team = h.resolve(t.team, "openai::gpt-4o").await.unwrap();
    assert_eq!(team.id, c.customer_gpt4o);
    assert_eq!(h.available(t.team).await, ids(&["openai::gpt-4o"]));

    for bad in ["openai", "::gpt-4o", "openai::", "nope::gpt-4o"] {
        assert!(
            matches!(
                h.resolve(t.root, bad).await,
                Err(DomainError::ModelNotFound { .. })
            ),
            "{bad}"
        );
    }
    let _ = (c.root_openai, c.partner_openai, c.customer_openai);
}

#[tokio::test]
async fn disabled_shared_shadow_blocks_the_slug_in_the_whole_subtree() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, false).await;
    for tenant in [t.customer, t.team] {
        assert!(matches!(
            h.resolve(tenant, "openai::gpt-4o").await,
            Err(DomainError::ProviderDisabled { .. })
        ));
        assert!(matches!(
            h.resolve(tenant, "openai::gpt-4o-mini").await,
            Err(DomainError::ModelNotFound { .. })
        ));
        assert!(h.available(tenant).await.is_empty());
    }
    assert_eq!(
        h.available(t.partner).await,
        ids(&["openai::gpt-4o"]),
        "the owner's subtree only"
    );

    // Re-enabling restores the models on the next request.
    h.service
        .update_provider(
            &ctx(t.customer),
            c.customer_openai,
            UpdateProviderRequestV1 {
                enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        h.resolve(t.team, "openai::gpt-4o").await.unwrap().id,
        c.customer_gpt4o
    );
}

#[tokio::test]
async fn deleting_a_shadowing_provider_restores_the_inherited_one() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;
    h.service
        .delete_provider(&ctx(t.customer), c.customer_openai)
        .await
        .unwrap();
    for tenant in [t.customer, t.team] {
        assert_eq!(
            h.resolve(tenant, "openai::gpt-4o").await.unwrap().id,
            c.root_gpt4o,
            "partner's tenant-mode provider is not a candidate"
        );
        assert_eq!(
            h.available(tenant).await,
            ids(&["openai::gpt-4o", "openai::gpt-4o-mini"])
        );
    }
    assert!(
        matches!(
            h.service
                .get_model(&ctx(t.customer), c.customer_gpt4o)
                .await,
            Err(DomainError::ModelNotFound { .. })
        ),
        "cascade"
    );
    assert!(
        h.managed_models(t.customer)
            .await
            .iter()
            .all(|m| m.inherited)
    );
}

#[tokio::test]
async fn sharing_mode_changes_apply_on_the_next_request() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;
    h.service
        .update_provider(
            &ctx(t.partner),
            c.partner_openai,
            UpdateProviderRequestV1 {
                sharing_mode: Some(SharingMode::Shared),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Customer still wins with its own provider; a tenant without one now
    // sees Partner's.
    assert_eq!(
        h.resolve(t.customer, "openai::gpt-4o").await.unwrap().id,
        c.customer_gpt4o
    );
    h.service
        .delete_provider(&ctx(t.customer), c.customer_openai)
        .await
        .unwrap();
    assert_eq!(
        h.resolve(t.team, "openai::gpt-4o").await.unwrap().id,
        c.partner_gpt4o
    );

    h.service
        .update_model(
            &ctx(t.partner),
            c.partner_gpt4o,
            UpdateModelRequestV1 {
                sharing_mode: Some(SharingMode::Tenant),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        h.resolve(t.team, "openai::gpt-4o").await,
        Err(DomainError::ModelNotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// Approval
// ---------------------------------------------------------------------------

#[tokio::test]
async fn only_approved_models_resolve_and_revoke_applies_on_the_next_call() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let subject = Uuid::new_v4();
    let created = h
        .service
        .create_model(
            &security_context_for(t.root, subject),
            model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Pending),
        )
        .await
        .unwrap();
    assert_eq!(created.model.approval.state, ApprovalState::Pending);
    assert_eq!(created.model.approval.decided_by, subject);
    assert_eq!(created.model.approval.decided_at, h.clock.now_for_test());
    assert!(!created.available_for_eval);
    let err = h.resolve(t.other, "openai::gpt-4o").await.unwrap_err();
    assert!(matches!(
        err,
        DomainError::ModelNotApproved {
            state: ApprovalState::Pending,
            ..
        }
    ));
    assert!(h.available(t.other).await.is_empty());

    let approver = Uuid::new_v4();
    h.clock.advance(chrono::Duration::minutes(5));
    let approved = h
        .service
        .update_model(
            &security_context_for(t.root, approver),
            created.model.id,
            UpdateModelRequestV1 {
                approval_state: Some(ApprovalState::Approved),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(approved.model.approval.decided_by, approver);
    assert_eq!(approved.model.approval.decided_at, h.clock.now_for_test());
    assert!(approved.available_for_eval);
    assert!(h.resolve(t.other, "openai::gpt-4o").await.is_ok());
    assert_eq!(h.available(t.other).await, ids(&["openai::gpt-4o"]));

    // Approval defaults to approved on create.
    let default_created = h
        .service
        .create_model(&ctx(t.root), {
            let mut r = model_req(
                p,
                "gpt-4o-mini",
                SharingMode::Shared,
                ApprovalState::Approved,
            );
            r.approval_state = None;
            r
        })
        .await
        .unwrap();
    assert_eq!(
        default_created.model.approval.state,
        ApprovalState::Approved
    );

    h.service
        .update_model(
            &ctx(t.root),
            created.model.id,
            UpdateModelRequestV1 {
                approval_state: Some(ApprovalState::Revoked),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        h.resolve(t.other, "openai::gpt-4o").await,
        Err(DomainError::ModelNotApproved {
            state: ApprovalState::Revoked,
            ..
        })
    ));
    assert_eq!(h.available(t.other).await, ids(&["openai::gpt-4o-mini"]));
    // The admin list still returns it, flagged.
    let listed = h.managed_models(t.other).await;
    let revoked = listed
        .iter()
        .find(|m| m.model.canonical_id == "openai::gpt-4o")
        .unwrap();
    assert!(
        revoked.inherited
            && !revoked.shadowed
            && revoked.provider_enabled
            && !revoked.available_for_eval
    );
}

// ---------------------------------------------------------------------------
// GTS checks
// ---------------------------------------------------------------------------

fn reasons(err: &DomainError) -> Vec<(String, field::Reason)> {
    match err {
        DomainError::Invalid(v) => v.iter().map(|v| (v.field.clone(), v.reason)).collect(),
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[tokio::test]
async fn provider_type_and_settings_are_validated_on_writes() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = ctx(t.root);

    for (gts_type, reason) in [
        ("not-an-id", field::Reason::InvalidGtsType),
        (
            "gts.cf.genai.model.provider.v1~",
            field::Reason::NotAProviderType,
        ),
        (
            "gts.cf.genai.model.provider.v1~cf.genai._.unknown.v1~",
            field::Reason::GtsTypeNotRegistered,
        ),
    ] {
        let req = CreateProviderRequestV1::builder("p", "P", gts::GtsTypeId::new(gts_type)).build();
        let err = h.service.create_provider(&c, req).await.unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![("gts_type".to_owned(), reason)],
            "{gts_type}"
        );
    }

    let strict = h
        .service
        .create_provider(
            &c,
            CreateProviderRequestV1::builder("strict", "Strict", gts::GtsTypeId::new(STRICT))
                .build(),
        )
        .await
        .unwrap()
        .provider;
    let mut req = model_req(
        strict.id,
        "m1",
        SharingMode::Tenant,
        ApprovalState::Approved,
    );
    req.provider_settings = serde_json::json!({ "cost": { "input": "high" }, "extra": 1 });
    let err = h.service.create_model(&c, req.clone()).await.unwrap_err();
    let r = reasons(&err);
    assert!(
        r.iter()
            .all(|(_, reason)| *reason == field::Reason::SchemaMismatch),
        "{r:?}"
    );
    assert!(
        r.iter().any(|(f, _)| f == "provider_settings/cost/input"),
        "{r:?}"
    );
    assert!(r.iter().any(|(f, _)| f == "provider_settings"), "{r:?}");

    req.provider_settings =
        serde_json::json!({ "oagw_alias": "strict-prod", "cost": { "input": 0.5 } });
    let created = h.service.create_model(&c, req).await.unwrap();
    assert_eq!(
        created.model.info.gts_type.as_ref(),
        STRICT,
        "gts_type is copied from the provider"
    );
    assert_eq!(created.model.canonical_id, "strict::m1");

    let err = h
        .service
        .update_model(
            &c,
            created.model.id,
            UpdateModelRequestV1 {
                provider_settings: Some(serde_json::json!({})),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(reasons(&err)[0].1, field::Reason::SchemaMismatch);
    let calls_before = h.schemas.calls();
    h.service
        .update_model(
            &c,
            created.model.id,
            UpdateModelRequestV1 {
                display_name: Some("renamed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        h.schemas.calls(),
        calls_before,
        "no schema fetch without provider_settings"
    );
    assert!(h.resolve(t.root, "strict::m1").await.is_ok());
}

#[tokio::test]
async fn request_field_checks_and_duplicates() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = ctx(t.root);
    let err = h
        .service
        .create_provider(&c, provider_req("Bad Slug", SharingMode::Tenant, true))
        .await
        .unwrap_err();
    assert_eq!(
        reasons(&err)[0],
        ("slug".to_owned(), field::Reason::InvalidFormat)
    );

    let p = h
        .provider(t.root, "openai", SharingMode::Tenant, true)
        .await;
    let err = h
        .service
        .create_provider(&c, provider_req("openai", SharingMode::Shared, true))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::ProviderAlreadyExists { ref slug } if slug == "openai"));

    h.model(
        t.root,
        p,
        "gpt-4o",
        SharingMode::Tenant,
        ApprovalState::Approved,
    )
    .await;
    let err = h
        .service
        .create_model(
            &c,
            model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Pending),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::ModelAlreadyExists { ref canonical_id } if canonical_id == "openai::gpt-4o")
    );

    let mut req = model_req(p, "bad id", SharingMode::Tenant, ApprovalState::Approved);
    req.base_url = Some("ftp://x".into());
    let err = h.service.create_model(&c, req).await.unwrap_err();
    let r = reasons(&err);
    assert!(r.contains(&("provider_model_id".to_owned(), field::Reason::InvalidFormat)));
    assert!(r.contains(&("base_url".to_owned(), field::Reason::InvalidFormat)));

    let err = h
        .service
        .create_model(
            &c,
            model_req(
                Uuid::new_v4(),
                "x",
                SharingMode::Tenant,
                ApprovalState::Approved,
            ),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::ProviderNotFound { .. }));
}

// ---------------------------------------------------------------------------
// Isolation and management reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn isolation_matrix_over_siblings_ancestors_and_descendants() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;

    // Siblings see nothing of each other.
    assert!(matches!(
        h.service
            .get_provider(&ctx(t.other), c.partner_openai)
            .await,
        Err(DomainError::ProviderNotFound { .. })
    ));
    assert!(matches!(
        h.service.get_model(&ctx(t.other), c.partner_gpt4o).await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert!(matches!(
        h.service
            .update_provider(
                &ctx(t.other),
                c.partner_openai,
                UpdateProviderRequestV1::default()
            )
            .await,
        Err(DomainError::ProviderNotFound { .. })
    ));

    // Ancestors never see descendant rows.
    assert_eq!(h.managed_models(t.root).await.len(), 3);
    assert!(matches!(
        h.service.get_model(&ctx(t.root), c.customer_gpt4o).await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert!(matches!(
        h.service.delete_model(&ctx(t.root), c.customer_gpt4o).await,
        Err(DomainError::ModelNotFound { .. })
    ));

    // Descendants read inherited shared rows but cannot write them.
    let inherited = h
        .service
        .get_provider(&ctx(t.team), c.root_openai)
        .await
        .unwrap();
    assert!(inherited.inherited && inherited.shadowed && !inherited.available_for_eval);
    assert_eq!(inherited.provider.name, "OPENAI", "full record");
    for err in [
        h.service
            .update_provider(
                &ctx(t.team),
                c.root_openai,
                UpdateProviderRequestV1::default(),
            )
            .await
            .err(),
        h.service
            .delete_provider(&ctx(t.team), c.root_openai)
            .await
            .err(),
        h.service
            .update_model(
                &ctx(t.team),
                c.root_gpt4o,
                UpdateModelRequestV1 {
                    approval_state: Some(ApprovalState::Revoked),
                    ..Default::default()
                },
            )
            .await
            .err(),
        h.service
            .delete_model(&ctx(t.team), c.root_gpt4o)
            .await
            .err(),
        h.service
            .create_model(
                &ctx(t.team),
                model_req(
                    c.root_openai,
                    "new",
                    SharingMode::Tenant,
                    ApprovalState::Approved,
                ),
            )
            .await
            .err(),
    ] {
        assert!(
            matches!(err, Some(DomainError::InheritedReadOnly { .. })),
            "{err:?}"
        );
    }
    // An ancestor's tenant-mode rows are not visible at all.
    assert!(matches!(
        h.service
            .get_provider(&ctx(t.customer), c.partner_openai)
            .await,
        Err(DomainError::ProviderNotFound { .. })
    ));
    assert!(matches!(
        h.service.get_model(&ctx(t.other), c.root_o1).await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert!(matches!(
        h.service
            .delete_provider(&ctx(t.customer), c.partner_openai)
            .await,
        Err(DomainError::ProviderNotFound { .. })
    ));
    assert_eq!(
        h.resolve(t.root, "openai::gpt-4o")
            .await
            .unwrap()
            .approval
            .state,
        ApprovalState::Approved,
        "root's row is untouched"
    );
}

#[tokio::test]
async fn management_lists_include_shadowed_candidates_with_flags() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;

    let providers = h
        .service
        .list_providers(&ctx(t.team), &ODataQuery::default())
        .await
        .unwrap()
        .items;
    let by_id = |id: Uuid| providers.iter().find(|p| p.provider.id == id).unwrap();
    assert_eq!(
        providers.len(),
        2,
        "partner's tenant-mode provider is excluded"
    );
    let customer = by_id(c.customer_openai);
    assert!(customer.inherited && !customer.shadowed && customer.available_for_eval);
    let root = by_id(c.root_openai);
    assert!(root.inherited && root.shadowed && !root.available_for_eval);

    let models = h.managed_models(t.team).await;
    let names: BTreeSet<(String, bool)> = models
        .iter()
        .map(|m| (m.model.canonical_id.clone(), m.shadowed))
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            ("openai::gpt-4o".to_owned(), false),
            ("openai::gpt-4o".to_owned(), true),
            ("openai::gpt-4o-mini".to_owned(), true),
        ]),
        "o1-preview is tenant mode, partner's gpt-4o has a tenant-mode provider"
    );
    let own = h.managed_models(t.customer).await;
    let mine = own.iter().find(|m| m.model.id == c.customer_gpt4o).unwrap();
    assert!(!mine.inherited && !mine.shadowed && mine.available_for_eval);

    // The root admin sees its own rows only, in every state.
    let pending = h
        .model(
            t.root,
            c.root_openai,
            "pending",
            SharingMode::Tenant,
            ApprovalState::Pending,
        )
        .await;
    let root_models = h.managed_models(t.root).await;
    assert_eq!(root_models.len(), 4);
    let p = root_models.iter().find(|m| m.model.id == pending).unwrap();
    assert!(!p.available_for_eval && !p.inherited);
    assert_eq!(
        h.service
            .get_model(&ctx(t.root), pending)
            .await
            .unwrap()
            .model
            .approval
            .state,
        ApprovalState::Pending
    );
}

// ---------------------------------------------------------------------------
// Authorization and fail-closed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pdp_outcomes_per_operation() {
    let t = Tenants::new();
    // Permit everything except what the test flips through a shared switch.
    let denied = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let denied_in = denied.clone();
    let authz = StaticAuthz::with(move |req| {
        if denied_in.lock().unwrap().contains(&req.action.name) {
            Ok(deny())
        } else {
            Ok(permit_with(vec![own_tenant_constraint(req)]))
        }
    });
    let h = harness_with(
        t.resolver(),
        authz,
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await;
    let c = seed(&h, &t, true).await;

    *denied.lock().unwrap() = vec!["evaluate".into()];
    assert!(matches!(
        h.resolve(t.root, "openai::gpt-4o").await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(
        h.available(t.root).await.is_empty(),
        "evaluate denial gives an empty page"
    );
    assert!(
        h.service
            .get_model(&ctx(t.root), c.root_gpt4o)
            .await
            .is_ok(),
        "management read unaffected"
    );

    *denied.lock().unwrap() = vec!["list_available".into()];
    assert!(matches!(
        h.service
            .list_available_models(&ctx(t.root), &ODataQuery::default())
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(h.resolve(t.root, "openai::gpt-4o").await.is_ok());

    *denied.lock().unwrap() = vec![
        "list".into(),
        "get".into(),
        "update".into(),
        "delete".into(),
        "create".into(),
    ];
    assert!(matches!(
        h.service
            .list_models(&ctx(t.root), &ODataQuery::default())
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service
            .list_providers(&ctx(t.root), &ODataQuery::default())
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service.get_provider(&ctx(t.root), c.root_openai).await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service.delete_model(&ctx(t.root), c.root_gpt4o).await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service
            .create_provider(&ctx(t.root), provider_req("x", SharingMode::Tenant, true))
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(
        h.resolve(t.root, "openai::gpt-4o").await.is_ok(),
        "evaluation unaffected"
    );

    // Catalog reasons come before the PDP on resolve.
    *denied.lock().unwrap() = vec!["evaluate".into()];
    assert!(matches!(
        h.resolve(t.other, "openai::o1-preview").await,
        Err(DomainError::ModelNotFound { .. })
    ));
    let actions = h.authz.actions();
    assert_eq!(
        actions.last().map(String::as_str),
        Some("evaluate"),
        "{actions:?}"
    );
}

#[tokio::test]
async fn pdp_row_constraints_admit_matching_rows() {
    let t = Tenants::new();
    let allowed = Arc::new(std::sync::Mutex::new(Uuid::nil()));
    let allowed_in = allowed.clone();
    let authz = StaticAuthz::with(move |req| {
        let id = *allowed_in.lock().unwrap();
        let mut predicates = vec![Predicate::Eq(EqPredicate::new(
            "owner_tenant_id",
            subject_tenant(req),
        ))];
        if id != Uuid::nil() {
            predicates.push(Predicate::In(InPredicate::new("provider_id", [id])));
        }
        Ok(permit_with(vec![Constraint { predicates }]))
    });
    let h = harness_with(
        t.resolver(),
        authz,
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await;
    let openai = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let anthropic = h
        .provider(t.root, "anthropic", SharingMode::Shared, true)
        .await;
    let gpt = h
        .model(
            t.root,
            openai,
            "gpt-4o",
            SharingMode::Shared,
            ApprovalState::Approved,
        )
        .await;
    let claude = h
        .model(
            t.root,
            anthropic,
            "claude",
            SharingMode::Shared,
            ApprovalState::Approved,
        )
        .await;
    *allowed.lock().unwrap() = openai;

    assert_eq!(h.resolve(t.other, "openai::gpt-4o").await.unwrap().id, gpt);
    assert!(matches!(
        h.resolve(t.other, "anthropic::claude").await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert_eq!(h.available(t.other).await, ids(&["openai::gpt-4o"]));
    let listed: Vec<Uuid> = h
        .managed_models(t.other)
        .await
        .into_iter()
        .map(|m| m.model.id)
        .collect();
    assert_eq!(listed, vec![gpt]);
    assert!(h.service.get_model(&ctx(t.root), gpt).await.is_ok());
    assert!(matches!(
        h.service.get_model(&ctx(t.root), claude).await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service.delete_model(&ctx(t.root), claude).await,
        Err(DomainError::NotPermitted { .. })
    ));
    // Create checks the row constraints against the new record.
    assert!(matches!(
        h.service
            .create_model(
                &ctx(t.root),
                model_req(
                    anthropic,
                    "claude-2",
                    SharingMode::Shared,
                    ApprovalState::Approved
                )
            )
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(
        h.service
            .create_model(
                &ctx(t.root),
                model_req(
                    openai,
                    "gpt-4o-mini",
                    SharingMode::Shared,
                    ApprovalState::Approved
                )
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn every_dependency_fails_closed() {
    let t = Tenants::new();

    // PDP down: every operation is dependency unavailable, never a grant.
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let down_in = down.clone();
    let authz = StaticAuthz::with(move |req| {
        if down_in.load(std::sync::atomic::Ordering::SeqCst) {
            Err(toolkit_canonical_errors::CanonicalError::service_unavailable().create())
        } else {
            Ok(permit_with(vec![own_tenant_constraint(req)]))
        }
    });
    let h = harness_with(
        t.resolver(),
        authz,
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await;
    seed(&h, &t, true).await;
    down.store(true, std::sync::atomic::Ordering::SeqCst);
    for err in [
        h.resolve(t.root, "openai::gpt-4o").await.err(),
        h.service
            .list_available_models(&ctx(t.root), &ODataQuery::default())
            .await
            .err(),
        h.service
            .list_models(&ctx(t.root), &ODataQuery::default())
            .await
            .err(),
        h.service
            .create_provider(&ctx(t.root), provider_req("x", SharingMode::Tenant, true))
            .await
            .err(),
    ] {
        assert!(
            matches!(
                err,
                Some(DomainError::DependencyUnavailable {
                    dependency: Dependency::Pdp,
                    ..
                })
            ),
            "{err:?}"
        );
    }

    // PDP slow past the deadline.
    let cfg = ModelRegistryConfig {
        pdp_deadline_ms: 20,
        ..Default::default()
    };
    let h = harness_with(
        t.resolver(),
        StaticAuthz::permit_own_tenant().with_delay(Duration::from_millis(200)),
        schemas(),
        cfg,
    )
    .await;
    assert!(matches!(
        h.service
            .list_models(&ctx(t.root), &ODataQuery::default())
            .await,
        Err(DomainError::DependencyUnavailable {
            dependency: Dependency::Pdp,
            ..
        })
    ));

    // Tenant Resolver down, and refusing the caller's tenant.
    let h = harness(
        StaticTenantResolver::new()
            .failing_with(|| tenant_resolver_sdk::TenantResolverError::Internal("x".into())),
    )
    .await;
    assert!(matches!(
        h.resolve(t.root, "openai::gpt-4o").await,
        Err(DomainError::DependencyUnavailable {
            dependency: Dependency::TenantResolver,
            ..
        })
    ));
    let h = harness(StaticTenantResolver::new()).await; // unknown tenant → TenantNotFound
    assert!(matches!(
        h.resolve(t.root, "openai::gpt-4o").await,
        Err(DomainError::NotPermitted { .. })
    ));

    // Types Registry down: writes that need it fail, reads are unaffected.
    let h = harness_with(
        t.resolver(),
        StaticAuthz::permit_own_tenant(),
        StaticSchemaSource::new()
            .failing_with(|| DomainError::unavailable(Dependency::TypesRegistry, "down")),
        ModelRegistryConfig::default(),
    )
    .await;
    assert!(matches!(
        h.service
            .create_provider(
                &ctx(t.root),
                provider_req("openai", SharingMode::Shared, true)
            )
            .await,
        Err(DomainError::DependencyUnavailable {
            dependency: Dependency::TypesRegistry,
            ..
        })
    ));
    assert!(matches!(
        h.resolve(t.root, "openai::gpt-4o").await,
        Err(DomainError::ModelNotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// OData
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filters_only_narrow_and_orderby_follows_the_field_rules() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    for (pmid, state, vendor) in [
        ("a", ApprovalState::Approved, "OpenAI"),
        ("b", ApprovalState::Pending, "OpenAI"),
        ("c", ApprovalState::Approved, "Other"),
    ] {
        let mut req = model_req(p, pmid, SharingMode::Shared, state);
        req.vendor = Some(vendor.into());
        h.service.create_model(&ctx(t.root), req).await.unwrap();
    }
    let c = ctx(t.other);

    // A caller filter cannot switch the approval predicate off.
    let q = QueryBuilder::<ModelSchema>::new()
        .filter(MODEL_APPROVAL_STATE.eq(ApprovalState::Pending))
        .build();
    assert!(
        h.service
            .list_available_models(&c, &q)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // The admin list does return it under the same filter.
    assert_eq!(h.service.list_models(&c, &q).await.unwrap().items.len(), 1);

    let q = QueryBuilder::<ModelSchema>::new()
        .filter(MODEL_VENDOR.eq("Other"))
        .build();
    let items = h.service.list_available_models(&c, &q).await.unwrap().items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].canonical_id, "openai::c");

    // Nullable fields are filter-only.
    let q = QueryBuilder::<ModelSchema>::new()
        .order_by(MODEL_VENDOR, SortDir::Asc)
        .build();
    assert!(matches!(
        h.service.list_available_models(&c, &q).await,
        Err(DomainError::OData(_))
    ));
    let q = QueryBuilder::<ModelSchema>::new()
        .order_by(MODEL_CANONICAL_ID, SortDir::Desc)
        .build();
    let items = h.service.list_available_models(&c, &q).await.unwrap().items;
    assert_eq!(
        items
            .iter()
            .map(|m| m.canonical_id.as_str())
            .collect::<Vec<_>>(),
        vec!["openai::c", "openai::a"]
    );

    // Keyset paging in the default order with the id tie-breaker.
    let mut query = ODataQuery::default().with_limit(1);
    let mut seen = Vec::new();
    loop {
        let page = h.service.list_models(&c, &query).await.unwrap();
        assert_eq!(page.page_info.limit, 1);
        seen.extend(page.items.into_iter().map(|m| m.model.canonical_id));
        let Some(next) = page.page_info.next_cursor else {
            break;
        };
        query = ODataQuery::default()
            .with_limit(1)
            .with_cursor(toolkit_odata::CursorV1::decode(&next).unwrap());
    }
    assert_eq!(seen, vec!["openai::a", "openai::b", "openai::c"]);

    // Page size bounds.
    let cfg = ModelRegistryConfig {
        default_page_size: 2,
        max_page_size: 2,
        ..Default::default()
    };
    let h2 = harness_with(
        t.resolver(),
        StaticAuthz::permit_own_tenant(),
        schemas(),
        cfg,
    )
    .await;
    let page = h2
        .service
        .list_models(&ctx(t.root), &ODataQuery::default().with_limit(50))
        .await
        .unwrap();
    assert_eq!(page.page_info.limit, 2);
    let empty = h2
        .service
        .list_available_models(&ctx(t.root), &ODataQuery::default().with_limit(50))
        .await
        .unwrap();
    assert_eq!(
        empty.page_info.limit, 2,
        "an empty page reports the clamped limit"
    );
}

#[tokio::test]
async fn available_list_equals_the_set_of_resolvable_models() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;
    h.model(
        t.root,
        c.root_openai,
        "pending",
        SharingMode::Shared,
        ApprovalState::Pending,
    )
    .await;
    let anthropic = h
        .provider(t.partner, "anthropic", SharingMode::Shared, false)
        .await;
    h.model(
        t.partner,
        anthropic,
        "claude",
        SharingMode::Shared,
        ApprovalState::Approved,
    )
    .await;
    let all_ids = [
        "openai::gpt-4o",
        "openai::gpt-4o-mini",
        "openai::o1-preview",
        "openai::pending",
        "anthropic::claude",
    ];
    for tenant in t.all() {
        let mut resolvable = BTreeSet::new();
        for id in all_ids {
            if h.resolve(tenant, id).await.is_ok() {
                resolvable.insert(id.to_owned());
            }
        }
        assert_eq!(h.available(tenant).await, resolvable, "tenant {tenant}");
    }
}

// ---------------------------------------------------------------------------
// Chain cache, audit, updates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chain_cache_serves_the_warm_path_with_barrier_ignore() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;
    h.resolve(t.team, "openai::gpt-4o").await.unwrap();
    let calls = h.resolver.calls();
    h.resolve(t.team, "openai::gpt-4o").await.unwrap();
    h.available(t.team).await;
    h.managed_models(t.team).await;
    assert_eq!(
        h.resolver.calls(),
        calls,
        "warm chain makes no resolver call"
    );
    assert_eq!(h.resolver.last_barrier_mode(), Some(BarrierMode::Ignore));

    h.clock.advance(chrono::Duration::seconds(301));
    h.resolve(t.team, "openai::gpt-4o").await.unwrap();
    assert_eq!(h.resolver.calls(), calls + 1, "expired entry is refreshed");
    let _ = c;

    let cfg = ModelRegistryConfig {
        chain_cache_ttl_seconds: 0,
        ..Default::default()
    };
    let h = harness_with(
        t.resolver(),
        StaticAuthz::permit_own_tenant(),
        schemas(),
        cfg,
    )
    .await;
    let before = h.resolver.calls();
    h.available(t.root).await;
    h.available(t.root).await;
    assert_eq!(h.resolver.calls(), before + 2, "TTL 0 disables the cache");
}

#[tokio::test]
async fn audit_columns_use_the_clock_and_the_subject() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let creator = Uuid::new_v4();
    let created = h
        .service
        .create_provider(
            &security_context_for(t.root, creator),
            provider_req("openai", SharingMode::Tenant, true),
        )
        .await
        .unwrap()
        .provider;
    let t0 = h.clock.now_for_test();
    assert_eq!((created.created_by, created.updated_by), (creator, creator));
    assert_eq!((created.created_at, created.updated_at), (t0, t0));

    let editor = Uuid::new_v4();
    h.clock.advance(chrono::Duration::hours(1));
    let updated = h
        .service
        .update_provider(
            &security_context_for(t.root, editor),
            created.id,
            UpdateProviderRequestV1 {
                name: Some("Renamed".into()),
                metadata: Some(Some(serde_json::json!({ "k": 1 }))),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .provider;
    assert_eq!(updated.name, "Renamed");
    assert_eq!(updated.metadata, Some(serde_json::json!({ "k": 1 })));
    assert_eq!((updated.created_by, updated.created_at), (creator, t0));
    assert_eq!(
        (updated.updated_by, updated.updated_at),
        (editor, h.clock.now_for_test())
    );
    assert_eq!(updated.slug, "openai", "immutable");

    let cleared = h
        .service
        .update_provider(
            &ctx(t.root),
            created.id,
            UpdateProviderRequestV1 {
                metadata: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .provider;
    assert_eq!(cleared.metadata, None);
    assert_eq!(cleared.name, "Renamed", "absent fields stay");
}

#[tokio::test]
async fn model_update_replaces_whole_values_and_keeps_promoted_columns_in_sync() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let mut req = model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Approved);
    req.capabilities.streaming = true;
    req.capabilities.vision.enabled = true;
    req.base_url = Some("https://api.openai.com/v1".into());
    req.secret_ref = Some("openai-key".into());
    let id = h
        .service
        .create_model(&ctx(t.root), req)
        .await
        .unwrap()
        .model
        .id;
    let c = ctx(t.other);

    let q = QueryBuilder::<ModelSchema>::new()
        .filter(model_registry_sdk::odata::MODEL_STREAMING.eq(true))
        .build();
    assert_eq!(
        h.service
            .list_available_models(&c, &q)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let resolved = h.resolve(t.other, "openai::gpt-4o").await.unwrap();
    assert_eq!(
        resolved.info.base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(resolved.info.secret_ref.as_deref(), Some("openai-key"));

    let mut disabled = model_registry_sdk::DisabledCapabilities::none();
    disabled.streaming = true;
    let updated = h
        .service
        .update_model(
            &ctx(t.root),
            id,
            UpdateModelRequestV1 {
                disabled_capabilities: Some(disabled),
                base_url: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(updated.model.info.capabilities.streaming, "declared stays");
    assert!(updated.model.info.disabled_capabilities.streaming);
    assert_eq!(updated.model.info.base_url, None);
    assert_eq!(updated.model.info.secret_ref.as_deref(), Some("openai-key"));
    assert!(
        h.service
            .list_available_models(&c, &q)
            .await
            .unwrap()
            .items
            .is_empty(),
        "effective capability is filtered"
    );
    let q = QueryBuilder::<ModelSchema>::new()
        .filter(model_registry_sdk::odata::MODEL_VISION.eq(true))
        .build();
    assert_eq!(
        h.service
            .list_available_models(&c, &q)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        h.resolve(t.other, "openai::gpt-4o")
            .await
            .unwrap()
            .info
            .base_url,
        None,
        "absent stays absent"
    );
}

trait ClockForTest {
    fn now_for_test(&self) -> chrono::DateTime<chrono::Utc>;
}

impl ClockForTest for ManualClock {
    fn now_for_test(&self) -> chrono::DateTime<chrono::Utc> {
        model_registry::domain::clock::Clock::now(self)
    }
}
