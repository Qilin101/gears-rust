//! `SeaORM`-backed implementation of [`ModelRepository`].
//!
//! Cross-entity read: [`Self::create`] queries `provider::Entity` to verify the
//! referenced provider exists before inserting a model.

use async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Set};
use toolkit_db::odata::sea_orm_filter::{PaginateOdataTryError, paginate_odata_try};
use toolkit_db::secure::{DBRunner, SecureEntityExt, secure_update_with_scope};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::config::{ModelRegistryConfig, PageLimits};
use crate::domain::error::DomainError;
use crate::domain::repo::{ListVisibility, ModelRepository};
use crate::{ApprovalStatus, CreateModelRequestV1, ModelV1, UpdateModelRequestV1};

use super::entity::{self, model, provider};
use super::error_mapping::map_scope_error;
use super::model_mapper;
use super::model_odata_mapper::ModelODataMapper;
use model_registry_sdk::odata::ModelFilterField;

// =============================================================================
// ModelRepositoryImpl — holds the configured pagination bounds
// =============================================================================

/// `SeaORM`-backed [`ModelRepository`] implementation.
///
/// Carries only the pagination bounds from [`ModelRegistryConfig`]; all database
/// interactions go through the [`DBRunner`] connection passed per-call, ensuring
/// transactional boundaries are caller-controlled.
#[derive(Debug, Clone, Copy)]
pub struct ModelRepositoryImpl {
    limits: PageLimits,
}

impl ModelRepositoryImpl {
    #[must_use]
    pub fn new(limits: PageLimits) -> Self {
        Self { limits }
    }
}

impl Default for ModelRepositoryImpl {
    /// Pagination bounds from [`ModelRegistryConfig::default`].
    fn default() -> Self {
        Self::new(ModelRegistryConfig::default().page_limits())
    }
}

#[async_trait]
impl ModelRepository for ModelRepositoryImpl {
    async fn find_by_canonical(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        canonical_id: &str,
    ) -> Result<ModelV1, DomainError> {
        let entity = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::CanonicalId.eq(canonical_id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::model_not_found(canonical_id))?;

        model_mapper::model_entity_to_v1(entity)
    }

    async fn list(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        query: &ODataQuery,
        visibility: ListVisibility<'_>,
    ) -> Result<Page<ModelV1>, DomainError> {
        let mut base = model::Entity::find().secure().scope_with(scope);

        // ── Eval path ──────────────────────────────────────────────────────
        // Mandatory predicates: allow-list membership + unconditional lifecycle
        // and approval exclusions (DESIGN §3.3). The OData `$filter` is ANDed on
        // top, so it can only narrow within this set — neither
        // `$filter=lifecycle_status eq 'deprecated'` nor
        // `$filter=approval_status eq 'pending'` can escape them.
        if let ListVisibility::Eval { allow_list } = visibility {
            base = base.filter(
                Condition::all()
                    .add(model::Column::ProviderId.is_in(allow_list.to_vec()))
                    .add(model::Column::LifecycleStatus.ne("deprecated"))
                    .add(model::Column::LifecycleStatus.ne("sunset"))
                    .add(model::Column::ApprovalStatus.eq(ApprovalStatus::Approved.as_str())),
            );
        }

        // ── Management path ────────────────────────────────────────────────
        // No mandatory predicates beyond the optional deprecated/sunset exclusion.
        if let ListVisibility::Management { include_deprecated } = visibility
            && !include_deprecated
        {
            base = base.filter(
                Condition::all()
                    .add(model::Column::LifecycleStatus.ne("deprecated"))
                    .add(model::Column::LifecycleStatus.ne("sunset")),
            );
        }

        // `paginate_odata_try` because `model_entity_to_v1` is fallible — a row
        // with an out-of-domain enum string surfaces as `DomainError::Internal`
        // rather than panicking the worker.
        let page = paginate_odata_try::<
            ModelFilterField,
            ModelODataMapper,
            model::Entity,
            ModelV1,
            _,
            _,
            _,
        >(
            base,
            conn,
            query,
            ("canonical_id", SortDir::Asc),
            self.limits.limit_cfg(),
            model_mapper::model_entity_to_v1,
        )
        .await
        .map_err(|e| match e {
            PaginateOdataTryError::OData(odata_err) => {
                DomainError::internal(format!("OData pagination failed: {odata_err}"))
            }
            PaginateOdataTryError::MapError(domain_err) => domain_err,
        })?;

        Ok(page)
    }

    async fn create(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        req: &CreateModelRequestV1,
    ) -> Result<ModelV1, DomainError> {
        let canonical_id = format!("{}::{}", req.provider_slug, req.info.provider_model_id);

        // Check for duplicate canonical_id within the tenant scope.
        let existing = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::CanonicalId.eq(&canonical_id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        if existing.is_some() {
            return Err(DomainError::validation(format!(
                "model with canonical_id `{canonical_id}` already exists \
                 (duplicate provider_slug + provider_model_id)"
            )));
        }

        // Verify provider exists within scope.
        let provider_entity = provider::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(provider::Column::Slug.eq(&req.provider_slug)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::validation(format!(
                "provider with slug `{}` not found",
                req.provider_slug,
            )))?;
        // ^^ NOTE: Validation (400) is used here but the service-layer
        // pre-check (`create_model`) resolves the provider own-tenant-only
        // first and returns ProviderNotFoundBySlug (404) / ProviderNotOwned
        // (403). This repository path is TOCTOU-only — reachable only when
        // the provider is deleted between the pre-check and this query.
        // Keeping Validation rather than adding a new error variant avoids
        // coupling the repository to a domain concept ("ownership check")
        // it does not implement.

        let initial_approval = req.approval_status.unwrap_or(ApprovalStatus::Pending);
        let am = model_mapper::model_create_active_model(
            tenant_id,
            provider_entity.id,
            req,
            initial_approval,
        );

        let entity = toolkit_db::secure::secure_insert::<model::Entity>(am, scope, conn)
            .await
            .map_err(map_scope_error)?;

        model_mapper::model_entity_to_v1(entity)
    }

    async fn update(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        canonical_id: &str,
        req: &UpdateModelRequestV1,
    ) -> Result<ModelV1, DomainError> {
        // Fetch existing entity (scope-checked).
        let existing = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::CanonicalId.eq(canonical_id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::model_not_found(canonical_id))?;

        // Build the patched ActiveModel via the mapper (PATCH semantics).
        let am = model_mapper::model_update_active_model(&existing, req)?;

        let model_id = existing.id;

        // Execute update using secure_update_with_scope.
        let updated = secure_update_with_scope::<model::Entity>(am, scope, model_id, conn)
            .await
            .map_err(map_scope_error)?;

        model_mapper::model_entity_to_v1(updated)
    }

    async fn soft_delete(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        canonical_id: &str,
    ) -> Result<ModelV1, DomainError> {
        // Fetch existing entity (scope-checked).
        let existing = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::CanonicalId.eq(canonical_id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::model_not_found(canonical_id))?;

        let model_id = existing.id;
        let mut am: entity::model::ActiveModel = existing.into();
        am.lifecycle_status = Set("deprecated".to_owned());
        let now = chrono::Utc::now();
        am.deprecated_at = Set(Some(now));
        am.updated_at = Set(now);

        let updated = secure_update_with_scope::<model::Entity>(am, scope, model_id, conn)
            .await
            .map_err(map_scope_error)?;

        model_mapper::model_entity_to_v1(updated)
    }
}

#[cfg(test)]
mod tests {
    // NOTE: setup_provider, test_tenant, other_tenant, scope_for are duplicated
    // from provider_repo::tests to keep this test module self-contained.
    use super::*;
    use model_registry_sdk::models::{
        ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities, DisabledMediaCapability,
        DisabledReasoningCapability, DisabledWebSearchCapability, MediaCapability,
        ModelCapabilities, ModelPerformance, ReasoningCapability, SupportedApi,
        WebSearchCapability,
    };
    use sea_orm_migration::MigratorTrait;
    use std::collections::{HashMap, HashSet};
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};

    use crate::domain::repo::{ListVisibility, ProviderRepository};
    use crate::infra::storage::migrations::Migrator;
    use crate::infra::storage::provider_repo::ProviderRepositoryImpl;

    async fn setup_provider() -> DBProvider<DbError> {
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

    fn test_tenant() -> Uuid {
        Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
    }

    fn other_tenant() -> Uuid {
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
    }

    fn scope_for(tenant_id: Uuid) -> AccessScope {
        AccessScope::for_tenants(vec![tenant_id])
    }

    /// Helper: create a provider for model tests.
    async fn create_test_provider(
        repo: &ProviderRepositoryImpl,
        conn: &impl DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        slug: &str,
    ) -> (Uuid, String) {
        let gts = gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~");
        let req = crate::CreateProviderRequestV1::builder(slug, slug, gts).build();
        let p = ProviderRepository::create(repo, conn, scope, tenant_id, &req)
            .await
            .expect("create test provider");
        (p.id, p.slug)
    }

    /// Helper: create a model for test purposes.
    /// Builds `ModelInfoV1` directly via struct literal — the SDK entity/info
    /// structs are not `#[non_exhaustive]`, so no JSON round-trip is needed.
    fn make_create_model_req(provider_slug: &str, provider_model_id: &str) -> CreateModelRequestV1 {
        let gts_leaf = "cf.genai._.openai.v1~";
        let gts_type = format!("gts.cf.genai.model.info.v1~{gts_leaf}");

        let info = model_registry_sdk::ModelInfoV1 {
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
            lifecycle_status: crate::LifecycleStatus::Production,
            approval_status: None,
            info,
        }
    }

    // =======================================================================
    // create — models
    // =======================================================================

    #[tokio::test]
    async fn model_create_stores_model() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let model = ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("create model");

        assert_eq!(model.canonical_id, "openai::gpt-4o");
        assert_eq!(model.lifecycle_status, crate::LifecycleStatus::Production);
        assert_eq!(model.approval_status, crate::ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn model_create_with_initial_approval() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        let model = ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("create model");

        assert_eq!(model.approval_status, crate::ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn model_create_duplicate_canonical_id_rejected() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("first create");

        // Second create with same canonical_id must fail.
        let err = ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
            .await
            .expect_err("duplicate canonical_id should be rejected");
        assert!(
            matches!(&err, DomainError::Validation { .. }),
            "expected Validation for duplicate, got {err:?}"
        );
    }

    #[tokio::test]
    async fn model_create_provider_not_found() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let req = make_create_model_req("nonexistent-provider", "gpt-4o");
        let err = ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
            .await
            .expect_err("nonexistent provider should be rejected");
        assert!(
            matches!(&err, DomainError::Validation { .. }),
            "expected Validation for missing provider, got {err:?}"
        );
    }

    // =======================================================================
    // find_by_canonical
    // =======================================================================

    #[tokio::test]
    async fn model_find_by_canonical_returns_model() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let created = ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("create model");

        let found =
            ModelRepository::find_by_canonical(&model_repo, &conn, &scope, "openai::gpt-4o")
                .await
                .expect("find_by_canonical");
        assert_eq!(found.id, created.id);
        assert_eq!(found.canonical_id, "openai::gpt-4o");
    }

    #[tokio::test]
    async fn model_find_by_canonical_not_found() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let model_repo = ModelRepositoryImpl::default();
        let scope = scope_for(test_tenant());

        let err =
            ModelRepository::find_by_canonical(&model_repo, &conn, &scope, "nonexistent::model")
                .await
                .expect_err("should return not found");

        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn model_find_by_canonical_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let (_provider_id, provider_slug) = create_test_provider(
            &provider_repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            "openai",
        )
        .await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let _created =
            ModelRepository::create(&model_repo, &conn, &scope_for(tenant_a), tenant_a, &req)
                .await
                .expect("create model");

        // Other tenant should not see this model.
        let err = ModelRepository::find_by_canonical(
            &model_repo,
            &conn,
            &scope_for(tenant_b),
            "openai::gpt-4o",
        )
        .await
        .expect_err("should be not found for other tenant");

        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound, got {err:?}"
        );
    }

    // =======================================================================
    // update — models
    // =======================================================================

    #[tokio::test]
    async fn model_update_changes_fields() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let _created = ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        let updated = ModelRepository::update(
            &model_repo,
            &conn,
            &scope,
            "openai::gpt-4o",
            &UpdateModelRequestV1 {
                lifecycle_status: Some(crate::LifecycleStatus::Preview),
                ..Default::default()
            },
        )
        .await
        .expect("update should succeed");

        assert_eq!(updated.lifecycle_status, crate::LifecycleStatus::Preview);
        // Canonical ID must remain unchanged.
        assert_eq!(updated.canonical_id, "openai::gpt-4o");
    }

    #[tokio::test]
    async fn model_update_not_found() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let model_repo = ModelRepositoryImpl::default();
        let scope = scope_for(test_tenant());

        let err = ModelRepository::update(
            &model_repo,
            &conn,
            &scope,
            "nonexistent::model",
            &UpdateModelRequestV1 {
                lifecycle_status: Some(crate::LifecycleStatus::Preview),
                ..Default::default()
            },
        )
        .await
        .expect_err("should return not found");

        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound, got {err:?}"
        );
    }

    // =======================================================================
    // soft_delete
    // =======================================================================

    #[tokio::test]
    async fn model_soft_delete_sets_deprecated() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let _created = ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::soft_delete(&model_repo, &conn, &scope, "openai::gpt-4o")
            .await
            .expect("soft delete should succeed");

        // After soft-delete, direct fetch should show deprecated.
        let found =
            ModelRepository::find_by_canonical(&model_repo, &conn, &scope, "openai::gpt-4o")
                .await
                .expect("find_by_canonical after soft delete");
        assert_eq!(found.lifecycle_status, crate::LifecycleStatus::Deprecated);
    }

    #[tokio::test]
    async fn model_soft_delete_hides_from_list() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::soft_delete(&model_repo, &conn, &scope, "openai::gpt-4o")
            .await
            .expect("soft delete");

        // Default list (Management, no deprecated) must NOT include the deprecated model.
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery::default(),
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("list should succeed");
        assert!(
            page.items.is_empty(),
            "deprecated model should be hidden from default list"
        );
    }

    #[tokio::test]
    async fn model_soft_delete_not_found() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let model_repo = ModelRepositoryImpl::default();
        let scope = scope_for(test_tenant());

        let err = ModelRepository::soft_delete(&model_repo, &conn, &scope, "nonexistent::model")
            .await
            .expect_err("should return not found");

        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound, got {err:?}"
        );
    }

    // =======================================================================
    // list — models
    // =======================================================================

    #[tokio::test]
    async fn model_list_returns_all_non_deprecated() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create gpt-4o");
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o-mini"),
        )
        .await
        .expect("create gpt-4o-mini");

        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery::default(),
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("list should succeed");
        assert!(!page.items.is_empty(), "should list non-deprecated models");
    }

    #[tokio::test]
    async fn model_list_clamps_to_configured_max_page_size() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let cfg = ModelRegistryConfig {
            max_page_size: 2,
            ..Default::default()
        };
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::new(cfg.page_limits());
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        for i in 0..4 {
            ModelRepository::create(
                &model_repo,
                &conn,
                &scope,
                tenant_id,
                &make_create_model_req(&provider_slug, &format!("gpt-{i}")),
            )
            .await
            .expect("create model");
        }

        // A `$top` above the configured maximum is clamped down to it.
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery {
                limit: Some(10),
                ..Default::default()
            },
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("list should succeed");
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.page_info.limit, 2);

        // No `$top`: the default page size (20) is clamped to the maximum too.
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery::default(),
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("list should succeed");
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.page_info.limit, 2);
    }

    #[tokio::test]
    async fn model_list_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let (_provider_id, provider_slug) = create_test_provider(
            &provider_repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            "openai",
        )
        .await;
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope_for(tenant_b),
            &ODataQuery::default(),
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("list should succeed");
        assert!(page.items.is_empty(), "tenant B should see no models");
    }

    #[tokio::test]
    async fn model_list_respects_limit() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        for i in 0..4 {
            ModelRepository::create(
                &model_repo,
                &conn,
                &scope,
                tenant_id,
                &make_create_model_req(&provider_slug, &format!("model-{i}")),
            )
            .await
            .expect("create model");
        }

        let query = ODataQuery {
            limit: Some(2),
            ..Default::default()
        };
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &query,
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("list with limit");
        assert_eq!(page.items.len(), 2);
    }

    // =======================================================================
    // Model list OData filtering
    // =======================================================================

    #[tokio::test]
    async fn model_list_filters_by_approval_status() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Create model with approved status directly on create.
        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("create approved model");

        // Create another model with pending status (default).
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o-mini"),
        )
        .await
        .expect("create model");

        // Filter by approval_status eq 'approved'
        let parsed = toolkit_odata::parse_filter_string("approval_status eq 'approved'")
            .expect("parse filter");
        let query = ODataQuery {
            filter: Some(Box::new(parsed.into_expr())),
            ..Default::default()
        };
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &query,
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("filtered list");
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            page.items[0].approval_status,
            crate::ApprovalStatus::Approved
        );
    }

    #[tokio::test]
    async fn model_list_filters_by_gts_type() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        // Filter by gts_type column.
        let gts_str = "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~";
        let parsed = toolkit_odata::parse_filter_string(&format!("gts_type eq '{gts_str}'"))
            .expect("parse filter");
        let query = ODataQuery {
            filter: Some(Box::new(parsed.into_expr())),
            ..Default::default()
        };
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &query,
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("filtered list");
        assert_eq!(page.items.len(), 1);
    }

    #[tokio::test]
    async fn model_list_filters_by_vision_capability() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        // Filter by cap_vision true
        let parsed = toolkit_odata::parse_filter_string("vision eq true").expect("parse filter");
        let query = ODataQuery {
            filter: Some(Box::new(parsed.into_expr())),
            ..Default::default()
        };
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &query,
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("filtered list");
        assert_eq!(page.items.len(), 1);
    }

    // =======================================================================
    // ListVisibility — Eval with allow-list filtering
    // =======================================================================

    #[tokio::test]
    async fn model_list_eval_with_non_empty_allow_list() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        // Create two providers.
        let (openai_id, openai_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let (_anthropic_id, anthropic_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "anthropic").await;

        // Create an approved model under each provider — the eval path also
        // gates on `approval_status = approved`, so a `pending` fixture would
        // make this test pass for the wrong reason.
        let mut openai_req = make_create_model_req(&openai_slug, "gpt-4o");
        openai_req.approval_status = Some(crate::ApprovalStatus::Approved);
        ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &openai_req)
            .await
            .expect("create openai model");
        let mut anthropic_req = make_create_model_req(&anthropic_slug, "claude-3");
        anthropic_req.approval_status = Some(crate::ApprovalStatus::Approved);
        ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &anthropic_req)
            .await
            .expect("create anthropic model");

        // Eval with allow-list containing only openai's provider_id.
        let allow_list = vec![openai_id];
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery::default(),
            ListVisibility::Eval {
                allow_list: &allow_list,
            },
        )
        .await
        .expect("list with allow-list");

        // Only the openai model should be returned.
        assert_eq!(
            page.items.len(),
            1,
            "only the openai model should be visible"
        );
        assert_eq!(page.items[0].canonical_id, "openai::gpt-4o");
    }

    #[tokio::test]
    async fn model_list_eval_with_empty_allow_list() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        // Eval with empty allow-list — no models should be returned.
        let allow_list: Vec<Uuid> = vec![];
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery::default(),
            ListVisibility::Eval {
                allow_list: &allow_list,
            },
        )
        .await
        .expect("list with empty allow-list");

        assert!(
            page.items.is_empty(),
            "empty allow-list should yield empty results"
        );
    }

    // =======================================================================
    // ListVisibility — Eval lifecycle exclusion is unconditional
    // =======================================================================

    #[tokio::test]
    async fn model_list_eval_hides_deprecated_even_with_filter() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Create a model and then soft-delete it.
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        // Create another model that stays active.
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o-mini"),
        )
        .await
        .expect("create gpt-4o-mini");

        // Soft-delete the first one.
        ModelRepository::soft_delete(&model_repo, &conn, &scope, "openai::gpt-4o")
            .await
            .expect("soft delete");

        // Eval with $filter=lifecycle_status eq 'deprecated' — should return
        // empty because the eval path unconditionally excludes deprecated/sunset.
        let parsed = toolkit_odata::parse_filter_string("lifecycle_status eq 'deprecated'")
            .expect("parse filter");
        let query = ODataQuery {
            filter: Some(Box::new(parsed.into_expr())),
            ..Default::default()
        };
        let allow_list = vec![provider_id];
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &query,
            ListVisibility::Eval {
                allow_list: &allow_list,
            },
        )
        .await
        .expect("eval list with deprecated filter");

        // The eval path must return empty — no lifecycle_status filter can
        // escape the unconditional exclusion.
        assert!(
            page.items.is_empty(),
            "eval path must unconditionally exclude deprecated models"
        );
    }

    /// The eval approval exclusion is mandatory: a `$filter` naming
    /// `approval_status` narrows within the approved set, it cannot re-admit
    /// non-approved rows (DESIGN §3.3 "The narrowing invariant").
    #[tokio::test]
    async fn model_list_eval_hides_non_approved_even_with_filter() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // One row per non-approved status, plus one approved row.
        for (model_id, status) in [
            ("gpt-4o", crate::ApprovalStatus::Pending),
            ("gpt-4o-mini", crate::ApprovalStatus::Rejected),
            ("o3", crate::ApprovalStatus::Revoked),
            ("gpt-5", crate::ApprovalStatus::Approved),
        ] {
            let mut req = make_create_model_req(&provider_slug, model_id);
            req.approval_status = Some(status);
            ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
                .await
                .expect("create model");
        }

        let allow_list = vec![provider_id];

        // Unfiltered: only the approved row survives.
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery::default(),
            ListVisibility::Eval {
                allow_list: &allow_list,
            },
        )
        .await
        .expect("eval list");
        assert_eq!(
            page.items.len(),
            1,
            "only the approved model is eval-visible"
        );
        assert_eq!(page.items[0].canonical_id, "openai::gpt-5");

        // Filtered by a non-approved status: empty, not those rows.
        for status in ["pending", "rejected", "revoked"] {
            let parsed =
                toolkit_odata::parse_filter_string(&format!("approval_status eq '{status}'"))
                    .expect("parse filter");
            let query = ODataQuery {
                filter: Some(Box::new(parsed.into_expr())),
                ..Default::default()
            };
            let page = ModelRepository::list(
                &model_repo,
                &conn,
                &scope,
                &query,
                ListVisibility::Eval {
                    allow_list: &allow_list,
                },
            )
            .await
            .expect("eval list with approval filter");
            assert!(
                page.items.is_empty(),
                "eval path must unconditionally exclude `{status}` models"
            );
        }
    }

    #[tokio::test]
    async fn model_list_management_include_deprecated_returns_deprecated_rows() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;

        // Create two models.
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o-mini"),
        )
        .await
        .expect("create gpt-4o-mini");

        // Soft-delete the first one.
        ModelRepository::soft_delete(&model_repo, &conn, &scope, "openai::gpt-4o")
            .await
            .expect("soft delete");

        // Management with include_deprecated: true — should return both.
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery::default(),
            ListVisibility::Management {
                include_deprecated: true,
            },
        )
        .await
        .expect("management list with include_deprecated");

        assert_eq!(
            page.items.len(),
            2,
            "management include_deprecated should return all rows"
        );

        // Verify the deprecated model IS included.
        let canonical_ids: Vec<&str> = page.items.iter().map(|m| m.canonical_id.as_str()).collect();
        assert!(
            canonical_ids.contains(&"openai::gpt-4o"),
            "deprecated model must be included"
        );
        assert!(
            canonical_ids.contains(&"openai::gpt-4o-mini"),
            "active model must be included"
        );
    }

    // =======================================================================
    // ListVisibility — Management returns rows outside allow-list
    // =======================================================================

    #[tokio::test]
    async fn model_list_management_returns_rows_outside_allow_list() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl::default();
        let model_repo = ModelRepositoryImpl::default();
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        // Create two providers.
        let (_openai_id, openai_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let (_anthropic_id, anthropic_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "anthropic").await;

        // Create a model under each provider.
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&openai_slug, "gpt-4o"),
        )
        .await
        .expect("create openai model");
        ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&anthropic_slug, "claude-3"),
        )
        .await
        .expect("create anthropic model");

        // Management listing must return ALL rows regardless of provider_id.
        let page = ModelRepository::list(
            &model_repo,
            &conn,
            &scope,
            &ODataQuery::default(),
            ListVisibility::Management {
                include_deprecated: false,
            },
        )
        .await
        .expect("management list");

        assert_eq!(
            page.items.len(),
            2,
            "management path must return all rows regardless of provider_id"
        );
        let canonical_ids: Vec<&str> = page.items.iter().map(|m| m.canonical_id.as_str()).collect();
        assert!(canonical_ids.contains(&"openai::gpt-4o"));
        assert!(canonical_ids.contains(&"anthropic::claude-3"));
    }

    // =======================================================================
    // Send + Sync bounds
    // =======================================================================

    #[test]
    fn repo_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ModelRepositoryImpl>();
    }
}
