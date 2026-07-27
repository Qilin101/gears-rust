//! `SeaORM`-backed implementation of [`ModelRepository`].
//!
//! Cross-entity read: [`Self::create`] queries `provider::Entity` to verify the
//! referenced provider exists before inserting a model.

use async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Set};
use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict,
    secure_update_with_scope,
};
use toolkit_odata::{ODataQuery, Page, SortDir, normalize_filter_for_hash};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::ModelRepository;
use crate::{
    ApprovalStatus, CreateModelRequestV1, ModelV1, UpdateModelRequestV1,
};

use super::entity::{self, model, provider};
use super::error_mapping::map_scope_error;
use super::mapper;
use super::odata_mapper::{ModelFilterField, ModelODataMapper};

// =============================================================================
// ModelRepositoryImpl — holds no per-instance state
// =============================================================================

/// `SeaORM`-backed [`ModelRepository`] implementation.
///
/// Stateless: all database interactions go through the [`DBRunner`] connection
/// passed per-call, ensuring transactional boundaries are caller-controlled.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelRepositoryImpl;

impl ModelRepositoryImpl {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Check whether an `OData` filter expression references `lifecycle_status`.
///
/// When the caller explicitly filters on `lifecycle_status` (e.g.
/// `lifecycle_status eq 'deprecated'`), the base exclusion of deprecated
/// / sunset models is omitted so their filter works as intended.
#[must_use]
fn filter_references_lifecycle_status(query: &ODataQuery) -> bool {
    query.filter.as_ref().is_some_and(|expr| {
        let normalized = normalize_filter_for_hash(expr);
        normalized.contains("id(lifecycle_status)")
    })
}

/// Convert an [`ApprovalStatus`] to its lowercase storage string.
#[must_use]
fn approval_status_to_string(status: ApprovalStatus) -> String {
    match status {
        ApprovalStatus::Approved => "approved".to_owned(),
        ApprovalStatus::Rejected => "rejected".to_owned(),
        ApprovalStatus::Revoked => "revoked".to_owned(),
        _ => "pending".to_owned(),
    }
}

/// Parse a lowercase string back to [`ApprovalStatus`].
#[must_use]
fn approval_status_from_string(s: &str) -> ApprovalStatus {
    match s {
        "approved" => ApprovalStatus::Approved,
        "rejected" => ApprovalStatus::Rejected,
        "revoked" => ApprovalStatus::Revoked,
        _ => ApprovalStatus::Pending,
    }
}

#[async_trait]
impl ModelRepository for ModelRepositoryImpl {
    async fn find_by_canonical<C: DBRunner>(
        &self,
        conn: &C,
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

        Ok(mapper::model_entity_to_v1(&entity))
    }

    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, DomainError> {
        // Exclude deprecated / sunset models by default so the default list
        // shows only active, non-deprecated models. If the caller explicitly
        // wants deprecated models, they must add `lifecycle_status eq 'deprecated'`
        // to the OData filter — detected below to avoid a contradictory AND.
        let mut base = model::Entity::find().secure().scope_with(scope);
        if !filter_references_lifecycle_status(query) {
            base = base.filter(
                Condition::all()
                    .add(model::Column::LifecycleStatus.ne("deprecated"))
                    .add(model::Column::LifecycleStatus.ne("sunset")),
            );
        }

        let page =
            paginate_odata::<ModelFilterField, ModelODataMapper, model::Entity, ModelV1, _, C>(
                base,
                conn,
                query,
                ("canonical_id", SortDir::Asc),
                LimitCfg {
                    default: 20,
                    max: 100,
                },
                |m| mapper::model_entity_to_v1(&m),
            )
            .await
            .map_err(|e| DomainError::internal(format!("OData pagination failed: {e}")))?;

        Ok(page)
    }

    async fn create<C: DBRunner>(
        &self,
        conn: &C,
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

        let initial_approval = req.approval_status.unwrap_or(ApprovalStatus::Pending);
        let am =
            mapper::model_create_active_model(tenant_id, provider_entity.id, req, initial_approval);

        let _ = model::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(map_scope_error)?
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        // Re-fetch to get the DB-persisted state via canonical_id.
        let entity = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::CanonicalId.eq(&canonical_id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or_else(|| DomainError::internal("created model not found after insert"))?;

        Ok(mapper::model_entity_to_v1(&entity))
    }

    async fn update<C: DBRunner>(
        &self,
        conn: &C,
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
        let am = mapper::model_update_active_model(&existing, req);

        let model_id = existing.id;

        // Execute update using secure_update_with_scope.
        let updated = secure_update_with_scope::<model::Entity>(am, scope, model_id, conn)
            .await
            .map_err(map_scope_error)?;

        Ok(mapper::model_entity_to_v1(&updated))
    }

    async fn soft_delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        canonical_id: &str,
    ) -> Result<(), DomainError> {
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

        let _updated = secure_update_with_scope::<model::Entity>(am, scope, model_id, conn)
            .await
            .map_err(map_scope_error)?;

        Ok(())
    }

    async fn get_approval<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        model_id: Uuid,
    ) -> Result<ApprovalStatus, DomainError> {
        // Read approval status from the denormalized models column.
        let entity = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::Id.eq(model_id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::model_not_found(model_id.to_string()))?;

        Ok(approval_status_from_string(&entity.approval_status))
    }

    async fn set_approval<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        model_id: Uuid,
        status: ApprovalStatus,
    ) -> Result<(), DomainError> {
        let status_str = approval_status_to_string(status);
        let now = chrono::Utc::now();

        // Fetch model within scope (validates caller can access this model).
        let entity = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::Id.eq(model_id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::model_not_found(model_id.to_string()))?;

        // Upsert model_approvals FIRST (the P1 seam of record). If this fails,
        // the denormalized column is unchanged — no desync.
        let approval_am = entity::model_approval::ActiveModel {
            tenant_id: Set(entity.tenant_id),
            model_id: Set(model_id),
            approval_status: Set(status_str.clone()),
            created_at: Set(now),
            updated_at: Set(now),
        };

        let on_conflict = SecureOnConflict::columns([
            entity::model_approval::Column::TenantId,
            entity::model_approval::Column::ModelId,
        ])
        .update_columns([
            entity::model_approval::Column::ApprovalStatus,
            entity::model_approval::Column::UpdatedAt,
        ])
        .map_err(|e| DomainError::internal(format!("upsert conflict config: {e}")))?;

        let _ = entity::model_approval::Entity::insert(approval_am.clone())
            .secure()
            .scope_with_model(scope, &approval_am)
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        // Update denormalized models.approval_status using secure_update_with_scope.
        let mut model_am: entity::model::ActiveModel = entity.into();
        model_am.approval_status = Set(status_str);
        model_am.updated_at = Set(now);
        let _updated = secure_update_with_scope::<model::Entity>(model_am, scope, model_id, conn)
            .await
            .map_err(map_scope_error)?;

        Ok(())
    }

    async fn delete_approval<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        model_id: Uuid,
    ) -> Result<(), DomainError> {
        // Fetch model within scope.
        let entity = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::Id.eq(model_id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::model_not_found(model_id.to_string()))?;

        // Delete the model_approval record FIRST (the authoritative record).
        let _ = entity::model_approval::Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(entity::model_approval::Column::ModelId.eq(model_id))
                    .add(entity::model_approval::Column::TenantId.eq(entity.tenant_id)),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        // Then reset the denormalized column to default (Pending).
        let mut model_am: entity::model::ActiveModel = entity.into();
        model_am.approval_status = Set("pending".to_owned());
        model_am.updated_at = Set(chrono::Utc::now());
        let _ = secure_update_with_scope::<model::Entity>(model_am, scope, model_id, conn)
            .await
            .map_err(map_scope_error)?;

        Ok(())
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

    use crate::domain::repo::ProviderRepository;
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
        let gts = gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
        let model_repo = ModelRepositoryImpl;
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let created = ModelRepository::create(&model_repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("create model");

        let found = ModelRepository::find_by_canonical(&model_repo, &conn, &scope, "openai::gpt-4o")
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
        let model_repo = ModelRepositoryImpl;
        let scope = scope_for(test_tenant());

        let err = ModelRepository::find_by_canonical(&model_repo, &conn, &scope, "nonexistent::model")
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope_for(tenant_a), tenant_a, "openai")
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
        let model_repo = ModelRepositoryImpl;
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        // Default list must NOT include the deprecated model.
        let page = ModelRepository::list(&model_repo, &conn, &scope, &ODataQuery::default())
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
        let model_repo = ModelRepositoryImpl;
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        let page = ModelRepository::list(&model_repo, &conn, &scope, &ODataQuery::default())
            .await
            .expect("list should succeed");
        assert!(!page.items.is_empty(), "should list non-deprecated models");
    }

    #[tokio::test]
    async fn model_list_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope_for(tenant_a), tenant_a, "openai")
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

        let page =
            ModelRepository::list(&model_repo, &conn, &scope_for(tenant_b), &ODataQuery::default())
                .await
                .expect("list should succeed");
        assert!(page.items.is_empty(), "tenant B should see no models");
    }

    #[tokio::test]
    async fn model_list_respects_limit() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
        let page = ModelRepository::list(&model_repo, &conn, &scope, &query)
            .await
            .expect("list with limit");
        assert_eq!(page.items.len(), 2);
    }

    // =======================================================================
    // Approval operations
    // =======================================================================

    #[tokio::test]
    async fn approval_get_default_is_pending() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        let status = ModelRepository::get_approval(&model_repo, &conn, &scope, model.id)
            .await
            .expect("get approval");
        assert_eq!(status, crate::ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn approval_set_updates_status_and_denormalized_column() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        // Approve
        ModelRepository::set_approval(
            &model_repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Approved,
        )
        .await
        .expect("set approval");

        // Verify via get_approval.
        let status = ModelRepository::get_approval(&model_repo, &conn, &scope, model.id)
            .await
            .expect("get approval");
        assert_eq!(status, crate::ApprovalStatus::Approved);

        // Verify denormalized column on model read.
        let found =
            ModelRepository::find_by_canonical(&model_repo, &conn, &scope, "openai::gpt-4o")
                .await
                .expect("find model");
        assert_eq!(found.approval_status, crate::ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn approval_set_reject_then_revoke() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::set_approval(
            &model_repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Rejected,
        )
        .await
        .expect("reject");
        assert_eq!(
            ModelRepository::get_approval(&model_repo, &conn, &scope, model.id)
                .await
                .expect("get approval"),
            crate::ApprovalStatus::Rejected
        );

        ModelRepository::set_approval(
            &model_repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Revoked,
        )
        .await
        .expect("revoke");
        assert_eq!(
            ModelRepository::get_approval(&model_repo, &conn, &scope, model.id)
                .await
                .expect("get approval"),
            crate::ApprovalStatus::Revoked
        );
    }

    #[tokio::test]
    async fn approval_delete_resets_to_pending() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::set_approval(
            &model_repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Approved,
        )
        .await
        .expect("approve");

        ModelRepository::delete_approval(&model_repo, &conn, &scope, model.id)
            .await
            .expect("delete approval");

        assert_eq!(
            ModelRepository::get_approval(&model_repo, &conn, &scope, model.id)
                .await
                .expect("get approval"),
            crate::ApprovalStatus::Pending
        );
    }

    #[tokio::test]
    async fn approval_cross_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope_for(tenant_a), tenant_a, "openai")
                .await;
        let model = ModelRepository::create(
            &model_repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        // Tenant b cannot find the model, so approval ops won't succeed on it
        // (the model doesn't exist in tenant_b's scope).
        let err = ModelRepository::get_approval(&model_repo, &conn, &scope_for(tenant_b), model.id)
            .await
            .expect_err("should fail for other tenant");
        assert!(
            matches!(&err, DomainError::ModelNotFound { .. }),
            "expected ModelNotFound for cross-tenant approval, got {err:?}"
        );
    }

    // =======================================================================
    // Model list OData filtering
    // =======================================================================

    #[tokio::test]
    async fn model_list_filters_by_approval_status() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&provider_repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &model_repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::set_approval(
            &model_repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Approved,
        )
        .await
        .expect("approve");

        // Create another model with pending status.
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
        let page = ModelRepository::list(&model_repo, &conn, &scope, &query)
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
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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

        // Filter by gts_type (denormalized column).
        let gts_str = "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~";
        let parsed = toolkit_odata::parse_filter_string(&format!("gts_type eq '{gts_str}'"))
            .expect("parse filter");
        let query = ODataQuery {
            filter: Some(Box::new(parsed.into_expr())),
            ..Default::default()
        };
        let page = ModelRepository::list(&model_repo, &conn, &scope, &query)
            .await
            .expect("filtered list");
        assert_eq!(page.items.len(), 1);
    }

    #[tokio::test]
    async fn model_list_filters_by_vision_capability() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let provider_repo = ProviderRepositoryImpl;
        let model_repo = ModelRepositoryImpl;
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
        let page = ModelRepository::list(&model_repo, &conn, &scope, &query)
            .await
            .expect("filtered list");
        assert_eq!(page.items.len(), 1);
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
