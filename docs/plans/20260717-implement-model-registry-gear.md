# Implement Model Registry Gear (P1)

## Overview

Implement the `model-registry` gear **implementation crate** (`gears/model-registry/model-registry/`) against the already-complete `model-registry-sdk`. The SDK defines the `ModelRegistryClientV1` trait, all model/provider types (`ModelV1<P>`, `ProviderV1`, request DTOs, GTS-typed provider settings), and `ModelRegistryError`. Only the implementation is missing.

**Scope: P1 only** (per DESIGN §3.3): Models read (get/list), Models CRUD (create/update/delete with direct approval status writes), Providers CRUD. P2 (discovery, OAGW, Approval Service) and P3 (health, aliases, tags) are explicitly out of scope and absent from the SDK trait.

**Problem it solves**: Model Registry is the authoritative catalog of AI models with tenant-level availability and approval status. LLM Gateway resolves model identifiers to provider routing/settings via this gear.

**Key design decisions** (confirmed with user + plan review):
- **Approval in P1**: authoritative write path is a local `model_approvals` table, written directly by admins via `update_model` — this preserves the P2 seam (P2 swaps only the write path to the Approval Service). Because the toolkit OData layer cannot join (see next bullet), `approval_status` is **also denormalized onto the `models` table** as a filterable/fast-read shadow column kept in sync on every approval write. Reads resolve `approval_status` from the `models` column; the `model_approvals` table remains the seam of record.
- **OData filtering uses real columns, not JSONB paths** (⚠️ resolved from plan review): `libs/toolkit-db/src/odata/sea_orm_filter.rs` maps each filter field to exactly one real SeaORM `Column` via `FieldToColumn::map_field` — there is **no JSONB-path filtering and no join support** (the `map_value` hook only does wire↔storage enum translation on a single column). Therefore every P1-filterable `info.*` field and `approval_status` is **denormalized into a real column** on `models`, populated from `info`/approval on write. `info`/`provider_settings` remain the full JSONB source of truth; the denormalized columns exist only to serve OData filtering and hot reads.
- **Cache**: define the `CacheService` trait and ship the `InMemoryCache` backend now. Redis is a feature-gated follow-up (not implemented this phase). Keeps `get_tenant_model` cache-first per DESIGN §2.1 without Redis infra.
- **Testing**: Regular (implement, then tests within the same task).

## Context (from discovery)

- **SDK (complete, do not modify unless a gap is found)**: `gears/model-registry/model-registry-sdk/src/` — `api.rs` (trait), `errors.rs`, `models/` (`entity.rs`, `info.rs`, `common.rs`, `default_parameters.rs`, `request.rs`, `providers/{openai,anthropic}.rs`).
- **Reference gear — wiring/CRUD**: `gears/simple-user-settings/simple-user-settings/src/` — `gear.rs` (`#[toolkit::gear(name, deps, capabilities=[rest,db])]`, `DatabaseCapability`, `RestApiCapability`), `config.rs`, `domain/{service,local_client,repo,error}.rs`, `infra/storage/{sea_orm_repo,entity,mapper,migrations}.rs`, `api/rest/{handlers,routes,dto,error}.rs`.
- **Reference gear — OData/JSONB/SecureConn**: `gears/chat-engine/chat-engine/src/` — `infra/db/odata_mapper.rs`, `infra/db/repo/session_repo.rs`, `infra/db/entity/*.rs` (JSONB columns), OData `Page<T>` list handlers in `api/rest/handlers/sessions.rs`.
- **Dependencies via ClientHub**: `tenant-resolver` (`TenantResolverClient` — `get_ancestors`, `is_ancestor` for additive inheritance + shadowing; SDK at `gears/system/tenant-resolver/tenant-resolver-sdk`, package `cf-gears-tenant-resolver-sdk`), `authz-resolver` (`AuthZResolverClient` + `PolicyEnforcer` as simple-user-settings uses; SDK at `gears/system/authz-resolver/authz-resolver-sdk`, package `cf-gears-authz-resolver-sdk`).
- **Toolkit primitives**: `toolkit::{Gear, GearCtx}`, `toolkit::gear` macro, `toolkit::contracts::{DatabaseCapability, RestApiCapability}`, `toolkit::api::{OperationBuilder, OpenApiRegistry}`, `toolkit_db::{DBProvider, DbError, secure::DBRunner}`, `toolkit_security::{SecurityContext, AccessScope}`, `toolkit_odata::{ODataQuery, Page}`, `toolkit_canonical_errors` (Problem/RFC-9457).
- **DB schema (DESIGN §3.6, adjusted for the OData-column constraint)**: `providers` (id, tenant_id, slug, name, gts_type, status, managed, metadata JSONB, discovery_enabled, discovery_interval_seconds, timestamps; unique (tenant_id, slug)); `models` (id, provider_id FK, tenant_id, canonical_id, lifecycle_status, deprecated_at, timestamps, `info` JSONB, `provider_settings` JSONB; unique (tenant_id, canonical_id)) **plus denormalized filterable columns** promoted from `info` (`gts_type`, `vendor`, `family`, `managed`, `architecture`, `format`, `provider_model_id`, `supported_api`, and boolean capability flags `cap_vision`, `cap_function_calling`, `cap_streaming`, `cap_reasoning_effort`) and a denormalized `approval_status` column; **new for P1**: `model_approvals` (tenant_id, model_id FK, approval_status, timestamps; PK (tenant_id, model_id)) as the write seam of record.
- **Workspace wiring**: root `Cargo.toml` members list (line ~52+), `apps/cf-gears-example-server/Cargo.toml` deps + `src/registered_gears.rs`.

## Development Approach

- **testing approach**: Regular (code first, then tests within the same task).
- complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task with code changes MUST include new/updated tests** (success + error/edge scenarios) as separate checklist items.
- **CRITICAL: all tests must pass before starting the next task**.
- Build/lint gate per task: `cargo build -p cf-gears-model-registry` and `cargo clippy -p cf-gears-model-registry --all-targets --all-features -- -D warnings -D clippy::perf` must be clean (repo denies `unwrap`/`expect` in non-test code, forbids `unsafe`, requires `SecureConn`/`AccessScope`).
- **CRITICAL: update this plan file when scope changes during implementation**.
- maintain backward compatibility with the SDK (do not break `ModelRegistryClientV1`).

## Testing Strategy

- **unit tests**: required for every task — mappers (round-trip JSONB), OData field mapping, cache behavior (TTL/isolation), service logic (inheritance, shadowing, approval resolve, cache-first), error mapping. Follow `#[cfg(test)] mod tests` and `*_test.rs` conventions already in the reference gears.
- **integration tests**: SeaORM repository tests against SQLite (`toolkit-db` `sqlite` feature; mirror `cargo test -p modkit-db --features "sqlite,integration"` style used elsewhere). Cover providers CRUD, models CRUD, OData list with `$filter`/`$top`/`$skip`, approval read/write, tenant scoping.
- **e2e**: skipped — the repo's e2e framework (`make e2e-local`) is not currently working. Integration tests cover the REST endpoints end-to-end against SQLite.
- treat integration tests with the same rigor as unit tests (must pass before next task).

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- keep this plan in sync with actual work done

## Solution Overview

Mirror the established DDD-light gear layout. Layering (DESIGN §1.3): REST handlers → `ModelRegistryService` (application) → `CacheService` + repository traits (domain) → SeaORM repo + entities + `InMemoryCache` (infra). `LocalClient` bridges the service to the `ModelRegistryClientV1` SDK trait and is registered in `ClientHub`.

Reads are cache-first with DB fallback and TTL by ownership (own 30 min, inherited 5 min). Provider/model visibility resolves additively over the tenant ancestor chain (from `tenant-resolver`), with child-tenant shadowing by slug. Approval status is resolved on read from `model_approvals` and written directly by `update_model` in P1. Provider-specific settings ride as GTS-typed JSONB (`info` + `provider_settings`), discriminated by `info.gts_type`.

## Technical Details

- **New crate**: package `cf-gears-model-registry`, lib name `model_registry`, path `gears/model-registry/model-registry/`.
- **Canonical ID**: `{provider_slug}::{provider_model_id}`, derived on `create_model`; immutable.
- **JSONB storage + denormalized columns**: `models.info` = serialized `ModelInfoV1<serde_json::Value>` common envelope (source of truth); `models.provider_settings` = flat per-provider JSON (raw `serde_json::Value`). The OData-filterable subset is **promoted to real columns** (see schema above) and rewritten from `info`/approval on every create/update. Regular B-tree indexes on the promoted columns (no Postgres GIN — keeps SQLite dev/test path working).
- **OData filterable fields** (DESIGN §3.3), each mapped to a **real column** via `FieldToColumn::map_field`: `lifecycle_status`, `approval_status`, `gts_type`, `supported_api`, `provider_model_id`, capability flags (`vision`, `function_calling`, `streaming`, `reasoning.effort`), `vendor`, `family`, `managed`, `architecture`, `format`. Use the `map_value` hook for enum-string↔storage translation where a column is stored as smallint/enum. `provider_settings` and `default_parameters` are NOT filterable in v1; non-allowlisted fields are rejected as validation errors.
- **`get_tenant_model` semantics** (⚠️ pinned from plan review vs SDK `api.rs`): returns the model with `approval_status` **populated** — it does **not** fail-closed on `pending`/`rejected`/`revoked`. The caller (LLM Gateway) decides. `ModelNotApproved` is reserved for a future explicit access-gate path, not the default read. `ModelDeprecated` is returned only when a soft-deleted model is fetched directly by canonical_id (deprecated models are still hidden from default `list_tenant_models`).
- **REST surface (P1)**: `GET/POST /model-registry/v1/models`, `GET/PATCH/DELETE /model-registry/v1/models/{canonical_id}`, `GET/POST /model-registry/v1/providers`, `GET/PATCH/DELETE /model-registry/v1/providers/{id}`.
- **Error mapping**: `ModelRegistryError` → `DomainError` → RFC-9457 `Problem`, covering **all 11 SDK variants**: `ModelNotFound`/`ProviderNotFound`→404, `ModelDeprecated`→404 (Gone semantics acceptable via 404), `ModelNotApproved`→403, `Forbidden`→403, `Unauthenticated`→401, `ProviderConflict`→409, `ProviderDisabled`→409, `InvalidTransition`→409, `Validation`→422, `Internal`→500.

## What Goes Where

- **Implementation Steps** (`[ ]`): all code, tests, docs achievable in this repo.
- **Post-Completion** (no checkboxes): LLM Gateway consumer integration, Redis backend, P2/P3 features, load/perf testing, deployment config.

## Implementation Steps

### Task 1: Scaffold implementation crate and wire into workspace + example server

**Files:**
- Create: `gears/model-registry/model-registry/Cargo.toml`
- Create: `gears/model-registry/model-registry/src/lib.rs`
- Create: `gears/model-registry/model-registry/src/gear.rs`
- Modify: `Cargo.toml` (workspace members)
- Modify: `apps/cf-gears-example-server/Cargo.toml`
- Modify: `apps/cf-gears-example-server/src/registered_gears.rs`

- [x] create `Cargo.toml` for `cf-gears-model-registry` (lib name `model_registry`), depending on `model-registry-sdk` and the two system SDKs by their renamed packages/paths (`tenant-resolver-sdk` → `package = "cf-gears-tenant-resolver-sdk", path = "../../system/tenant-resolver/tenant-resolver-sdk"`; `authz-resolver-sdk` → `package = "cf-gears-authz-resolver-sdk", path = "../../system/authz-resolver/authz-resolver-sdk"`), plus the toolkit crates used by simple-user-settings/chat-engine (`toolkit`, `toolkit-db` [sqlite], `toolkit-db-macros`, `toolkit-security`, `toolkit-odata`, `toolkit-canonical-errors` [axum], `toolkit-macros`), and `sea-orm`, `sea-orm-migration`, `axum`, `serde`, `serde_json`, `uuid`, `chrono`, `gts`, `async-trait`, `anyhow`, `thiserror`, `tracing`, `inventory`, `utoipa`
- [x] create `src/lib.rs` re-exporting the SDK trait/types and declaring `gear`, `config`, `domain`, `infra`, `api` modules (mirror simple-user-settings `lib.rs`)
- [x] create a minimal `src/gear.rs` with an empty `#[toolkit::gear(name = "model-registry", deps = ["tenant-resolver", "authz-resolver"], capabilities = [rest, db])]` struct that compiles (impls filled in later tasks)
- [x] add the crate to root `Cargo.toml` workspace members; wire into `apps/cf-gears-example-server` **feature-gated** like `chat-engine` — add `model-registry = ["dep:model_registry"]` feature + optional dep in its `Cargo.toml`, and `#[cfg(feature = "model-registry")] use model_registry as _;` in `registered_gears.rs`
- [x] write a unit test asserting `Gear::MODULE_NAME`/default construction (mirror simple-user-settings `gear.rs` tests)
- [x] run `cargo build -p cf-gears-model-registry` and workspace `cargo build` — must compile before next task

### Task 2: Config module

**Files:**
- Create: `gears/model-registry/model-registry/src/config.rs`

- [x] define `ModelRegistryConfig` (serde `Deserialize` + `Default`) with cache TTLs (`own_ttl_seconds` default 1800, `inherited_ttl_seconds` default 300) and any P1 tunables (e.g. `max_page_size`)
- [x] provide `#[serde(default = ...)]` defaults so `ctx.config_or_default()` works with no YAML
- [x] write tests for defaults and partial-YAML deserialization
- [x] run tests — must pass before next task

### Task 3: Domain errors and repository traits

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/mod.rs`
- Create: `gears/model-registry/model-registry/src/domain/error.rs`
- Create: `gears/model-registry/model-registry/src/domain/repo.rs`

- [x] define `DomainError` (thiserror) covering not-found/conflict/validation/forbidden/deprecated/not-approved/internal, with `From<DomainError> for ModelRegistryError` and `From` for DB errors
- [x] define `ProviderRepository` and `ModelRepository` traits over `&C: DBRunner` + `&AccessScope` (mirror `SettingsRepository`): provider get/list(OData)/create/update/delete; model get_by_canonical/list(OData)/create/update/soft_delete; approval get/set/delete
- [x] create **empty stub files** for `service.rs`, `local_client.rs`, `cache.rs`, `inheritance.rs` and declare all `mod`s in `domain/mod.rs` now, so the crate compiles at this task's build gate (later tasks fill the stubs)
- [x] write tests for the `DomainError` → `ModelRegistryError` mappings (each variant)
- [x] run tests — must pass before next task

### Task 4: SeaORM entities and migrations (providers, models, model_approvals)

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/mod.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/mod.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/entity/mod.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/entity/provider.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/entity/model.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/entity/model_approval.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/migrations/mod.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/migrations/initial_001.rs`

- [x] define SeaORM `Entity`/`Model`/`ActiveModel` for `providers`, `models` (with `info` + `provider_settings` JSONB columns **plus the denormalized filterable columns**: `gts_type`, `vendor`, `family`, `managed`, `architecture`, `format`, `provider_model_id`, `supported_api`, `approval_status`, and boolean capability flags `cap_vision`/`cap_function_calling`/`cap_streaming`/`cap_reasoning_effort`), `model_approvals` (mirror chat-engine JSONB entities)
- [x] write `initial_001` migration creating the three tables with columns/indexes per DESIGN §3.6 as adjusted (unique `(tenant_id, slug)`, unique `(tenant_id, canonical_id)`, FK `models.provider_id`→providers, FK+cascade `model_approvals.model_id`→models, `lifecycle_status` index, B-tree indexes on the denormalized filterable columns); use portable column types (JSONB on Postgres / JSON on SQLite via toolkit-db conventions); **no Postgres GIN indexes** (they break the SQLite dev/test path and are unnecessary now that filterable fields are real columns)
- [x] wire `Migrator` (`MigratorTrait`) listing `initial_001` in `migrations/mod.rs`
- [x] write a migration up/down test that applies against an in-memory SQLite DB
- [x] run tests — must pass before next task

### Task 5: Entity ↔ domain/SDK mappers (JSONB round-trip)

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/storage/mapper.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/mapper_test.rs`

- [x] implement mapping `provider::Model` ↔ `ProviderV1` (incl. `gts_type` string↔`GtsTypeId`, `status` enum, optional `metadata` JSONB)
- [x] implement mapping `model::Model` (+ resolved `ApprovalStatus`) ↔ `ModelV1<serde_json::Value>` — deserialize `info` JSONB into `ModelInfoV1`, attach `provider_settings` raw JSON, derive `canonical_id`
- [x] implement request → `ActiveModel` builders for create/update (PATCH semantics: only set provided fields; enforce immutability of `canonical_id`/`provider_slug`/`info.provider_model_id`/`info.gts_type`) **and project the denormalized filterable columns from `info`** (gts_type, vendor, family, managed, architecture, format, provider_model_id, supported_api, capability flags) so they stay in sync with the JSONB source of truth
- [x] write a test asserting the denormalized columns match the `info` JSONB after a create and after a PATCH that changes a promoted field
- [x] write round-trip tests (domain→entity→domain) for provider and model incl. OpenAI + Anthropic provider_settings and unknown-provider raw JSON
- [x] write tests for immutability rejection and malformed-JSONB error paths
- [x] run tests — must pass before next task

### Task 6: OData field mapping for models and providers

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/storage/odata_mapper.rs`

- [x] implement `FieldToColumn` for a model filter-field enum, mapping each allowed field to a **real `models` column** (the denormalized columns from Task 4): `lifecycle_status`, `approval_status`, `gts_type`, `supported_api`, `provider_model_id`, `vendor`, `family`, `managed`, `architecture`, `format`, and capability-flag columns; use `map_value` for any enum-string↔storage translation; reject non-allowlisted fields (incl. `provider_settings.*`, `default_parameters.*`, per-MIME array fields) with a validation error
- [x] implement `FieldToColumn` for a provider filter-field enum (`slug`, `name`, `status`, `gts_type`, `managed`, `discovery_enabled`)
- [x] apply `$top`/`$skip`/`$orderby`/`$select` via `toolkit_odata`/`toolkit-db` `paginate_odata` helpers (mirror chat-engine `odata_mapper.rs`) — mappers implement `ODataFieldMapping` for use with `paginate_odata`
- [x] write tests for allowed field translation (each field → column), rejected fields, `map_value` enum translation, and pagination bounds (default/max page size from config)
- [x] run tests — must pass before next task

### Task 7: SeaORM repository — providers CRUD (secure)

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/storage/sea_orm_repo.rs`

- [x] implement `ProviderRepository` for `SeaOrmRepository` using `SecureConn`/`AccessScope` (no raw `all/one/exec` — respect clippy disallowed-methods): get by id, list with OData, create (unique-slug conflict → `ProviderConflict`), update (PATCH; slug immutable), delete
- [x] enforce tenant scoping on every query via `AccessScope`
- [x] write integration tests (SQLite) for provider create/get/list(OData)/update/delete + slug-conflict + tenant-isolation (cross-tenant not visible)
- [x] run tests — must pass before next task

### Task 8: SeaORM repository — models CRUD, OData list, approvals

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/sea_orm_repo.rs`

- [ ] implement `ModelRepository` for `SeaOrmRepository`: get_by_canonical, list with OData (filtering entirely on `models` columns — no join), create (derive canonical_id; unique conflict), update (PATCH; immutable identity fields), soft-delete (set `lifecycle_status=deprecated`, `deprecated_at`)
- [ ] implement approval read/write (`get_approval`, `set_approval`, `delete_approval`) against `model_approvals` **and keep the denormalized `models.approval_status` column in sync** on every approval write (single transaction)
- [ ] resolve `approval_status` on model reads from the `models` column (default `Pending`); exclude deprecated from default list
- [ ] write integration tests (SQLite): model CRUD, OData `$filter` on `lifecycle_status`/`info.*`/`approval_status`, `$top`/`$skip`, soft-delete hiding, approval read/write/default, tenant isolation
- [ ] run tests — must pass before next task

### Task 9: CacheService trait and InMemoryCache backend

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/cache.rs`

- [ ] define `CacheService` trait: `get`, `set` (with TTL), `delete`, `invalidate_tenant`; key format `mr:{tenant_id}:{entity}:{id}` (DESIGN §3.6)
- [ ] implement `InMemoryCache` (TTL-aware, tenant-prefixed, `Send + Sync`); gate a future `RedisCache` behind a `redis` cargo feature (declared, not implemented — leave a `#[cfg(feature="redis")]` stub or TODO)
- [ ] write tests: set/get hit, TTL expiry (own vs inherited), delete, `invalidate_tenant` clears only that tenant's keys, cross-tenant isolation
- [ ] run tests — must pass before next task

### Task 10: Tenant inheritance resolver helper

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/inheritance.rs`
- Modify: `gears/model-registry/model-registry/src/domain/mod.rs`

- [ ] add a helper that, given `SecurityContext` + `TenantResolverClient`, returns the ancestor tenant chain and computes the additive visible set with child-shadowing by slug (providers) / canonical_id (models) per DESIGN §2.1 "Additive Inheritance"
- [ ] define ownership classification (own vs inherited) to drive cache TTL selection
- [ ] write tests with a mocked `TenantResolverClient`: additive union, child shadows parent by slug, child cannot expand beyond parent, single-tenant (no ancestors) case
- [ ] run tests — must pass before next task

### Task 11: ModelRegistryService — providers operations

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/service.rs`

- [ ] define `Service<R: Repository, C: CacheService>` (or generic params mirroring simple-user-settings `Service<Repo>`) holding db provider, repo, cache, tenant-resolver client, `PolicyEnforcer`, `ModelRegistryConfig`
- [ ] add a helper deriving `AccessScope` from `SecurityContext` (mirror simple-user-settings service) used by every repo call
- [ ] implement provider ops (get/list/create/update/delete) with authz (`PolicyEnforcer`), tenant scoping, inheritance resolution for reads, cache read-through/invalidation on writes, and validation (slug format/immutability, gts_type)
- [ ] write unit tests (mocked repo + cache + tenant-resolver): create/get/list/update/delete, cache hit vs miss, invalidation on write, authz-denied → `Forbidden`, slug-conflict → `ProviderConflict`
- [ ] run tests — must pass before next task

### Task 12: ModelRegistryService — models read (cache-first, inheritance, approval resolve)

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`

- [ ] implement `get_tenant_model` (cache-first with DB fallback, `approval_status` **populated on the returned model — NOT fail-closed on pending/rejected/revoked**, TTL by ownership) and `list_tenant_models` (OData + inheritance union + pagination). Return `ModelDeprecated` only when a soft-deleted model is fetched directly; `ModelNotFound` when absent
- [ ] populate cache on miss; select TTL from ownership (own vs inherited) via the Task 10 helper
- [ ] write unit tests: cache hit path, miss→DB→populate, not-found→`ModelNotFound`, deprecated-on-direct-get→`ModelDeprecated`, **pending/rejected model returned with populated `approval_status` (no error)**, inherited model visible with shorter TTL, list filtering + pagination
- [ ] run tests — must pass before next task

### Task 13: ModelRegistryService — models CRUD and approval writes

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`

- [ ] implement `create_model` (provider-exists check incl. inherited providers, canonical_id derivation, default approval `Pending`, optional initial `approval_status`), `update_model` (PATCH non-status fields directly + `approval_status` transition written directly to `model_approvals` in P1), `delete_model` (soft-delete)
- [ ] invalidate affected cache entries on every write; validate state transitions (`InvalidTransition` where illegal)
- [ ] write unit tests: create with/without initial approval, update fields, approve/reject/revoke via `approval_status`, invalid transition rejected, soft-delete, cache invalidation, provider-not-found on create
- [ ] run tests — must pass before next task

### Task 14: LocalClient implementing ModelRegistryClientV1

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/local_client.rs`

- [ ] implement `LocalClient` wrapping `Arc<Service>` and implementing all 10 `ModelRegistryClientV1` methods, mapping `DomainError` → `ModelRegistryError`
- [ ] write unit tests (mocked service) verifying each trait method delegates and maps errors correctly
- [ ] run tests — must pass before next task

### Task 15: REST DTOs and error mapping

**Files:**
- Create: `gears/model-registry/model-registry/src/api/mod.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/mod.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/dto.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/error.rs`

- [ ] define REST DTOs (serde + `utoipa::ToSchema`, DTOs only in `api/rest/`): provider create/update/response, model create/update/response, list responses (`Page<...>`); map to/from SDK request types and `ModelV1`/`ProviderV1`
- [ ] implement `ModelRegistryError` → RFC-9457 `Problem` mapping for **all 11 variants** per Technical Details (`ModelNotFound`/`ProviderNotFound`/`ModelDeprecated`→404, `ModelNotApproved`/`Forbidden`→403, `Unauthenticated`→401, `ProviderConflict`/`ProviderDisabled`/`InvalidTransition`→409, `Validation`→422, `Internal`→500) in `error.rs`
- [ ] write tests: DTO (de)serialization incl. OpenAI/Anthropic/unknown provider_settings; error→Problem status/type mapping asserting **every one of the 11 variants**
- [ ] run tests — must pass before next task

### Task 16: REST handlers and routes (providers + models)

**Files:**
- Create: `gears/model-registry/model-registry/src/api/rest/handlers.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/routes.rs`

- [ ] implement handlers for all 10 P1 endpoints (extract `SecurityContext`, parse `ODataQuery` for list endpoints, call service, map errors to `Problem`)
- [ ] register routes with `OperationBuilder` (`.authenticated()`, `.json_request`/`.json_response_with_schema`, `.error_4xx/5xx`, license feature) under `/model-registry/v1/...`, attach service via `Extension`, mirror simple-user-settings `routes.rs`
- [ ] write handler tests (success + error) using an in-memory service/repo, incl. OData query parsing and 404/403/409/422 paths
- [ ] run tests — must pass before next task

### Task 17: Gear wiring — init, DatabaseCapability, RestApiCapability, ClientHub registration

**Files:**
- Modify: `gears/model-registry/model-registry/src/gear.rs`

- [ ] implement `Gear::init` (load config, get `DBProvider`, build repo + cache + service, fetch `tenant-resolver` and `authz-resolver` clients from `ClientHub`, register `LocalClient` as `dyn ModelRegistryClientV1`)
- [ ] implement `DatabaseCapability::migrations` returning `Migrator::migrations()` and `RestApiCapability::register_rest` calling `routes::register_routes`
- [ ] write gear tests (default construction, migrations non-empty) mirroring simple-user-settings
- [ ] run `cargo build`/`clippy` for the crate — must pass before next task

### Task 18: End-to-end gear integration tests

**Files:**
- Create: `gears/model-registry/model-registry/tests/integration.rs`

- [ ] write an integration test booting the gear (or service+repo+cache) against SQLite: provider create → model create → get_tenant_model (cache-first) → list with OData filter → update approval → soft-delete; assert tenant isolation and inheritance across a parent/child tenant pair
- [ ] run `cargo test -p cf-gears-model-registry --features sqlite -- --nocapture` — must pass before next task

### Task 19: Verify acceptance criteria

- [ ] verify all P1 requirements from Overview/DESIGN §3.3 are implemented (10 endpoints, cache-first read, inheritance+shadowing, approval resolve/write, tenant isolation, OData filtering)
- [ ] verify edge cases: unknown-provider raw JSON round-trip, immutable-field rejection, deprecated hidden from default list, `get_tenant_model` returns pending/rejected models with populated status (no fail-closed), denormalized filterable columns stay in sync with `info` after PATCH, non-allowlisted OData field rejected
- [ ] run full workspace suite: `cargo test --workspace`
- [ ] run `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::perf`
- [ ] run `make dylint` (architectural lints: DTO placement, version prefix, GTS ids) and `make gts-docs` if GTS ids added

### Task 20: Update documentation and finalize

**Files:**
- Modify: `gears/model-registry/README.md`
- Modify: `gears/model-registry/docs/DESIGN.md` (check off implemented P1 drivers in §1.2)

- [ ] update `README.md` with the implemented P1 REST surface and build/run notes
- [ ] check off the implemented `p1` functional drivers in DESIGN §1.2; note the two P1 implementation deviations from the DESIGN: (a) approval stored in local `model_approvals` table + denormalized `models.approval_status`; (b) OData-filterable `info.*` fields denormalized to real columns with B-tree indexes instead of JSONB + GIN (toolkit OData layer maps to real columns only)
- [ ] update `CLAUDE.local.md`/CLAUDE.md only if a new reusable pattern emerged
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion
*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification**:
- Run the example server (`make example` / quickstart) with the gear enabled and exercise the REST endpoints manually (create provider → create model → get/list/approve/delete).
- Performance: DESIGN NFR `get_tenant_model` < 10ms P99 — benchmark under load once Redis backend lands.

**External system updates / follow-ups (out of P1 scope)**:
- **LLM Gateway integration**: consume `ModelRegistryClientV1` via ClientHub to resolve model routing.
- **Redis cache backend**: implement the feature-gated `RedisCache` and its integration tests.
- **P2**: model discovery via OAGW, Approval Service delegation (swap the P1 direct approval-write path), bulk approve, discovery trigger endpoint.
- **P3**: provider health monitoring, aliases, tags/model_tags + `tag` OData filter, degraded-mode, tenant reparenting cache invalidation.
- **P4**: user-group and user-level approval overrides.
