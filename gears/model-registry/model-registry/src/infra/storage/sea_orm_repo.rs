//! `SeaORM` repository implementation for the Model Registry gear.
//!
//! Implements [`ProviderRepository`] and [`ModelRepository`] using
//! `SecureConn` / `AccessScope` for tenant-isolated database access. Every
//! query — read, write, list — is scoped so a caller can only see or modify
//! rows they are authorized for.

use async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::odata::sea_orm_filter::{paginate_odata, LimitCfg};
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt,
    secure_update_with_scope,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::ProviderRepository;
use crate::{
    CreateProviderRequestV1, ProviderV1, UpdateProviderRequestV1,
};

use super::entity::provider;
use super::mapper;
use super::odata_mapper::{ProviderFilterField, ProviderODataMapper};

// =============================================================================
// ScopeError → DomainError
// =============================================================================

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
            .filter(
                Condition::all()
                    .add(provider::Column::Slug.eq(req.slug())),
            )
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
            .filter(
                Condition::all()
                    .add(provider::Column::Slug.eq(req.slug())),
            )
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
        let result = provider::Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(provider::Column::Id.eq(id)))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        if result.rows_affected == 0 {
            return Err(DomainError::provider_not_found(id));
        }

        Ok(())
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
    use toolkit_db::{DbError, DBProvider, connect_db, ConnectOpts};

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
        let gts =
            gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
        CreateProviderRequestV1::builder(slug, name, gts).build()
    }

    fn make_full_create_req(slug: &str, name: &str) -> CreateProviderRequestV1 {
        let gts =
            gts::GtsTypeId::new("gts.cf.genai.models.provider.v1~cf.genai._.openai.v1~");
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

        let created = repo
            .create(&conn, &scope, tenant_id, &make_create_req("openai", "OpenAI"))
            .await
            .expect("create should succeed");

        let found = repo
            .find_by_id(&conn, &scope, created.id)
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

        let err = repo
            .find_by_id(&conn, &scope, Uuid::nil())
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

        let created = repo
            .create(
                &conn,
                &scope_for(tenant_a),
                tenant_a,
                &make_create_req("openai", "OpenAI"),
            )
            .await
            .expect("create should succeed");

        // Other tenant should not see this provider.
        let err = repo
            .find_by_id(&conn, &scope_for(tenant_b), created.id)
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

        let provider = repo
            .create(&conn, &scope, tenant_id, &make_create_req("openai", "OpenAI"))
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

        let provider = repo
            .create(
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

        repo.create(
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("first create should succeed");

        let err = repo
            .create(
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

        repo.create(
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("tenant A create should succeed");

        let provider_b = repo
            .create(
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

        let created = repo
            .create(
                &conn,
                &scope,
                tenant_id,
                &make_create_req("openai", "OpenAI"),
            )
            .await
            .expect("create should succeed");

        let updated = repo
            .update(
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

        let err = repo
            .update(
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

        let created = repo
            .create(
                &conn,
                &scope_for(tenant_a),
                tenant_a,
                &make_create_req("openai", "OpenAI"),
            )
            .await
            .expect("create should succeed");

        // Other tenant should not be able to update.
        let err = repo
            .update(
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

        let created = repo
            .create(
                &conn,
                &scope,
                tenant_id,
                &make_create_req("openai", "OpenAI"),
            )
            .await
            .expect("create should succeed");

        repo.delete(&conn, &scope, created.id)
            .await
            .expect("delete should succeed");

        // Verify it's gone.
        let err = repo
            .find_by_id(&conn, &scope, created.id)
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

        let err = repo
            .delete(&conn, &scope, Uuid::nil())
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

        let created = repo
            .create(
                &conn,
                &scope_for(tenant_a),
                tenant_a,
                &make_create_req("openai", "OpenAI"),
            )
            .await
            .expect("create should succeed");

        // Other tenant should not be able to delete.
        let err = repo
            .delete(&conn, &scope_for(tenant_b), created.id)
            .await
            .expect_err("should be not found for other tenant");

        assert!(
            matches!(&err, DomainError::ProviderNotFound { .. }),
            "expected ProviderNotFound, got {err:?}"
        );

        // Original tenant's provider should still exist.
        let found = repo
            .find_by_id(&conn, &scope_for(tenant_a), created.id)
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

        repo.create(
            &conn,
            &scope,
            tenant_id,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("create openai");
        repo.create(
            &conn,
            &scope,
            tenant_id,
            &make_create_req("anthropic", "Anthropic"),
        )
        .await
        .expect("create anthropic");

        let query = ODataQuery::default();
        let page = repo.list(&conn, &scope, &query).await.expect("list should succeed");

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

        repo.create(
            &conn,
            &scope_for(tenant_a),
            tenant_a,
            &make_create_req("openai", "OpenAI"),
        )
        .await
        .expect("tenant A create");

        // Tenant B should see no providers.
        let page = repo
            .list(&conn, &scope_for(tenant_b), &ODataQuery::default())
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

        repo.create(
            &conn,
            &scope,
            tenant_id,
            &CreateProviderRequestV1::builder("openai", "OpenAI", gts_openai).build(),
        )
        .await
        .expect("create openai");
        repo.create(
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

        let page = repo
            .list(&conn, &scope, &query)
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
            repo.create(
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

        let page = repo
            .list(&conn, &scope, &query)
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
}
