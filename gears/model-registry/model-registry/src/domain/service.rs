//! Catalog service (DESIGN §3.2): the order of checks and side effects of
//! every operation, in the sequences of §3.6.

use std::sync::Arc;

use model_registry_sdk::{
    ApprovalState, ApprovalV1, CreateModelRequestV1, CreateProviderRequestV1, ModelEntryV1,
    ModelInfoV1, ModelV1, ProviderEntryV1, ProviderV1, SharingMode, UpdateModelRequestV1,
    UpdateProviderRequestV1,
};
use toolkit_db::DBProvider;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::access_guard::{
    AccessGuard, MODEL_RESOURCE, PROP_CANONICAL_ID, PROP_GTS_TYPE, PROP_PROVIDER_ID,
    PROVIDER_RESOURCE, actions,
};
use super::clock::Clock;
use super::error::DomainError;
use super::gts_validator::{self, SchemaSource};
use super::validation::{self, Limits};
use super::visibility::Catalog;
use crate::config::{ModelRegistryConfig, PageLimits};
use crate::infra::chain_cache::ChainCache;
use crate::infra::storage::mapper::{apply_model_update, apply_provider_update};
use crate::infra::storage::model_repo::ModelRepo;
use crate::infra::storage::provider_repo::ProviderRepo;

pub struct Service {
    db: Arc<DBProvider<DomainError>>,
    providers: ProviderRepo,
    models: ModelRepo,
    chain_cache: Arc<ChainCache>,
    guard: AccessGuard,
    schemas: Arc<dyn SchemaSource>,
    clock: Arc<dyn Clock>,
    page_limits: PageLimits,
    limits: Limits,
}

/// Canonical id split on the first `::`.
fn split_canonical_id(canonical_id: &str) -> Option<(&str, &str)> {
    let (slug, model_id) = canonical_id.split_once("::")?;
    (!slug.is_empty() && !model_id.is_empty()).then_some((slug, model_id))
}

fn empty_page<T>(limits: PageLimits, query: &ODataQuery) -> Page<T> {
    Page {
        items: Vec::new(),
        page_info: PageInfo {
            next_cursor: None,
            prev_cursor: None,
            limit: limits.clamp(query.limit),
        },
    }
}

impl Service {
    #[must_use]
    pub fn new(
        db: Arc<DBProvider<DomainError>>,
        chain_cache: Arc<ChainCache>,
        guard: AccessGuard,
        schemas: Arc<dyn SchemaSource>,
        clock: Arc<dyn Clock>,
        config: &ModelRegistryConfig,
    ) -> Self {
        let page_limits = config.page_limits();
        Self {
            db,
            providers: ProviderRepo::new(page_limits),
            models: ModelRepo::new(page_limits),
            chain_cache,
            guard,
            schemas,
            clock,
            page_limits,
            limits: Limits {
                max_json_document_bytes: config.max_json_document_bytes,
            },
        }
    }

    async fn chain(&self, ctx: &SecurityContext) -> Result<Arc<[Uuid]>, DomainError> {
        self.chain_cache.chain(ctx, ctx.subject_tenant_id()).await
    }

    async fn catalog(&self, conn: &impl DBRunner, chain: &[Uuid]) -> Result<Catalog, DomainError> {
        let candidates = self.providers.candidates(conn, chain).await?;
        Ok(Catalog::build(chain, candidates))
    }

    /// `InheritedReadOnly` when T0 can see the provider as an inherited
    /// candidate, otherwise `ProviderNotFound`.
    async fn missing_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        operation: &'static str,
    ) -> DomainError {
        let visible = async {
            let chain = self.chain(ctx).await?;
            let conn = self.db.conn()?;
            let catalog = self.catalog(&conn, &chain).await?;
            let row = self
                .providers
                .find_by_id(&conn, &AccessScope::for_tenants(chain.to_vec()), id)
                .await?;
            Ok::<bool, DomainError>(row.is_some_and(|p| catalog.provider_is_candidate(&p)))
        };
        match visible.await {
            Ok(true) => DomainError::InheritedReadOnly { operation, id },
            Ok(false) => DomainError::ProviderNotFound { id },
            Err(e) => e,
        }
    }

    /// `InheritedReadOnly` when T0 can see the model as an inherited
    /// candidate, otherwise `ModelNotFound`.
    async fn missing_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        operation: &'static str,
    ) -> DomainError {
        let visible = async {
            let chain = self.chain(ctx).await?;
            let conn = self.db.conn()?;
            let catalog = self.catalog(&conn, &chain).await?;
            let row = self
                .models
                .find_by_id(&conn, &AccessScope::for_tenants(chain.to_vec()), id)
                .await?;
            Ok::<bool, DomainError>(row.is_some_and(|m| catalog.model_is_candidate(&m)))
        };
        match visible.await {
            Ok(true) => DomainError::InheritedReadOnly { operation, id },
            Ok(false) => DomainError::model_not_found(id.to_string()),
            Err(e) => e,
        }
    }

    /// The entry of an own model: never inherited or shadowed.
    async fn own_model_entry(
        &self,
        conn: &impl DBRunner,
        model: ModelV1,
    ) -> Result<ModelEntryV1, DomainError> {
        let provider_enabled = self
            .providers
            .find_by_id(
                conn,
                &AccessScope::for_tenant(model.tenant_id),
                model.provider_id,
            )
            .await?
            .is_some_and(|p| p.enabled);
        let approved = model.approval.state == ApprovalState::Approved;
        Ok(ModelEntryV1 {
            inherited: false,
            shadowed: false,
            provider_enabled,
            available_for_eval: provider_enabled && approved,
            model,
        })
    }

    fn own_provider_entry(provider: ProviderV1) -> ProviderEntryV1 {
        ProviderEntryV1 {
            inherited: false,
            shadowed: false,
            available_for_eval: provider.enabled,
            provider,
        }
    }

    // ==================== Resolution ====================

    /// DESIGN §3.6 `seq-resolve-model`.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), canonical_id))]
    pub async fn resolve_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, DomainError> {
        const OP: &str = "resolve_model";
        let Some((slug, provider_model_id)) = split_canonical_id(canonical_id) else {
            return Err(DomainError::model_not_found(canonical_id));
        };
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let conn = self.db.conn()?;
        let candidates = self
            .providers
            .candidates_by_slug(&conn, &chain, slug)
            .await?;
        let catalog = Catalog::build(&chain, candidates);
        let Some(winner) = catalog.winner(slug) else {
            return Err(DomainError::model_not_found(canonical_id));
        };
        let model = self
            .models
            .find_for_resolution(&conn, &chain, winner.id, provider_model_id)
            .await?
            .filter(|m| m.tenant_id == t0 || m.sharing_mode == SharingMode::Shared)
            .ok_or_else(|| DomainError::model_not_found(canonical_id))?;
        if !winner.enabled {
            return Err(DomainError::ProviderDisabled {
                canonical_id: canonical_id.to_owned(),
            });
        }
        if model.approval.state != ApprovalState::Approved {
            return Err(DomainError::ModelNotApproved {
                canonical_id: canonical_id.to_owned(),
                state: model.approval.state,
            });
        }
        let decision = self
            .guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::EVALUATE,
                Some(model.id),
                &[
                    (
                        PROP_PROVIDER_ID,
                        serde_json::json!(model.provider_id.to_string()),
                    ),
                    (PROP_CANONICAL_ID, serde_json::json!(model.canonical_id)),
                    (
                        PROP_GTS_TYPE,
                        serde_json::json!(model.info.gts_type.to_string()),
                    ),
                ],
                &chain,
                OP,
            )
            .await?;
        if !decision.matches(&model) {
            return Err(DomainError::NotPermitted { operation: OP });
        }
        Ok(model)
    }

    /// DESIGN §3.6 `seq-list-available-models`.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id()))]
    pub async fn list_available_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, DomainError> {
        const OP: &str = "list_available_models";
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let conn = self.db.conn()?;
        let catalog = self.catalog(&conn, &chain).await?;
        self.guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::LIST_AVAILABLE,
                None,
                &[],
                &chain,
                OP,
            )
            .await?;
        let decision = match self
            .guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::EVALUATE,
                None,
                &[],
                &chain,
                OP,
            )
            .await
        {
            Ok(decision) => decision,
            Err(DomainError::NotPermitted { .. }) => {
                return Ok(empty_page(self.page_limits, query));
            }
            Err(e) => return Err(e),
        };
        let allow_list = catalog.allow_list();
        if allow_list.is_empty() {
            return Ok(empty_page(self.page_limits, query));
        }
        let conn = self.db.conn()?;
        self.models
            .list(
                &conn,
                &decision.scope(&chain),
                ModelRepo::available_condition(t0, &allow_list),
                query,
            )
            .await
    }

    // ==================== Providers ====================

    /// DESIGN §3.6 `seq-provider-write`, create.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), slug = req.slug()))]
    pub async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequestV1,
    ) -> Result<ProviderEntryV1, DomainError> {
        const OP: &str = "create_provider";
        validation::validate_create_provider(&req, self.limits)?;
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &PROVIDER_RESOURCE,
                actions::CREATE,
                None,
                &[],
                &chain,
                OP,
            )
            .await?;
        gts_validator::check_provider_type(req.gts_type())?;
        gts_validator::provider_type_schema(self.schemas.as_ref(), ctx, req.gts_type(), "gts_type")
            .await?;
        let now = self.clock.now();
        let subject = ctx.subject_id();
        let provider = ProviderV1 {
            id: Uuid::now_v7(),
            tenant_id: t0,
            slug: req.slug().to_owned(),
            name: req.name().to_owned(),
            gts_type: req.gts_type().clone(),
            enabled: req.enabled(),
            sharing_mode: req.sharing_mode(),
            managed: req.managed(),
            metadata: req.metadata().cloned(),
            created_at: now,
            created_by: subject,
            updated_at: now,
            updated_by: subject,
        };
        if !decision.matches(&provider) {
            return Err(DomainError::NotPermitted { operation: OP });
        }
        let repo = self.providers;
        let to_insert = provider.clone();
        self.db
            .transaction(move |tx| Box::pin(async move { repo.insert(tx, &to_insert).await }))
            .await?;
        tracing::info!(provider_id = %provider.id, %t0, %subject, "provider created");
        Ok(Self::own_provider_entry(provider))
    }

    /// DESIGN §3.6 `seq-management-read`, point read.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    pub async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderEntryV1, DomainError> {
        const OP: &str = "get_provider";
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &PROVIDER_RESOURCE,
                actions::GET,
                Some(id),
                &[],
                &chain,
                OP,
            )
            .await?;
        let conn = self.db.conn()?;
        let catalog = self.catalog(&conn, &chain).await?;
        let provider = self
            .providers
            .find_by_id(&conn, &AccessScope::for_tenants(chain.to_vec()), id)
            .await?
            .filter(|p| catalog.provider_is_candidate(p))
            .ok_or(DomainError::ProviderNotFound { id })?;
        if !decision.matches(&provider) {
            return Err(DomainError::NotPermitted { operation: OP });
        }
        Ok(catalog.provider_entry(provider))
    }

    /// DESIGN §3.6 `seq-management-read`, provider list.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id()))]
    pub async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderEntryV1>, DomainError> {
        const OP: &str = "list_providers";
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &PROVIDER_RESOURCE,
                actions::LIST,
                None,
                &[],
                &chain,
                OP,
            )
            .await?;
        let conn = self.db.conn()?;
        let catalog = self.catalog(&conn, &chain).await?;
        let page = self
            .providers
            .list(
                &conn,
                &decision.scope(&chain),
                ProviderRepo::management_condition(t0),
                query,
            )
            .await?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(|p| catalog.provider_entry(p))
                .collect(),
            page_info: page.page_info,
        })
    }

    /// DESIGN §3.6 `seq-provider-write`, update.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    pub async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
    ) -> Result<ProviderEntryV1, DomainError> {
        const OP: &str = "update_provider";
        validation::validate_update_provider(&req, self.limits)?;
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &PROVIDER_RESOURCE,
                actions::UPDATE,
                Some(id),
                &[],
                &chain,
                OP,
            )
            .await?;
        let subject = ctx.subject_id();
        let now = self.clock.now();
        let repo = self.providers;
        let updated = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let Some(mut provider) = repo.find_for_update(tx, t0, id).await? else {
                        return Ok(None);
                    };
                    if !decision.matches(&provider) {
                        return Err(DomainError::NotPermitted { operation: OP });
                    }
                    let before = (provider.enabled, provider.sharing_mode);
                    apply_provider_update(&mut provider, &req, subject, now);
                    let saved = repo.save(tx, &provider).await?;
                    Ok(Some((before, saved)))
                })
            })
            .await?;
        let Some(((enabled_before, mode_before), provider)) = updated else {
            return Err(self.missing_provider(ctx, id, OP).await);
        };
        tracing::info!(provider_id = %id, %t0, %subject, "provider updated");
        if enabled_before != provider.enabled {
            tracing::info!(provider_id = %id, %t0, %subject, old = enabled_before, new = provider.enabled, "provider enabled changed");
        }
        if mode_before != provider.sharing_mode {
            tracing::info!(provider_id = %id, %t0, %subject, old = mode_before.as_str(), new = provider.sharing_mode.as_str(), "provider sharing_mode changed");
        }
        Ok(Self::own_provider_entry(provider))
    }

    /// DESIGN §3.6 `seq-provider-write`, delete.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    pub async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        const OP: &str = "delete_provider";
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &PROVIDER_RESOURCE,
                actions::DELETE,
                Some(id),
                &[],
                &chain,
                OP,
            )
            .await?;
        let repo = self.providers;
        let deleted = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let Some(provider) = repo.find_for_update(tx, t0, id).await? else {
                        return Ok(false);
                    };
                    if !decision.matches(&provider) {
                        return Err(DomainError::NotPermitted { operation: OP });
                    }
                    repo.delete(tx, t0, id).await
                })
            })
            .await?;
        if !deleted {
            return Err(self.missing_provider(ctx, id, OP).await);
        }
        tracing::info!(provider_id = %id, %t0, subject = %ctx.subject_id(), "provider deleted");
        Ok(())
    }

    // ==================== Models ====================

    /// DESIGN §3.6 `seq-model-write`, create.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), provider_id = %req.provider_id))]
    pub async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelEntryV1, DomainError> {
        const OP: &str = "create_model";
        validation::validate_create_model(&req, self.limits)?;
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::CREATE,
                None,
                &[(
                    PROP_PROVIDER_ID,
                    serde_json::json!(req.provider_id.to_string()),
                )],
                &chain,
                OP,
            )
            .await?;
        let conn = self.db.conn()?;
        let provider = self
            .providers
            .find_by_id(&conn, &AccessScope::for_tenant(t0), req.provider_id)
            .await?;
        let Some(provider) = provider else {
            return Err(self.missing_provider(ctx, req.provider_id, OP).await);
        };
        let schema = gts_validator::provider_type_schema(
            self.schemas.as_ref(),
            ctx,
            &provider.gts_type,
            "provider_settings",
        )
        .await?;
        gts_validator::validate_settings(&schema, &req.provider_settings)?;

        let now = self.clock.now();
        let subject = ctx.subject_id();
        let model = ModelV1 {
            id: Uuid::now_v7(),
            tenant_id: t0,
            provider_id: provider.id,
            canonical_id: format!("{}::{}", provider.slug, req.provider_model_id),
            sharing_mode: req.sharing_mode.unwrap_or_default(),
            approval: ApprovalV1 {
                state: req.approval_state.unwrap_or(ApprovalState::Approved),
                decided_by: subject,
                decided_at: now,
            },
            info: ModelInfoV1 {
                gts_type: provider.gts_type.clone(),
                provider_model_id: req.provider_model_id,
                base_url: req.base_url,
                secret_ref: req.secret_ref,
                display_name: req.display_name,
                description: req.description,
                family: req.family,
                vendor: req.vendor,
                managed: req.managed,
                architecture: req.architecture,
                size_bytes: req.size_bytes,
                format: req.format,
                region: req.region,
                hosted_by: req.hosted_by,
                last_release_at: req.last_release_at,
                reasoning_level: req.reasoning_level,
                version: req.version,
                sort_order: req.sort_order,
                icon: req.icon,
                multiplier_display: req.multiplier_display,
                performance: req.performance,
                additional_info: req.additional_info,
                supported_api: req.supported_api,
                capabilities: req.capabilities,
                disabled_capabilities: req.disabled_capabilities,
                context_window: req.context_window,
                default_parameters: req.default_parameters,
                allow_parameter_override: req.allow_parameter_override,
                allow_extra_params: req.allow_extra_params,
                provider_settings: req.provider_settings,
            },
            created_at: now,
            created_by: subject,
            updated_at: now,
            updated_by: subject,
        };
        if !decision.matches(&model) {
            return Err(DomainError::NotPermitted { operation: OP });
        }
        let repo = self.models;
        let to_insert = model.clone();
        self.db
            .transaction(move |tx| Box::pin(async move { repo.insert(tx, &to_insert).await }))
            .await?;
        tracing::info!(model_id = %model.id, canonical_id = %model.canonical_id, %t0, %subject, approval = model.approval.state.as_str(), "model created");
        let approved = model.approval.state == ApprovalState::Approved;
        Ok(ModelEntryV1 {
            inherited: false,
            shadowed: false,
            provider_enabled: provider.enabled,
            available_for_eval: provider.enabled && approved,
            model,
        })
    }

    /// DESIGN §3.6 `seq-management-read`, point read.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    pub async fn get_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ModelEntryV1, DomainError> {
        const OP: &str = "get_model";
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::GET,
                Some(id),
                &[],
                &chain,
                OP,
            )
            .await?;
        let conn = self.db.conn()?;
        let catalog = self.catalog(&conn, &chain).await?;
        let model = self
            .models
            .find_by_id(&conn, &AccessScope::for_tenants(chain.to_vec()), id)
            .await?
            .filter(|m| catalog.model_is_candidate(m))
            .ok_or_else(|| DomainError::model_not_found(id.to_string()))?;
        if !decision.matches(&model) {
            return Err(DomainError::NotPermitted { operation: OP });
        }
        catalog
            .model_entry(model)
            .ok_or_else(|| DomainError::internal("candidate model without a candidate provider"))
    }

    /// DESIGN §3.6 `seq-management-read`, admin list.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id()))]
    pub async fn list_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelEntryV1>, DomainError> {
        const OP: &str = "list_models";
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(ctx, &MODEL_RESOURCE, actions::LIST, None, &[], &chain, OP)
            .await?;
        let conn = self.db.conn()?;
        let catalog = self.catalog(&conn, &chain).await?;
        let page = self
            .models
            .list(
                &conn,
                &decision.scope(&chain),
                ModelRepo::management_condition(t0, &catalog.inherited_candidate_ids()),
                query,
            )
            .await?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .filter_map(|m| catalog.model_entry(m))
                .collect(),
            page_info: page.page_info,
        })
    }

    /// DESIGN §3.6 `seq-model-write`, update.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    pub async fn update_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateModelRequestV1,
    ) -> Result<ModelEntryV1, DomainError> {
        const OP: &str = "update_model";
        validation::validate_update_model(&req, self.limits)?;
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::UPDATE,
                Some(id),
                &[],
                &chain,
                OP,
            )
            .await?;
        let conn = self.db.conn()?;
        let current = self
            .models
            .find_by_id(&conn, &AccessScope::for_tenant(t0), id)
            .await?;
        let Some(current) = current else {
            return Err(self.missing_model(ctx, id, OP).await);
        };
        if !decision.matches(&current) {
            return Err(DomainError::NotPermitted { operation: OP });
        }
        if let Some(settings) = &req.provider_settings {
            let schema = gts_validator::provider_type_schema(
                self.schemas.as_ref(),
                ctx,
                &current.info.gts_type,
                "provider_settings",
            )
            .await?;
            gts_validator::validate_settings(&schema, settings)?;
        }
        let subject = ctx.subject_id();
        let now = self.clock.now();
        let repo = self.models;
        let updated = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let Some(mut model) = repo.find_for_update(tx, t0, id).await? else {
                        return Ok(None);
                    };
                    let before = (model.approval.state, model.sharing_mode);
                    apply_model_update(&mut model, &req, subject, now);
                    let saved = repo.save(tx, &model).await?;
                    Ok(Some((before, saved)))
                })
            })
            .await?;
        let Some(((state_before, mode_before), model)) = updated else {
            return Err(self.missing_model(ctx, id, OP).await);
        };
        tracing::info!(model_id = %id, %t0, %subject, "model updated");
        if state_before != model.approval.state {
            tracing::info!(model_id = %id, %t0, %subject, old = state_before.as_str(), new = model.approval.state.as_str(), "model approval changed");
        }
        if mode_before != model.sharing_mode {
            tracing::info!(model_id = %id, %t0, %subject, old = mode_before.as_str(), new = model.sharing_mode.as_str(), "model sharing_mode changed");
        }
        let conn = self.db.conn()?;
        self.own_model_entry(&conn, model).await
    }

    /// DESIGN §3.6 `seq-model-write`, delete.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    pub async fn delete_model(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        const OP: &str = "delete_model";
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::DELETE,
                Some(id),
                &[],
                &chain,
                OP,
            )
            .await?;
        let repo = self.models;
        let deleted = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let Some(model) = repo.find_for_update(tx, t0, id).await? else {
                        return Ok(false);
                    };
                    if !decision.matches(&model) {
                        return Err(DomainError::NotPermitted { operation: OP });
                    }
                    repo.delete(tx, t0, id).await
                })
            })
            .await?;
        if !deleted {
            return Err(self.missing_model(ctx, id, OP).await);
        }
        tracing::info!(model_id = %id, %t0, subject = %ctx.subject_id(), "model deleted");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::split_canonical_id;

    #[test]
    fn canonical_id_splits_on_the_first_separator() {
        assert_eq!(
            split_canonical_id("openai::gpt-4o"),
            Some(("openai", "gpt-4o"))
        );
        assert_eq!(split_canonical_id("a::b::c"), Some(("a", "b::c")));
        assert_eq!(split_canonical_id("openai"), None);
        assert_eq!(split_canonical_id("::gpt"), None);
        assert_eq!(split_canonical_id("openai::"), None);
    }
}
