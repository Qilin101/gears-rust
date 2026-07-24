//! `SeaORM` repository implementation for the Model Registry gear.
//!
//! Implements [`ProviderRepository`] and [`ModelRepository`] using
//! `SecureConn` / `AccessScope` for tenant-isolated database access. Every
//! query — read, write, list — is scoped so a caller can only see or modify
//! rows they are authorized for.

use async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, Set};
use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict,
    secure_update_with_scope,
};
use toolkit_odata::{ODataQuery, Page, SortDir, normalize_filter_for_hash};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::{ModelRepository, ProviderRepository};
use crate::{
    ApprovalStatus, CreateModelRequestV1, CreateProviderRequestV1, ModelV1, ProviderV1,
    UpdateModelRequestV1, UpdateProviderRequestV1,
};

use super::entity::{self, model, provider};
use super::mapper;
use super::odata_mapper::{
    ModelFilterField, ModelODataMapper, ProviderFilterField, ProviderODataMapper,
};

// =============================================================================
// ScopeError → DomainError
// =============================================================================

/// Check whether a [`sea_orm::DbErr`] represents a foreign-key constraint violation.
///
/// Uses `SeaORM`'s built-in `sql_err()` detection first, then falls back to
/// string matching on the error message for backends that strip the SQLSTATE.
#[must_use]
fn is_fk_violation(err: &DbErr) -> bool {
    // Discard unique-constraint violations early.
    if matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) {
        return false;
    }

    // Fallback: string-based detection for FK violations.
    let msg = err.to_string().to_lowercase();
    msg.contains("foreign key")
        || msg.contains("constraint failed")
        || msg.contains("is still referenced")
}

/// Map a [`ScopeError`] to a [`DomainError`].
///
/// Security denials become `Forbidden`; infrastructure problems become
/// `Internal`; invalid states become `Internal` (programming error).
fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Denied(msg) => DomainError::forbidden(msg),
        ScopeError::Invalid(msg) => DomainError::internal(format!("scope invalid: {msg}")),
        ScopeError::Db(e) => DomainError::internal(format!("database error: {e}")),
        ScopeError::TenantNotInScope { tenant_id } => {
            DomainError::forbidden(format!("tenant {tenant_id} not in scope"))
        }
    }
}

// =============================================================================
// SeaOrmRepository — holds no per-instance state
// =============================================================================

/// Shared repository struct implementing both [`ProviderRepository`] and
/// [`ModelRepository`].
///
/// Stateless: all database interactions go through the [`DBRunner`] connection
/// passed per-call, ensuring transactional boundaries are caller-controlled.
#[derive(Debug, Clone, Copy, Default)]
pub struct SeaOrmRepository;

impl SeaOrmRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

// =============================================================================
// ProviderRepository implementation
// =============================================================================

#[async_trait]
impl ProviderRepository for SeaOrmRepository {
    async fn find_by_id<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<ProviderV1, DomainError> {
        let entity = provider::Entity::find()
            .secure()
            .scope_with(scope)
            .and_id(id)
            .map_err(map_scope_error)?
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::provider_not_found(id))?;

        Ok(mapper::provider_entity_to_v1(&entity))
    }

    async fn find_by_slug<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        slug: &str,
    ) -> Result<ProviderV1, DomainError> {
        let entity = provider::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(provider::Column::Slug.eq(slug)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::provider_not_found_by_slug(slug))?;

        Ok(mapper::provider_entity_to_v1(&entity))
    }

    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, DomainError> {
        let base = provider::Entity::find().secure().scope_with(scope);

        let page = paginate_odata::<
            ProviderFilterField,
            ProviderODataMapper,
            provider::Entity,
            ProviderV1,
            _,
            C,
        >(
            base,
            conn,
            query,
            ("slug", SortDir::Asc),
            LimitCfg {
                default: 20,
                max: 100,
            },
            |m| mapper::provider_entity_to_v1(&m),
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
        req: &CreateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError> {
        // Check for slug conflict within the tenant scope before inserting.
        let existing = provider::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(provider::Column::Slug.eq(req.slug())))
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        if existing.is_some() {
            return Err(DomainError::provider_conflict(req.slug()));
        }

        let am = mapper::provider_create_active_model(tenant_id, req);

        let _ = provider::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(map_scope_error)?
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        // Re-fetch to get the DB-persisted state
        let entity = provider::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(provider::Column::Slug.eq(req.slug())))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or_else(|| DomainError::internal("created provider not found after insert"))?;

        Ok(mapper::provider_entity_to_v1(&entity))
    }

    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        req: &UpdateProviderRequestV1,
    ) -> Result<ProviderV1, DomainError> {
        // Fetch existing entity (scope-checked).
        let existing = provider::Entity::find()
            .secure()
            .scope_with(scope)
            .and_id(id)
            .map_err(map_scope_error)?
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::provider_not_found(id))?;

        // Build the patched ActiveModel via the mapper (PATCH semantics).
        let am = mapper::provider_update_active_model(&existing, req);

        // Execute update using the toolkit-db helper which validates the scope,
        // ensures tenant_id immutability, and routes to the correct DB runner.
        let updated = secure_update_with_scope::<provider::Entity>(am, scope, id, conn)
            .await
            .map_err(map_scope_error)?;

        Ok(mapper::provider_entity_to_v1(&updated))
    }

    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<(), DomainError> {
        // Pre-check: refuse deletion if models still reference this provider.
        let models = model::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(model::Column::ProviderId.eq(id)))
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        if !models.is_empty() {
            return Err(DomainError::provider_has_models(id, models.len() as u64));
        }

        let result = provider::Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(provider::Column::Id.eq(id)))
            .exec(conn)
            .await
            .map_err(|e| match &e {
                // TOCTOU guard: a concurrent model creation between the
                // pre-check and the DELETE can fire the FK constraint.
                ScopeError::Db(db_err) if is_fk_violation(db_err) => {
                    DomainError::provider_has_models(id, 0)
                }
                _ => map_scope_error(e),
            })?;

        if result.rows_affected == 0 {
            return Err(DomainError::provider_not_found(id));
        }

        Ok(())
    }
}

// =============================================================================
// ModelRepository implementation
// =============================================================================

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

#[async_trait]
impl ModelRepository for SeaOrmRepository {
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

// ═══════════════════════════════════════════════════════════════════════════════
// Internal helpers — ApprovalStatus ↔ string
// ═══════════════════════════════════════════════════════════════════════════════

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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CreateProviderRequestV1;
    use sea_orm_migration::MigratorTrait;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};

    use crate::infra::storage::migrations::Migrator;

    /// Helper to set up an in-memory `SQLite` database with the providers table.
    /// Returns a [`DBProvider`] whose `.conn()` method returns a [`DbConn`]
    /// implementing [`DBRunner`].
    async fn setup_provider() -> DBProvider<DbError> {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts)
            .await
            .expect("in-memory SQLite connection");

        // Apply the production migration to create all three tables.
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

    fn make_create_req(slug: &str, name: &str) -> CreateProviderRequestV1 {
        let gts = gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
        CreateProviderRequestV1::builder(slug, name, gts).build()
    }

    fn make_full_create_req(slug: &str, name: &str) -> CreateProviderRequestV1 {
        let gts = gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
        CreateProviderRequestV1::builder(slug, name, gts)
            .managed(true)
            .metadata(serde_json::json!({"region": "us-east-1"}))
            .discovery_enabled(true)
            .discovery_interval_seconds(3600)
            .build()
    }

    // =========================================================================
    // find_by_id
    // =========================================================================

    #[tokio::test]
    async fn find_by_id_returns_provider() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let created = ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create should succeed");

        let found = ProviderRepository::find_by_id(&repo, &conn, &scope, created.id)
            .await
            .expect("find_by_id should succeed");

        assert_eq!(found.id, created.id);
        assert_eq!(found.slug, "openai");
        assert_eq!(found.name, "OpenAI");
    }

    #[tokio::test]
    async fn find_by_id_returns_not_found_for_wrong_id() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let scope = scope_for(test_tenant());

        let err = ProviderRepository::find_by_id(&repo, &conn, &scope, Uuid::nil())
            .await
            .expect_err("should return provider not found");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { .. }),
            "expected ProviderNotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn find_by_id_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let created = ProviderRepository::create(
            &repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create should succeed");

        // Other tenant should not see this provider.
        let err = ProviderRepository::find_by_id(&repo, &conn, &scope_for(tenant_b), created.id)
            .await
            .expect_err("should be not found for other tenant");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { .. }),
            "expected ProviderNotFound, got {err:?}"
        );
    }

    // =========================================================================
    // create
    // =========================================================================

    #[tokio::test]
    async fn create_stores_provider() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let provider = ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create should succeed");

        assert_eq!(provider.slug, "openai");
        assert_eq!(provider.name, "OpenAI");
        assert_eq!(provider.status, crate::ProviderStatus::Active);
        assert!(!provider.managed);
        assert!(!provider.discovery_enabled);
        assert!(provider.metadata.is_none());
        assert!(provider.discovery_interval_seconds.is_none());
    }

    #[tokio::test]
    async fn create_with_all_fields() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let provider = ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_full_create_req("anthropic", "Anthropic"),
        )
        .await
        .expect("create should succeed");

        assert_eq!(provider.slug, "anthropic");
        assert_eq!(provider.name, "Anthropic");
        assert!(provider.managed);
        assert!(provider.discovery_enabled);
        assert_eq!(
            provider.metadata,
            Some(serde_json::json!({"region": "us-east-1"}))
        );
        assert_eq!(provider.discovery_interval_seconds, Some(3600));
    }

    #[tokio::test]
    async fn create_rejects_duplicate_slug_in_same_tenant() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("first create should succeed");

        let err = ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI Duplicate"),
        )
        .await
        .expect_err("duplicate slug should be rejected");

        assert!(
            matches!(&err, DomainError::ProviderConflict { slug } if slug == "openai"),
            "expected ProviderConflict for slug openai, got {err:?}"
        );
    }

    #[tokio::test]
    async fn create_same_slug_different_tenants_is_allowed() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        ProviderRepository::create(
            &repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("tenant A create should succeed");

        let provider_b = ProviderRepository::create(
            &repo,
            &conn,
            &scope_for(tenant_b),
            tenant_b,
            &make_create_req("openai", "OpenAI B"),
        )
        .await
        .expect("tenant B create with same slug should succeed");

        assert_eq!(provider_b.slug, "openai");
        assert_eq!(provider_b.name, "OpenAI B");
    }

    // =========================================================================
    // update
    // =========================================================================

    #[tokio::test]
    async fn update_changes_fields() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let created = ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create should succeed");

        let updated = ProviderRepository::update(
            &repo,
            &conn,
            &scope,
            created.id,
            &UpdateProviderRequestV1 {
                name: Some("OpenAI Updated".into()),
                managed: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("update should succeed");

        assert_eq!(updated.name, "OpenAI Updated");
        assert!(updated.managed);
        // Slug should remain unchanged.
        assert_eq!(updated.slug, "openai");
    }

    #[tokio::test]
    async fn update_not_found() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let scope = scope_for(test_tenant());

        let err = ProviderRepository::update(
            &repo,
            &conn,
            &scope,
            Uuid::nil(),
            &UpdateProviderRequestV1 {
                name: Some("N/A".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("should return not found");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { .. }),
            "expected ProviderNotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn update_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let created = ProviderRepository::create(
            &repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create should succeed");

        // Other tenant should not be able to update.
        let err = ProviderRepository::update(
            &repo,
            &conn,
            &scope_for(tenant_b),
            created.id,
            &UpdateProviderRequestV1 {
                name: Some("Hacked".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("should be not found for other tenant");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { .. }),
            "expected ProviderNotFound, got {err:?}"
        );
    }

    // =========================================================================
    // delete
    // =========================================================================

    #[tokio::test]
    async fn delete_removes_provider() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let created = ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create should succeed");

        ProviderRepository::delete(&repo, &conn, &scope, created.id)
            .await
            .expect("delete should succeed");

        // Verify it's gone.
        let err = ProviderRepository::find_by_id(&repo, &conn, &scope, created.id)
            .await
            .expect_err("should be gone after delete");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { .. }),
            "expected ProviderNotFound after delete, got {err:?}"
        );
    }

    #[tokio::test]
    async fn delete_not_found() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let scope = scope_for(test_tenant());

        let err = ProviderRepository::delete(&repo, &conn, &scope, Uuid::nil())
            .await
            .expect_err("should return not found");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { .. }),
            "expected ProviderNotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn delete_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let created = ProviderRepository::create(
            &repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create should succeed");

        // Other tenant should not be able to delete.
        let err = ProviderRepository::delete(&repo, &conn, &scope_for(tenant_b), created.id)
            .await
            .expect_err("should be not found for other tenant");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { .. }),
            "expected ProviderNotFound, got {err:?}"
        );

        // Original tenant's provider should still exist.
        let found = ProviderRepository::find_by_id(&repo, &conn, &scope_for(tenant_a), created.id)
            .await
            .expect("provider should still exist");

        assert_eq!(found.id, created.id);
    }

    // =========================================================================
    // list (OData)
    // =========================================================================

    #[tokio::test]
    async fn list_returns_all_providers_in_tenant() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create openai");
        ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_req("anthropic", "Anthropic"),
        )
        .await
        .expect("create anthropic");

        let query = ODataQuery::default();
        let page = ProviderRepository::list(&repo, &conn, &scope, &query)
            .await
            .expect("list should succeed");

        assert_eq!(page.items.len(), 2);
        assert!(page.items.iter().any(|p| p.slug == "openai"));
        assert!(page.items.iter().any(|p| p.slug == "anthropic"));
    }

    #[tokio::test]
    async fn list_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        ProviderRepository::create(
            &repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("tenant A create");

        // Tenant B should see no providers.
        let page =
            ProviderRepository::list(&repo, &conn, &scope_for(tenant_b), &ODataQuery::default())
                .await
                .expect("list should succeed");

        assert!(page.items.is_empty(), "tenant B should see no providers");
    }

    #[tokio::test]
    async fn list_respects_odata_filter() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let gts_openai =
            gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
        let gts_anthropic =
            gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.anthropic.v1~");

        ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &CreateProviderRequestV1::builder("openai", "OpenAI", gts_openai).build(),
        )
        .await
        .expect("create openai");
        ProviderRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &CreateProviderRequestV1::builder("anthropic", "Anthropic", gts_anthropic).build(),
        )
        .await
        .expect("create anthropic");

        // Filter by slug eq 'openai' using the OData filter parser
        let parsed = toolkit_odata::parse_filter_string("slug eq 'openai'")
            .expect("parse filter should succeed");
        let query = ODataQuery {
            filter: Some(Box::new(parsed.into_expr())),
            ..Default::default()
        };

        let page = ProviderRepository::list(&repo, &conn, &scope, &query)
            .await
            .expect("filtered list should succeed");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].slug, "openai");
    }

    #[tokio::test]
    async fn list_respects_limit() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        // Create 4 providers.
        for i in 0..4 {
            ProviderRepository::create(
                &repo,
                &conn,
                &scope,
                tenant_id,
                &make_create_req(&format!("provider-{i}"), &format!("Provider {i}")),
            )
            .await
            .expect("create provider");
        }

        // Get page with limit=2
        let query = ODataQuery {
            limit: Some(2),
            ..Default::default()
        };

        let page = ProviderRepository::list(&repo, &conn, &scope, &query)
            .await
            .expect("list with limit should succeed");
        assert_eq!(page.items.len(), 2);
    }

    // =========================================================================
    // Send + Sync bounds
    // =========================================================================

    #[test]
    fn repo_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SeaOrmRepository>();
    }

    // =========================================================================
    // ModelRepository tests
    // =========================================================================

    /// Helper: create a provider for model tests.
    async fn create_test_provider(
        repo: &SeaOrmRepository,
        conn: &impl DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        slug: &str,
    ) -> (Uuid, String) {
        let p =
            ProviderRepository::create(repo, conn, scope, tenant_id, &make_create_req(slug, slug))
                .await
                .expect("create test provider");
        (p.id, p.slug)
    }

    /// Helper: create a model for test purposes.
    /// Uses JSON roundtrip for `ModelInfoV1` (which is `#[non_exhaustive]`
    /// in the SDK crate).
    fn make_create_model_req(provider_slug: &str, provider_model_id: &str) -> CreateModelRequestV1 {
        let gts_leaf = "cf.genai._.openai.v1~";
        let gts_type = format!("gts.cf.genai.model.info.v1~{gts_leaf}");

        let info_value = serde_json::json!({
            "gts_type": gts_type,
            "display_name": format!("Test {provider_model_id}"),
            "description": null,
            "family": "test-family",
            "vendor": "TestVendor",
            "managed": false,
            "architecture": "transformer",
            "size_bytes": null,
            "format": "api-only",
            "region": null,
            "hosted_by": null,
            "last_release_at": null,
            "reasoning_level": null,
            "version": null,
            "sort_order": null,
            "icon": null,
            "multiplier_display": null,
            "performance": {
                "response_latency_ms": null,
                "tokens_per_second": null
            },
            "additional_info": {},
            "supported_api": ["completion"],
            "provider_model_id": provider_model_id,
            "capabilities": {
                "vision": { "enabled": true, "supported_mime_types": ["image/jpeg"] },
                "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                "function_calling": true,
                "response_schema": false,
                "streaming": true,
                "file_input": { "enabled": false, "supported_mime_types": [] },
                "image_generation": { "enabled": false, "supported_mime_types": [] },
                "audio_input": { "enabled": false, "supported_mime_types": [] },
                "audio_output": { "enabled": false, "supported_mime_types": [] },
                "code_interpreter": false,
                "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
            },
            "disabled_capabilities": {
                "vision": { "disabled": false, "disabled_mime_types": [] },
                "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
                "function_calling": false,
                "response_schema": false,
                "streaming": false,
                "file_input": { "disabled": false, "disabled_mime_types": [] },
                "image_generation": { "disabled": false, "disabled_mime_types": [] },
                "audio_input": { "disabled": false, "disabled_mime_types": [] },
                "audio_output": { "disabled": false, "disabled_mime_types": [] },
                "code_interpreter": false,
                "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
            },
            "context_window": {
                "max_input_tokens": 8192,
                "max_output_tokens": 4096
            },
            "default_parameters": {
                "temperature": null, "top_p": null, "max_output_tokens": null,
                "max_tool_calls": null, "presence_penalty": null, "frequency_penalty": null,
                "top_logprobs": null, "truncation": null, "service_tier": null,
                "parallel_tool_calls": null, "text": null, "reasoning": null,
                "tool_choice": null, "store": null
            },
            "allow_parameter_override": false,
            "allow_extra_params": [],
            "provider_settings": {}
        });

        let info = serde_json::from_value(info_value).expect("ModelInfoV1 from test JSON");

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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let model = ModelRepository::create(&repo, &conn, &scope, tenant_id, &req)
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let mut req = make_create_model_req(&provider_slug, "gpt-4o");
        req.approval_status = Some(crate::ApprovalStatus::Approved);
        let model = ModelRepository::create(&repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("create model");

        assert_eq!(model.approval_status, crate::ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn model_create_duplicate_canonical_id_rejected() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        ModelRepository::create(&repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("first create");

        // Second create with same canonical_id must fail.
        let err = ModelRepository::create(&repo, &conn, &scope, tenant_id, &req)
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let req = make_create_model_req("nonexistent-provider", "gpt-4o");
        let err = ModelRepository::create(&repo, &conn, &scope, tenant_id, &req)
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let created = ModelRepository::create(&repo, &conn, &scope, tenant_id, &req)
            .await
            .expect("create model");

        let found = ModelRepository::find_by_canonical(&repo, &conn, &scope, "openai::gpt-4o")
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
        let repo = SeaOrmRepository;
        let scope = scope_for(test_tenant());

        let err = ModelRepository::find_by_canonical(&repo, &conn, &scope, "nonexistent::model")
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
        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope_for(tenant_a), tenant_a, "openai").await;
        let req = make_create_model_req(&provider_slug, "gpt-4o");
        let _created = ModelRepository::create(&repo, &conn, &scope_for(tenant_a), tenant_a, &req)
            .await
            .expect("create model");

        // Other tenant should not see this model.
        let err = ModelRepository::find_by_canonical(
            &repo,
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let _created = ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        let updated = ModelRepository::update(
            &repo,
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
        let repo = SeaOrmRepository;
        let scope = scope_for(test_tenant());

        let err = ModelRepository::update(
            &repo,
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let _created = ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::soft_delete(&repo, &conn, &scope, "openai::gpt-4o")
            .await
            .expect("soft delete should succeed");

        // After soft-delete, direct fetch should show deprecated.
        let found = ModelRepository::find_by_canonical(&repo, &conn, &scope, "openai::gpt-4o")
            .await
            .expect("find_by_canonical after soft delete");
        assert_eq!(found.lifecycle_status, crate::LifecycleStatus::Deprecated);
    }

    #[tokio::test]
    async fn model_soft_delete_hides_from_list() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::soft_delete(&repo, &conn, &scope, "openai::gpt-4o")
            .await
            .expect("soft delete");

        // Default list must NOT include the deprecated model.
        let page = ModelRepository::list(&repo, &conn, &scope, &ODataQuery::default())
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
        let repo = SeaOrmRepository;
        let scope = scope_for(test_tenant());

        let err = ModelRepository::soft_delete(&repo, &conn, &scope, "nonexistent::model")
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;

        ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create gpt-4o");
        ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o-mini"),
        )
        .await
        .expect("create gpt-4o-mini");

        let page = ModelRepository::list(&repo, &conn, &scope, &ODataQuery::default())
            .await
            .expect("list should succeed");
        assert!(!page.items.is_empty(), "should list non-deprecated models");
    }

    #[tokio::test]
    async fn model_list_tenant_isolation() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope_for(tenant_a), tenant_a, "openai").await;
        ModelRepository::create(
            &repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        let page =
            ModelRepository::list(&repo, &conn, &scope_for(tenant_b), &ODataQuery::default())
                .await
                .expect("list should succeed");
        assert!(page.items.is_empty(), "tenant B should see no models");
    }

    #[tokio::test]
    async fn model_list_respects_limit() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;

        for i in 0..4 {
            ModelRepository::create(
                &repo,
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
        let page = ModelRepository::list(&repo, &conn, &scope, &query)
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        let status = ModelRepository::get_approval(&repo, &conn, &scope, model.id)
            .await
            .expect("get approval");
        assert_eq!(status, crate::ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn approval_set_updates_status_and_denormalized_column() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        // Approve
        ModelRepository::set_approval(
            &repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Approved,
        )
        .await
        .expect("set approval");

        // Verify via get_approval.
        let status = ModelRepository::get_approval(&repo, &conn, &scope, model.id)
            .await
            .expect("get approval");
        assert_eq!(status, crate::ApprovalStatus::Approved);

        // Verify denormalized column on model read.
        let found = ModelRepository::find_by_canonical(&repo, &conn, &scope, "openai::gpt-4o")
            .await
            .expect("find model");
        assert_eq!(found.approval_status, crate::ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn approval_set_reject_then_revoke() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::set_approval(
            &repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Rejected,
        )
        .await
        .expect("reject");
        assert_eq!(
            ModelRepository::get_approval(&repo, &conn, &scope, model.id)
                .await
                .expect("get approval"),
            crate::ApprovalStatus::Rejected
        );

        ModelRepository::set_approval(
            &repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Revoked,
        )
        .await
        .expect("revoke");
        assert_eq!(
            ModelRepository::get_approval(&repo, &conn, &scope, model.id)
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::set_approval(
            &repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Approved,
        )
        .await
        .expect("approve");

        ModelRepository::delete_approval(&repo, &conn, &scope, model.id)
            .await
            .expect("delete approval");

        assert_eq!(
            ModelRepository::get_approval(&repo, &conn, &scope, model.id)
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
        let repo = SeaOrmRepository;
        let tenant_a = test_tenant();
        let tenant_b = other_tenant();

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope_for(tenant_a), tenant_a, "openai").await;
        let model = ModelRepository::create(
            &repo,
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        // Tenant b cannot find the model, so approval ops won't succeed on it
        // (the model doesn't exist in tenant_b's scope).
        let err = ModelRepository::get_approval(&repo, &conn, &scope_for(tenant_b), model.id)
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        let model = ModelRepository::create(
            &repo,
            &conn,
            &scope,
            tenant_id,
            &make_create_model_req(&provider_slug, "gpt-4o"),
        )
        .await
        .expect("create model");

        ModelRepository::set_approval(
            &repo,
            &conn,
            &scope,
            model.id,
            crate::ApprovalStatus::Approved,
        )
        .await
        .expect("approve");

        // Create another model with pending status.
        ModelRepository::create(
            &repo,
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
        let page = ModelRepository::list(&repo, &conn, &scope, &query)
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
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        ModelRepository::create(
            &repo,
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
        let page = ModelRepository::list(&repo, &conn, &scope, &query)
            .await
            .expect("filtered list");
        assert_eq!(page.items.len(), 1);
    }

    #[tokio::test]
    async fn model_list_filters_by_vision_capability() {
        let provider = setup_provider().await;
        #[allow(clippy::expect_used)]
        let conn = provider.conn().expect("conn");
        let repo = SeaOrmRepository;
        let tenant_id = test_tenant();
        let scope = scope_for(tenant_id);

        let (_provider_id, provider_slug) =
            create_test_provider(&repo, &conn, &scope, tenant_id, "openai").await;
        ModelRepository::create(
            &repo,
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
        let page = ModelRepository::list(&repo, &conn, &scope, &query)
            .await
            .expect("filtered list");
        assert_eq!(page.items.len(), 1);
    }
}
