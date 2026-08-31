//! Local client implementing [`ModelRegistryClientV1`].
//!
//! Wraps [`Service`] behind the SDK trait, bridging the generic service layer
//! to the trait-object `ModelRegistryClientV1` registered in `ClientHub`.
//! Domain errors are mapped to SDK errors via `From<DomainError>`.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::repo::{ModelRepository, ProviderRepository};
use super::service::Service;
use crate::{
    CreateModelRequestV1, CreateProviderRequestV1, ModelManagementV1, ModelRegistryClientV1,
    ModelRegistryError, ModelV1, ProviderV1, UpdateModelRequestV1, UpdateProviderRequestV1,
};

/// Local client implementing [`ModelRegistryClientV1`].
///
/// Wraps an `Arc<Service>` and delegates every trait method to the
/// corresponding service method, mapping [`DomainError`] → [`ModelRegistryError`]
/// via the existing `From` impl.
#[domain_model]
pub struct LocalClient<R, M> {
    service: Arc<Service<R, M>>,
}

impl<R: ProviderRepository, M: ModelRepository> LocalClient<R, M> {
    /// Create a new `LocalClient` wrapping the given service.
    #[must_use]
    pub fn new(service: Arc<Service<R, M>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<R: ProviderRepository + Send + Sync, M: ModelRepository + Send + Sync> ModelRegistryClientV1
    for LocalClient<R, M>
{
    // ── Models — read ────────────────────────────────────────────────────

    async fn get_tenant_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, ModelRegistryError> {
        self.service
            .get_tenant_model(ctx, canonical_id)
            .await
            .map_err(Into::into)
    }

    async fn list_tenant_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, ModelRegistryError> {
        self.service
            .list_tenant_models(ctx, query)
            .await
            .map_err(Into::into)
    }

    async fn list_tenant_models_management(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        include_deprecated: bool,
    ) -> Result<Page<ModelManagementV1>, ModelRegistryError> {
        self.service
            .list_tenant_models_management(ctx, query, include_deprecated)
            .await
            .map_err(Into::into)
    }

    // ── Models — CRUD ───────────────────────────────────────────────────

    async fn get_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ModelV1, ModelRegistryError> {
        self.service.get_model(ctx, id).await.map_err(Into::into)
    }

    async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError> {
        self.service
            .create_model(ctx, &req)
            .await
            .map_err(Into::into)
    }

    async fn update_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError> {
        self.service
            .update_model(ctx, id, &req)
            .await
            .map_err(Into::into)
    }

    async fn delete_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), ModelRegistryError> {
        self.service.delete_model(ctx, id).await.map_err(Into::into)
    }

    // ── Providers ────────────────────────────────────────────────────────

    async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderV1, ModelRegistryError> {
        self.service.get_provider(ctx, id).await.map_err(Into::into)
    }

    async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, ModelRegistryError> {
        self.service
            .list_providers(ctx, query)
            .await
            .map_err(Into::into)
    }

    async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError> {
        self.service
            .create_provider(ctx, &req)
            .await
            .map_err(Into::into)
    }

    async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError> {
        self.service
            .update_provider(ctx, id, &req)
            .await
            .map_err(Into::into)
    }

    async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), ModelRegistryError> {
        self.service
            .delete_provider(ctx, id)
            .await
            .map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use toolkit_odata::ODataQuery;
    use toolkit_security::SecurityContext;
    use uuid::Uuid;

    use super::*;
    use crate::domain::cache::NoopResolutionCache;
    use crate::domain::error::DomainError;
    use crate::domain::repo::{ListVisibility, ModelRepository, ProviderRepository};
    use crate::{
        CreateModelRequestV1, CreateProviderRequestV1, ModelRegistryError, ModelV1, ProviderV1,
        UpdateModelRequestV1, UpdateProviderRequestV1,
    };
    use toolkit_db::ConnectOpts;

    // ═════════════════════════════════════════════════════════════════════════
    // Mock repos
    // ═════════════════════════════════════════════════════════════════════════

    #[domain_model]
    struct MockProviderRepo;

    #[async_trait]
    impl ProviderRepository for MockProviderRepo {
        async fn find_by_id(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: Uuid,
        ) -> Result<ProviderV1, DomainError> {
            Err(DomainError::provider_not_found(Uuid::nil()))
        }
        async fn find_all_by_slug(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: &str,
        ) -> Result<Vec<ProviderV1>, DomainError> {
            Ok(Vec::new())
        }
        async fn list(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: &ODataQuery,
        ) -> Result<toolkit_odata::Page<ProviderV1>, DomainError> {
            Err(DomainError::internal("not implemented"))
        }
        async fn list_all_for_tenant(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
        ) -> Result<Vec<ProviderV1>, DomainError> {
            Err(DomainError::internal("not implemented"))
        }
        async fn find_by_ids(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: &[Uuid],
        ) -> Result<Vec<ProviderV1>, DomainError> {
            Err(DomainError::internal("not implemented"))
        }
        async fn create(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: Uuid,
            _: &CreateProviderRequestV1,
        ) -> Result<ProviderV1, DomainError> {
            Err(DomainError::internal("not implemented"))
        }
        async fn update(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: Uuid,
            _: &UpdateProviderRequestV1,
        ) -> Result<ProviderV1, DomainError> {
            Err(DomainError::internal("not implemented"))
        }
        async fn delete(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: Uuid,
        ) -> Result<(), DomainError> {
            Err(DomainError::internal("not implemented"))
        }
    }

    #[domain_model]
    struct MockModelRepo;

    #[async_trait]
    impl ModelRepository for MockModelRepo {
        async fn find_by_id(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            id: Uuid,
        ) -> Result<ModelV1, DomainError> {
            Err(DomainError::model_not_found_by_id(id))
        }
        async fn find_by_canonical(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: &str,
        ) -> Result<ModelV1, DomainError> {
            Err(DomainError::model_not_found("nonexistent"))
        }
        async fn list(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: &ODataQuery,
            _: ListVisibility<'_>,
        ) -> Result<toolkit_odata::Page<ModelV1>, DomainError> {
            Err(DomainError::internal("not implemented"))
        }
        async fn create(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: Uuid,
            _: &ProviderV1,
            _: &CreateModelRequestV1,
        ) -> Result<ModelV1, DomainError> {
            Err(DomainError::internal("not implemented"))
        }
        async fn update(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: Uuid,
            _: &UpdateModelRequestV1,
        ) -> Result<ModelV1, DomainError> {
            Err(DomainError::internal("not implemented"))
        }
        async fn soft_delete(
            &self,
            _: &impl toolkit_db::secure::DBRunner,
            _: &toolkit_security::AccessScope,
            _: Uuid,
        ) -> Result<(), DomainError> {
            Err(DomainError::internal("not implemented"))
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Mock TenantResolverClient — returns no ancestors
    // ═════════════════════════════════════════════════════════════════════════

    #[domain_model]
    struct MockNoAncestors;

    #[async_trait]
    impl tenant_resolver_sdk::TenantResolverClient for MockNoAncestors {
        async fn get_tenant(
            &self,
            _: &SecurityContext,
            _: tenant_resolver_sdk::TenantId,
        ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError>
        {
            unimplemented!()
        }
        async fn get_root_tenant(
            &self,
            _: &SecurityContext,
        ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError>
        {
            unimplemented!()
        }
        async fn get_tenants(
            &self,
            _: &SecurityContext,
            _: &[tenant_resolver_sdk::TenantId],
            _: &tenant_resolver_sdk::GetTenantsOptions,
        ) -> Result<Vec<tenant_resolver_sdk::TenantInfo>, tenant_resolver_sdk::TenantResolverError>
        {
            unimplemented!()
        }
        async fn get_ancestors(
            &self,
            _: &SecurityContext,
            _: tenant_resolver_sdk::TenantId,
            _: &tenant_resolver_sdk::GetAncestorsOptions,
        ) -> Result<
            tenant_resolver_sdk::GetAncestorsResponse,
            tenant_resolver_sdk::TenantResolverError,
        > {
            Ok(tenant_resolver_sdk::GetAncestorsResponse {
                tenant: tenant_resolver_sdk::TenantRef {
                    id: tenant_resolver_sdk::TenantId(Uuid::nil()),
                    status: tenant_resolver_sdk::TenantStatus::Active,
                    tenant_type: None,
                    parent_id: None,
                    self_managed: false,
                },
                ancestors: vec![],
            })
        }
        async fn get_descendants(
            &self,
            _: &SecurityContext,
            _: tenant_resolver_sdk::TenantId,
            _: &tenant_resolver_sdk::GetDescendantsOptions,
        ) -> Result<
            tenant_resolver_sdk::GetDescendantsResponse,
            tenant_resolver_sdk::TenantResolverError,
        > {
            unimplemented!()
        }
        async fn is_ancestor(
            &self,
            _: &SecurityContext,
            _: tenant_resolver_sdk::TenantId,
            _: tenant_resolver_sdk::TenantId,
            _: &tenant_resolver_sdk::IsAncestorOptions,
        ) -> Result<bool, tenant_resolver_sdk::TenantResolverError> {
            unimplemented!()
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Mock AuthZResolverClient — permissive
    // ═════════════════════════════════════════════════════════════════════════

    #[domain_model]
    struct MockPermissiveAuthZ;

    #[async_trait]
    impl authz_resolver_sdk::AuthZResolverClient for MockPermissiveAuthZ {
        async fn evaluate(
            &self,
            request: authz_resolver_sdk::EvaluationRequest,
        ) -> Result<authz_resolver_sdk::EvaluationResponse, authz_resolver_sdk::AuthZResolverError>
        {
            // Extract the caller's tenant ID from subject properties.
            let tenant_id = request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .unwrap_or_else(Uuid::nil);

            Ok(authz_resolver_sdk::EvaluationResponse {
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

    // ═════════════════════════════════════════════════════════════════════════
    // Tests
    // ═════════════════════════════════════════════════════════════════════════

    /// Verify that every `DomainError` variant can be converted to
    /// `ModelRegistryError`. This is the core contract the `LocalClient` relies
    /// on via `.map_err(Into::into)` — already tested in `error.rs`, but we
    /// add a quick check here for coverage completeness.
    #[test]
    fn test_domain_error_to_sdk_error_mapping() {
        // Every variant (Database maps to Internal).
        let cases: Vec<(DomainError, &str)> = vec![
            (DomainError::model_not_found("m1"), "model not found: m1"),
            (
                DomainError::provider_not_found(Uuid::nil()),
                "provider not found: 00000000-0000-0000-0000-000000000000",
            ),
            (DomainError::model_deprecated("m1"), "model deprecated: m1"),
            (
                DomainError::model_not_approved("m1"),
                "model not approved for tenant: m1",
            ),
            (DomainError::forbidden("no access"), "forbidden: no access"),
            (
                DomainError::provider_disabled(Uuid::nil()),
                "provider disabled: 00000000-0000-0000-0000-000000000000",
            ),
            (
                DomainError::provider_conflict("openai"),
                "provider slug already exists: openai",
            ),
            (
                DomainError::invalid_transition("bad"),
                "invalid state transition: bad",
            ),
            (
                DomainError::validation("invalid"),
                "validation error: invalid",
            ),
            (DomainError::internal("oops"), "internal error: oops"),
        ];

        for (domain, expected_msg) in cases {
            let sdk: ModelRegistryError = domain.into();
            assert_eq!(sdk.to_string(), expected_msg);
        }
    }

    /// Verify `LocalClient` constructor works.
    #[tokio::test]
    async fn test_local_client_construction() {
        // We need a PolicyEnforcer — use the permissive mock.
        let enforcer = authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(MockPermissiveAuthZ));

        let raw_db = toolkit_db::connect_db("sqlite::memory:", ConnectOpts::default())
            .await
            .expect("in-memory SQLite");
        let db: toolkit_db::DBProvider<toolkit_db::DbError> = toolkit_db::DBProvider::new(raw_db);

        let service = Arc::new(crate::domain::service::Service::new(
            Arc::new(db),
            Arc::new(MockProviderRepo),
            Arc::new(MockModelRepo),
            Arc::new(NoopResolutionCache),
            Arc::new(MockNoAncestors),
            enforcer,
            crate::config::ModelRegistryConfig::default(),
        ));

        let client = LocalClient::new(service);
        // Verify it implements ModelRegistryClientV1 by calling a method.
        // Since we use mock repos that return errors, we should get an error
        // mapped through to ModelRegistryError.
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::nil())
            .build()
            .expect("security context");

        // get_tenant_model should return ModelNotFound (mapped from DomainError)
        let err = client
            .get_tenant_model(&ctx, "nonexistent")
            .await
            .expect_err("should return error");
        assert!(
            matches!(err, ModelRegistryError::ModelNotFound { .. }),
            "expected ModelNotFound, got: {err:?}"
        );

        // get_model is id-keyed and reports the id-carrying not-found variant,
        // distinct from the canonical_id one get_tenant_model returns.
        let err = client
            .get_model(&ctx, Uuid::nil())
            .await
            .expect_err("should return error");
        assert!(
            matches!(err, ModelRegistryError::ModelNotFoundById { .. }),
            "expected ModelNotFoundById, got: {err:?}"
        );

        // get_provider should return ProviderNotFound
        let err = client
            .get_provider(&ctx, Uuid::nil())
            .await
            .expect_err("should return error");
        assert!(
            matches!(err, ModelRegistryError::ProviderNotFound { .. }),
            "expected ProviderNotFound, got: {err:?}"
        );
    }
}
