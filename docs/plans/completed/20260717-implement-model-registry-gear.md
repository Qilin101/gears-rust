# Implement Model Registry Gear (P1)

> **Merged plan.** This file consolidates four incremental plans into the final
> shape of the work:
> - `20260717-implement-model-registry-gear` — original P1 implementation
> - `20260724-drop-models-info-jsonb` — dropped the `models.info` JSONB column and
>   promoted every `ModelInfoV1` field to typed columns / small JSONB sub-objects
> - `20260724-model-registry-from-dto-conversions` — dropped `#[non_exhaustive]`
>   from the SDK entity structs and replaced the handler serde round-trips with
>   `From` impls
> - `20260727-split-sea-orm-repo` — split `SeaOrmRepository` into per-trait
>   `ProviderRepositoryImpl` / `ModelRepositoryImpl` (Task 22 below)
>
> Everything below describes the **final** design, not the intermediate states.

## Overview

Implement the `model-registry` gear **implementation crate** (`gears/model-registry/model-registry/`) against the already-complete `model-registry-sdk`. The SDK defines the `ModelRegistryClientV1` trait, all model/provider types (`ModelV1<P>`, `ProviderV1`, request DTOs, GTS-typed provider settings), and `ModelRegistryError`. Only the implementation is missing (plus one small SDK adjustment — Task 1).

**Scope: P1 only** (per DESIGN §3.3): Models read (get/list), Models CRUD (create/update/delete with direct approval status writes), Providers CRUD. P2 (discovery, OAGW, Approval Service) and P3 (health, aliases, tags) are explicitly out of scope and absent from the SDK trait.

**Problem it solves**: Model Registry is the authoritative catalog of AI models with tenant-level availability and approval status. LLM Gateway resolves model identifiers to provider routing/settings via this gear.

**Key design decisions** (confirmed with user + plan review):
- **Approval in P1**: authoritative write path is a local `model_approvals` table, written directly by admins via `update_model` — this preserves the P2 seam (P2 swaps only the write path to the Approval Service). Because the toolkit OData layer cannot join (see next bullet), `approval_status` is **also denormalized onto the `models` table** as a filterable/fast-read shadow column kept in sync on every approval write. Reads resolve `approval_status` from the `models` column; the `model_approvals` table remains the seam of record.
- **OData filtering uses real columns, not JSONB paths**: `libs/toolkit-db/src/odata/sea_orm_filter.rs` maps each filter field to exactly one real SeaORM `Column` via `FieldToColumn::map_field` — there is **no JSONB-path filtering and no join support** (the `map_value` hook only does wire↔storage enum translation on a single column). Therefore every P1-filterable field is a real column on `models`.
- **No `info` JSONB column at all**: taken to its logical end — `ModelInfoV1` is *not* stored as one JSONB blob. Every field that promotes cleanly is a typed scalar column (17 of them); the rest live in five small JSONB sub-object columns. The polymorphic `provider_settings` JSONB stays, discriminated by the top-level scalar `gts_type`. The public SDK `ModelInfoV1<P>` and the wire DTO `ModelDto { info: JsonValue }` are unaffected — only storage layout differs.
- **SDK entity structs are not `#[non_exhaustive]`**: the 5 entity structs (`ModelV1`, `ProviderV1`, `ModelInfoV1`, `ModelCapabilities`, `DisabledCapabilities`) drop the attribute so the gear can write plain `impl From<SdkType> for Dto` (the idiom used by ~13 other REST gears) instead of `serde_json::from_value(serde_json::to_value(...))` round-trips in every handler. The 6 **enums** and `ModelRegistryError` keep `#[non_exhaustive]`.
- **Cache**: define the `CacheService` trait and ship the `InMemoryCache` backend now. Redis is a feature-gated follow-up (not implemented this phase). Keeps `get_tenant_model` cache-first per DESIGN §2.1 without Redis infra.
- **Testing**: Regular (implement, then tests within the same task).

## Context (from discovery)

- **SDK**: `gears/model-registry/model-registry-sdk/src/` — `api.rs` (trait), `errors.rs`, `models/` (`entity.rs`, `info.rs`, `common.rs`, `default_parameters.rs`, `request.rs`, `providers/{openai,anthropic}.rs`). Only change: drop `#[non_exhaustive]` from the 5 entity structs (Task 1).
- **Reference gear — wiring/CRUD**: `gears/simple-user-settings/simple-user-settings/src/` — `gear.rs` (`#[toolkit::gear(name, deps, capabilities=[rest,db])]`, `DatabaseCapability`, `RestApiCapability`), `config.rs`, `domain/{service,local_client,repo,error}.rs`, `infra/storage/{sea_orm_repo,entity,mapper,migrations}.rs`, `api/rest/{handlers,routes,dto,error}.rs`.
- **Reference gear — OData/SecureConn**: `gears/chat-engine/chat-engine/src/` — `infra/db/odata_mapper.rs`, `infra/db/repo/session_repo.rs`, OData `Page<T>` list handlers in `api/rest/handlers/sessions.rs`.
- **Reference for the DTO idiom**: `docs/toolkit_unified_system/04_rest_operation_builder.md:182-197` documents `UserDto::from(user)` as the canonical conversion; also `file-parser/mappers.rs`, `usage-collector/dto.rs`, `bss/ledger/dto.rs`.
- **Dependencies via ClientHub**: `tenant-resolver` (`TenantResolverClient` — `get_ancestors`, `is_ancestor` for additive inheritance + shadowing; package `cf-gears-tenant-resolver-sdk`), `authz-resolver` (`AuthZResolverClient` + `PolicyEnforcer`; package `cf-gears-authz-resolver-sdk`).
- **Toolkit primitives**: `toolkit::{Gear, GearCtx}`, `toolkit::gear` macro, `toolkit::contracts::{DatabaseCapability, RestApiCapability}`, `toolkit::api::{OperationBuilder, OpenApiRegistry}`, `toolkit_db::{DBProvider, DbError, secure::DBRunner}`, `toolkit_security::{SecurityContext, AccessScope}`, `toolkit_odata::{ODataQuery, Page}`, `toolkit_canonical_errors` (Problem/RFC-9457).
- **External consumers**: `llm-gateway-sdk` (`src/models/plugin.rs`) and `llm-gateway-demo` (`src/mock_registry.rs`) use `ModelInfoV1` as an opaque aggregate. Neither the storage change nor the `#[non_exhaustive]` removal breaks them.
- **Workspace wiring**: root `Cargo.toml` members list, `apps/cf-gears-example-server/Cargo.toml` deps + `src/registered_gears.rs`.

## DB schema (DESIGN §3.6, as implemented)

### `providers`
`id`, `tenant_id`, `slug`, `name`, `gts_type`, `status`, `managed`, `metadata` JSONB, `discovery_enabled`, `discovery_interval_seconds`, `created_at`, `updated_at`. PK `(id)`, unique `(tenant_id, slug)`.

### `models`
**Identity / lifecycle**: `id`, `provider_id` (FK → `providers`, ON DELETE RESTRICT), `tenant_id`, `canonical_id`, `lifecycle_status`, `deprecated_at`, `created_at`, `updated_at`. PK `(id)`, unique `(tenant_id, canonical_id)`.

**17 promoted scalar columns** (replacing the `info` JSONB — this is the source of truth):

| Column | Type (PG / MySQL / SQLite) | Source field | Nullable |
|---|---|---|---|
| `display_name` | TEXT | `info.display_name` | NOT NULL DEFAULT `''` |
| `description` | TEXT | `info.description` | NULL |
| `size_bytes` | BIGINT / BIGINT / INTEGER | `info.size_bytes` | NULL |
| `region` | VARCHAR(64) / VARCHAR(64) / TEXT | `info.region` | NULL |
| `hosted_by` | VARCHAR(64) / VARCHAR(64) / TEXT | `info.hosted_by` | NULL |
| `last_release_at` | TIMESTAMPTZ / DATETIME(6) / TEXT | `info.last_release_at` | NULL |
| `reasoning_level` | VARCHAR(64) / VARCHAR(64) / TEXT | `info.reasoning_level` | NULL |
| `version` | VARCHAR(64) / VARCHAR(64) / TEXT | `info.version` | NULL |
| `sort_order` | BIGINT / BIGINT / INTEGER | `info.sort_order` | NULL |
| `icon` | TEXT | `info.icon` | NULL |
| `multiplier_display` | VARCHAR(64) / VARCHAR(64) / TEXT | `info.multiplier_display` | NULL |
| `perf_response_latency_ms` | BIGINT / BIGINT / INTEGER | `info.performance.response_latency_ms` | NULL |
| `perf_tokens_per_second` | BIGINT / BIGINT / INTEGER | `info.performance.tokens_per_second` | NULL |
| `ctx_max_input_tokens` | BIGINT / BIGINT / INTEGER | `info.context_window.max_input_tokens` | NOT NULL DEFAULT `0` |
| `ctx_max_output_tokens` | BIGINT / BIGINT / INTEGER | `info.context_window.max_output_tokens` | NULL |
| `ctx_output_vector_size` | BIGINT / BIGINT / INTEGER | `info.context_window.output_vector_size` | NULL |
| `allow_parameter_override` | BOOLEAN | `info.allow_parameter_override` | NOT NULL DEFAULT `0` |

`INTEGER` is 32-bit on Postgres/MySQL and overflows for `size_bytes` ≥ 2 GiB (smaller than any modern weight file), so integer columns use `BIGINT` there; SQLite `INTEGER` is already 8-byte.

**5 JSONB sub-object columns** (fields that don't promote cleanly), all nullable, backend-dispatched `JSONB` / `JSON` / `TEXT`:

| Column | Holds |
|---|---|
| `capabilities_full` | `ModelCapabilities` minus the 4 promoted booleans (vision MIME types, reasoning toggle/resume/budget, response_schema, file_input, image_generation, audio_input, audio_output, code_interpreter, web_search) |
| `default_parameters` | `DefaultInferenceParametersV1` |
| `additional_info` | `HashMap<String, serde_json::Value>` forward-compat escape hatch |
| `disabled_capabilities_full` | `DisabledCapabilities` (symmetric with `capabilities_full`) |
| `allow_extra_params` | flat `Vec<String>` of caller-supplied parameter names permitted alongside the request |

**`provider_settings`** JSONB (nullable) — the only remaining polymorphic blob, keyed by the top-level scalar `gts_type`.

**13 OData/denormalized columns**: `gts_type`, `vendor`, `family`, `managed`, `architecture`, `format`, `provider_model_id`, `supported_api`, `approval_status` (NOT NULL DEFAULT `'pending'`), `cap_vision`, `cap_function_calling`, `cap_streaming`, `cap_reasoning_effort`.

**13 B-tree indexes** on `lifecycle_status`, `approval_status`, `gts_type`, `vendor`, `family`, `architecture`, `format`, `provider_model_id`, `supported_api`, and the 4 `cap_*` flags. **No Postgres GIN indexes** — they would break the SQLite dev/test path and are unnecessary now that every filterable field is a real column.

### `model_approvals`
`tenant_id`, `model_id` (FK → `models`, ON DELETE CASCADE), `approval_status`, `created_at`, `updated_at`. PK `(tenant_id, model_id)`. The write seam of record for P2.

## Development Approach

- **testing approach**: Regular (code first, then tests within the same task).
- complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task with code changes MUST include new/updated tests** (success + error/edge scenarios) as separate checklist items.
- **CRITICAL: all tests must pass before starting the next task**.
- Build/lint gate per task: `cargo build -p cf-gears-model-registry` and `cargo clippy -p cf-gears-model-registry --all-targets --all-features -- -D warnings -D clippy::perf` must be clean (repo denies `unwrap`/`expect` in non-test code, forbids `unsafe`, requires `SecureConn`/`AccessScope`).
- **CRITICAL: update this plan file when scope changes during implementation**.
- maintain backward compatibility with the SDK trait (`ModelRegistryClientV1`) and the REST wire format.

## Testing Strategy

- **unit tests**: required for every task — mappers (column↔`ModelInfoV1` round-trip, capability merge, write-path projection), OData field mapping, cache behavior (TTL/isolation), service logic (inheritance, shadowing, approval resolve, cache-first), DTO `From` impls, error mapping. Follow `#[cfg(test)] mod tests` and `*_test.rs` conventions already in the reference gears.
- **integration tests**: SeaORM repository tests against SQLite (`toolkit-db` `sqlite` feature). Cover providers CRUD, models CRUD, OData list with `$filter`/`$top`/`$skip`, approval read/write, tenant scoping, and the full storage round-trip (create → read → patch → delete).
- **consumer regression**: `cargo build -p cf-gears-llm-gateway-sdk -p cf-gears-llm-gateway-demo` after the SDK change.
- **e2e**: skipped — the repo's e2e framework (`make e2e-local`) is not currently working. Integration tests cover the REST endpoints end-to-end against SQLite.
- treat integration tests with the same rigor as unit tests (must pass before next task).

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- keep this plan in sync with actual work done

## Solution Overview

Mirror the established DDD-light gear layout. Layering (DESIGN §1.3): REST handlers → `ModelRegistryService` (application) → `CacheService` + repository traits (domain) → SeaORM repo + entities + `InMemoryCache` (infra). `LocalClient` bridges the service to the `ModelRegistryClientV1` SDK trait and is registered in `ClientHub`.

Reads are cache-first with DB fallback and TTL by ownership (own 30 min, inherited 5 min). Provider/model visibility resolves additively over the tenant ancestor chain (from `tenant-resolver`), with child-tenant shadowing by slug. Approval status is resolved on read from the denormalized `models.approval_status` column and written directly to `model_approvals` (plus the shadow column) by `update_model` in P1.

Storage keeps no `info` blob: the mapper reconstructs the in-memory `ModelInfoV1` on read by stitching the 17 scalar columns, the 5 JSONB sub-objects, and `provider_settings` into a `serde_json::json!{…}` value, then `serde_json::from_value`. The write path re-projects every column from `req.info.*`.

REST handlers convert SDK entities to DTOs with plain `From` impls — no serde round-trips.

## Technical Details

- **New crate**: package `cf-gears-model-registry`, lib name `model_registry`, path `gears/model-registry/model-registry/`.
- **Canonical ID**: `{provider_slug}::{provider_model_id}`, derived on `create_model`; immutable.
- **Read path** (`mapper.rs`): `model_entity_to_v1` → `build_model_info_v1` assembles the JSON value from columns + JSONB sub-objects and deserializes into `ModelInfoV1`. Helpers: `build_capabilities` (merges the 4 scalar booleans with `capabilities_full` — **columns are authoritative**), `merge_disabled_capabilities`, `merge_default_parameters`, `supported_api_denorm_to_json_array`. `build_minimal_info` is retained as a defensive graceful-degradation fallback for rows sitting on DB defaults (`display_name = ''`, `ctx_max_input_tokens = 0`).
- **Write path** (`mapper.rs`): `model_create_active_model` / `model_update_active_model` `Set(...)` every scalar column plus the 5 JSONB sub-objects (`build_capabilities_full_for_create` strips the 4 promoted booleans). Any PATCH touching an `info.*` field re-projects all columns — verbose SQL, but correct; optimization is out of scope. `apply_info_patches` operates on the in-memory `ModelInfoV1` and is storage-independent. Create/update enforce immutability of `canonical_id` / `provider_slug` / `info.provider_model_id` / `info.gts_type`.
- **OData filterable fields** (15, DESIGN §3.3), each mapped to a real column via `FieldToColumn::map_field`: `canonical_id`, `lifecycle_status`, `approval_status`, `gts_type`, `supported_api`, `provider_model_id`, `vendor`, `family`, `managed`, `architecture`, `format`, and the capability flags `vision`, `function_calling`, `streaming`, `reasoning.effort`. The `map_value` hook does enum-string↔storage translation. `provider_settings`, `default_parameters`, `capabilities_full` and per-MIME array fields are NOT filterable; non-allowlisted fields are rejected as validation errors.
- **`get_tenant_model` semantics** (pinned against SDK `api.rs`): returns the model with `approval_status` **populated** — it does **not** fail-closed on `pending`/`rejected`/`revoked`. The caller (LLM Gateway) decides. `ModelNotApproved` is reserved for a future explicit access-gate path, not the default read. `ModelDeprecated` is returned only when a soft-deleted model is fetched directly by canonical_id (deprecated models are still hidden from default `list_tenant_models`).
- **REST surface (P1)**: `GET/POST /model-registry/v1/models`, `GET/PATCH/DELETE /model-registry/v1/models/{canonical_id}`, `GET/POST /model-registry/v1/providers`, `GET/PATCH/DELETE /model-registry/v1/providers/{id}`.
- **DTO conversion**: `impl From<ProviderV1> for ProviderDto` and `impl<P> From<ModelV1<P>> for ModelDto` in `api/rest/dto.rs`. Enum→`String` mapping is done inline with `match` + `_ =>` wildcard (the enums stay `#[non_exhaustive]`); duplicating four match expressions is cheaper than a shared module. `ModelDto.info: JsonValue` comes from `serde_json::to_value(&source.info).unwrap_or(JsonValue::Null)` — `From` cannot return `Result`, and `ModelInfoV1` is fully-owned data with no re-serialization failure mode.
- **Error mapping**: `ModelRegistryError` → `DomainError` → RFC-9457 `Problem`, covering **all 11 SDK variants**: `ModelNotFound`/`ProviderNotFound`→404, `ModelDeprecated`→404 (Gone semantics acceptable via 404), `ModelNotApproved`→403, `Forbidden`→403, `Unauthenticated`→401, `ProviderConflict`→409, `ProviderDisabled`→409, `InvalidTransition`→409, `Validation`→422, `Internal`→500.

## What Goes Where

- **Implementation Steps** (`[ ]`): all code, tests, docs achievable in this repo.
- **Post-Completion** (no checkboxes): LLM Gateway consumer integration, Redis backend, P2/P3 features, load/perf testing, deployment config.

## Implementation Steps

### Task 1: SDK preparation — drop `#[non_exhaustive]` from entity structs

**Files:**
- Modify: `gears/model-registry/model-registry-sdk/src/models/entity.rs`
- Modify: `gears/model-registry/model-registry-sdk/src/models/info.rs`
- Modify: `gears/model-registry/model-registry-sdk/src/models/common.rs`

- [x] remove `#[non_exhaustive]` from `ModelV1<P>` and `ProviderV1` (`entity.rs`)
- [x] remove `#[non_exhaustive]` from `ModelInfoV1<P>` (`info.rs`)
- [x] remove `#[non_exhaustive]` from `ModelCapabilities` and `DisabledCapabilities` (`common.rs`)
- [x] leave `#[non_exhaustive]` on the 6 enums in `common.rs` and on `ModelRegistryError` in `errors.rs`
- [x] run `cargo build -p cf-gears-model-registry-sdk` and `cargo test -p cf-gears-model-registry-sdk` — must pass
- [x] run `cargo build -p cf-gears-llm-gateway-sdk -p cf-gears-llm-gateway-demo` — consumers still compile

### Task 2: Scaffold implementation crate and wire into workspace + example server

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

### Task 3: Config module

**Files:**
- Create: `gears/model-registry/model-registry/src/config.rs`

- [x] define `ModelRegistryConfig` (serde `Deserialize` + `Default`) with cache TTLs (`own_ttl_seconds` default 1800, `inherited_ttl_seconds` default 300) and any P1 tunables (e.g. `max_page_size`)
- [x] provide `#[serde(default = ...)]` defaults so `ctx.config_or_default()` works with no YAML
- [x] write tests for defaults and partial-YAML deserialization
- [x] run tests — must pass before next task

### Task 4: Domain errors and repository traits

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/mod.rs`
- Create: `gears/model-registry/model-registry/src/domain/error.rs`
- Create: `gears/model-registry/model-registry/src/domain/repo.rs`

- [x] define `DomainError` (thiserror) covering not-found/conflict/validation/forbidden/deprecated/not-approved/internal, with `From<DomainError> for ModelRegistryError` and `From` for DB errors
- [x] define `ProviderRepository` and `ModelRepository` traits over `&C: DBRunner` + `&AccessScope` (mirror `SettingsRepository`): provider get/list(OData)/create/update/delete; model get_by_canonical/list(OData)/create/update/soft_delete; approval get/set/delete
- [x] create **empty stub files** for `service.rs`, `local_client.rs`, `cache.rs`, `inheritance.rs` and declare all `mod`s in `domain/mod.rs` now, so the crate compiles at this task's build gate (later tasks fill the stubs)
- [x] write tests for the `DomainError` → `ModelRegistryError` mappings (each variant)
- [x] run tests — must pass before next task

### Task 5: SeaORM entities and migrations (providers, models, model_approvals)

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/mod.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/mod.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/entity/mod.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/entity/provider.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/entity/model.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/entity/model_approval.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/migrations/mod.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/migrations/initial_001.rs`

- [x] define SeaORM `Entity`/`Model`/`ActiveModel` for `providers`, `models`, `model_approvals` per the schema section above — **no `info` field**; the `models` entity carries identity/lifecycle fields, the 17 promoted scalars, the 13 OData/denormalized columns, and the 6 JSONB columns (`provider_settings`, `capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`, `allow_extra_params`) annotated `#[sea_orm(column_type = "JsonBinary", nullable)]`
- [x] document in the `entity/model.rs` doc block that the scalar columns are the source of truth, the five JSONB sub-objects hold fields that don't promote cleanly, and `provider_settings` is the only polymorphic blob (keyed by `gts_type`)
- [x] write `initial_001` migration creating the three tables via backend-dispatched raw SQL (`uuid`/`bool_`/`tstz`/`jsonb_nullable`/`varch`/`int` type variables per backend), with unique `(tenant_id, slug)`, unique `(tenant_id, canonical_id)`, FK `models.provider_id`→providers (RESTRICT), FK `model_approvals.model_id`→models (CASCADE), and the 13 B-tree indexes; `NOT NULL DEFAULT`s on `display_name`, `ctx_max_input_tokens`, `allow_parameter_override` (SQLite cannot `ALTER ADD NOT NULL`); **no Postgres GIN indexes**
- [x] wire `Migrator` (`MigratorTrait`) listing `initial_001` in `migrations/mod.rs`
- [x] write a migration up/down roundtrip test against an in-memory SQLite DB (tables absent → up → present → down → absent)
- [x] write tests asserting the schema: no `info` column, and the promoted scalar + JSONB sub-object columns exist with correct types/nullability
- [x] run tests — must pass before next task

### Task 6: Entity ↔ domain/SDK mappers

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/storage/mapper.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/mapper_test.rs`

- [x] implement mapping `provider::Model` ↔ `ProviderV1` (incl. `gts_type` string↔`GtsTypeId`, `status` enum, optional `metadata` JSONB)
- [x] implement `model_entity_to_v1` + `build_model_info_v1`: build a `serde_json` value from the scalar columns, the 5 JSONB sub-objects and `provider_settings`, then `serde_json::from_value::<ModelInfoV1>` — the `json!{…}`-then-roundtrip pattern
- [x] implement `build_capabilities` merging the 4 scalar booleans with `capabilities_full` JSONB — **columns are authoritative** over JSONB content
- [x] implement `merge_disabled_capabilities` / `merge_default_parameters` (+ their `default_*_value` helpers) and `supported_api_denorm_to_json_array`
- [x] keep `build_minimal_info` as the defensive fallback for rows on DB defaults (`display_name = ''`, `ctx_max_input_tokens = 0`)
- [x] implement `model_create_active_model` / `model_update_active_model`: `Set(...)` every scalar column from `req.info.*`, the 5 JSONB sub-objects (`build_capabilities_full_for_create` strips the 4 promoted booleans), the 13 denormalized OData columns, and `provider_settings`; PATCH semantics set only provided fields, and any PATCH touching `info.*` re-projects all columns
- [x] enforce immutability of `canonical_id` / `provider_slug` / `info.provider_model_id` / `info.gts_type`
- [x] keep `apply_info_patches` storage-independent (operates on in-memory `ModelInfoV1`)
- [x] write round-trip tests (domain→entity→domain) for provider and model with all columns populated, incl. OpenAI + Anthropic provider_settings and unknown-provider raw JSON
- [x] write tests for `build_capabilities`: scalar bools win over JSONB content (each of the 4), JSONB-only fields preserved
- [x] write tests for `model_create_active_model`: every column set correctly from a fully-populated `ModelInfoV1`; capability sub-object split (4 booleans out, rest in JSONB); `additional_info` map round-trip
- [x] write tests for `model_update_active_model`: PATCH on a single field re-projects all columns; denormalized OData columns stay in sync
- [x] write tests for immutability rejection, missing `provider_settings` (null on the wire), and the DB-default graceful-degradation path
- [x] construct SDK fixtures with plain struct literals (no `serde_json::from_value` workaround — `#[non_exhaustive]` is gone as of Task 1)
- [x] run tests — must pass before next task

### Task 7: OData field mapping for models and providers

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/storage/odata_mapper.rs`

- [x] implement `FieldToColumn` for a model filter-field enum, mapping each of the 15 allowed fields to a real `models` column; use `map_value` for enum-string↔storage translation; reject non-allowlisted fields (incl. `provider_settings.*`, `default_parameters.*`, per-MIME array fields) with a validation error
- [x] implement `FieldToColumn` for a provider filter-field enum (`slug`, `name`, `status`, `gts_type`, `managed`, `discovery_enabled`)
- [x] apply `$top`/`$skip`/`$orderby`/`$select` via `toolkit_odata`/`toolkit-db` `paginate_odata` helpers (mirror chat-engine `odata_mapper.rs`) — mappers implement `ODataFieldMapping`
- [x] write tests for allowed field translation (each field → column), rejected fields, `map_value` enum translation, cursor value extraction, and pagination bounds (default/max page size from config)
- [x] run tests — must pass before next task

### Task 8: SeaORM repository — providers CRUD (secure)

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/storage/provider_repo.rs`

- [x] implement `ProviderRepository` for `ProviderRepositoryImpl` using `SecureConn`/`AccessScope` (no raw `all/one/exec` — respect clippy disallowed-methods): get by id, list with OData, create (unique-slug conflict → `ProviderConflict`), update (PATCH; slug immutable), delete
- [x] enforce tenant scoping on every query via `AccessScope`
- [x] write integration tests (SQLite) for provider create/get/list(OData)/update/delete + slug-conflict + tenant-isolation (cross-tenant not visible)
- [x] run tests — must pass before next task

> **Note (2026-07-27):** the original implementation in `sea_orm_repo.rs` was split per trait — see Task 22 for the follow-up refactor. `ProviderRepositoryImpl` now lives in its own file.

### Task 9: SeaORM repository — models CRUD, OData list, approvals

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/storage/model_repo.rs`

- [x] implement `ModelRepository` for `ModelRepositoryImpl`: get_by_canonical, list with OData (filtering entirely on `models` columns — no join), create (derive canonical_id; unique conflict), update (PATCH; immutable identity fields), soft-delete (set `lifecycle_status=deprecated`, `deprecated_at`)
- [x] implement approval read/write (`get_approval`, `set_approval`, `delete_approval`) against `model_approvals` **and keep the denormalized `models.approval_status` column in sync** on every approval write (single transaction)
- [x] resolve `approval_status` on model reads from the `models` column (default `Pending`); exclude deprecated from default list
- [x] write integration tests (SQLite): model CRUD, OData `$filter` on `lifecycle_status`/promoted columns/`approval_status`, `$top`/`$skip`, soft-delete hiding, approval read/write/default, tenant isolation
- [x] build test fixtures (`make_create_model_req`) with struct literals rather than JSON round-trips
- [x] run tests — must pass before next task

> **Note (2026-07-27):** the original implementation in `sea_orm_repo.rs` was split per trait — see Task 22 for the follow-up refactor. `ModelRepositoryImpl` now lives in its own file.

### Task 10: CacheService trait and InMemoryCache backend

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/cache.rs`

- [x] define `CacheService` trait: `get`, `set` (with TTL), `delete`, `invalidate_tenant`; key format `mr:{tenant_id}:{entity}:{id}` (DESIGN §3.6)
- [x] implement `InMemoryCache` (TTL-aware, tenant-prefixed, `Send + Sync`); gate a future `RedisCache` behind a `redis` cargo feature (declared, not implemented — leave a `#[cfg(feature="redis")]` stub or TODO)
- [x] write tests: set/get hit, TTL expiry (own vs inherited), delete, `invalidate_tenant` clears only that tenant's keys, cross-tenant isolation
- [x] run tests — must pass before next task

### Task 11: Tenant inheritance resolver helper

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/inheritance.rs`
- Modify: `gears/model-registry/model-registry/src/domain/mod.rs`

- [x] add a helper that, given `SecurityContext` + `TenantResolverClient`, returns the ancestor tenant chain and computes the additive visible set with child-shadowing by slug (providers) / canonical_id (models) per DESIGN §2.1 "Additive Inheritance"
- [x] define ownership classification (own vs inherited) to drive cache TTL selection
- [x] write tests with a mocked `TenantResolverClient`: additive union, child shadows parent by slug, child cannot expand beyond parent, single-tenant (no ancestors) case
- [x] run tests — must pass before next task

### Task 12: ModelRegistryService — providers operations

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/service.rs`

- [x] define `Service<R, M, C>` (generic over ProviderRepository, ModelRepository, CacheService) holding db provider, provider_repo, model_repo, cache, tenant-resolver client, `PolicyEnforcer`, `ModelRegistryConfig`
- [x] add a helper deriving `AccessScope` from `SecurityContext` (mirror simple-user-settings service) used by every repo call
- [x] implement provider ops (get/list/create/update/delete) with authz (`PolicyEnforcer`), tenant scoping, inheritance resolution for reads, cache read-through/invalidation on writes, and validation (slug format)
- [x] write unit tests — slug validation (format, length); DB-bound coverage deferred to the integration suite
- [x] run tests — must pass before next task

### Task 13: ModelRegistryService — models read (cache-first, inheritance, approval resolve)

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`

- [x] implement `get_tenant_model` (cache-first with DB fallback, `approval_status` **populated on the returned model — NOT fail-closed on pending/rejected/revoked**, TTL by ownership) and `list_tenant_models` (OData + inheritance union + pagination). Return `ModelDeprecated` only when a soft-deleted model is fetched directly; `ModelNotFound` when absent
- [x] populate cache on miss; select TTL from ownership (own vs inherited) via the Task 11 helper
- [x] write unit tests: cache hit path, miss→DB→populate, not-found→`ModelNotFound`, deprecated-on-direct-get→`ModelDeprecated`, **pending/rejected model returned with populated `approval_status` (no error)**, inherited model visible with shorter TTL, list filtering + pagination
- [x] run tests — must pass before next task

### Task 14: ModelRegistryService — models CRUD and approval writes

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`

- [x] implement `create_model` (provider-exists check incl. inherited providers, canonical_id derivation, default approval `Pending`, optional initial `approval_status`), `update_model` (PATCH non-status fields directly + `approval_status` transition written directly to `model_approvals` in P1), `delete_model` (soft-delete)
- [x] invalidate affected cache entries on every write; validate state transitions (`InvalidTransition` where illegal)
- [x] write unit tests: create with/without initial approval, update fields, approve/reject/revoke via `approval_status`, invalid transition rejected, soft-delete, cache invalidation, provider-not-found on create
- [x] build test fixtures with struct literals
- [x] run tests — must pass before next task

### Task 15: LocalClient implementing ModelRegistryClientV1

**Files:**
- Create: `gears/model-registry/model-registry/src/domain/local_client.rs`

- [x] implement `LocalClient` wrapping `Arc<Service>` and implementing all 10 `ModelRegistryClientV1` methods, mapping `DomainError` → `ModelRegistryError`
- [x] write unit tests (mocked service) verifying each trait method delegates and maps errors correctly
- [x] run tests — must pass before next task

### Task 16: REST DTOs (`From` impls) and error mapping

**Files:**
- Create: `gears/model-registry/model-registry/src/api/mod.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/mod.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/dto.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/dto_test.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/error.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/error_test.rs`

- [x] define REST DTOs (serde + `utoipa::ToSchema`, DTOs only in `api/rest/`): provider create/update/response, model create/update/response, list responses (`Page<...>`)
- [x] add `impl From<ProviderV1> for ProviderDto` — `status: ProviderStatus` → `"active"|"disabled"` via `match` + `_ =>` wildcard (enums stay `#[non_exhaustive]`), `created_at`/`updated_at` → `String` via `.to_rfc3339()`, `metadata` passthrough
- [x] add `impl<P> From<ModelV1<P>> for ModelDto` — `lifecycle_status`/`approval_status` → strings via `match` + wildcard, `info: ModelInfoV1<P>` → `JsonValue` via `serde_json::to_value(&source.info).unwrap_or(JsonValue::Null)`
- [x] write the `dto.rs` doc comment describing the `From`-based pattern (no serde round-trip)
- [x] implement `ModelRegistryError` → RFC-9457 `Problem` mapping for **all 11 variants** per Technical Details in `error.rs`
- [x] write unit tests for both `From` impls: happy path + every status-string variant + info serialization
- [x] write tests: DTO (de)serialization; error→Problem status/type mapping asserting **every one of the 11 variants**
- [x] run tests — must pass before next task

### Task 17: REST handlers and routes (providers + models)

**Files:**
- Create: `gears/model-registry/model-registry/src/api/rest/handlers.rs`
- Create: `gears/model-registry/model-registry/src/api/rest/routes.rs`

- [x] implement handlers for all 10 P1 endpoints (extract `SecurityContext`, parse `ODataQuery` for list endpoints, call service, map errors to `Problem`)
- [x] convert every SDK entity to its DTO with `Type::from(...)` / `.into()` / `.map(Dto::from).collect()` — **no `serde_json::from_value(serde_json::to_value(...))` round-trips** in `get_provider`, `list_providers`, `create_provider`, `update_provider`, `get_model`, `list_models`, `create_model`, `update_model`
- [x] register routes with `OperationBuilder` (`.authenticated()`, `.json_request`/`.json_response_with_schema`, `.error_4xx/5xx`, license feature) under `/model-registry/v1/...`, attach service via `Extension`, mirror simple-user-settings `routes.rs`
- [x] write handler tests (success + error) using an in-memory service/repo, incl. OData query parsing and 404/403/409/422 paths
- [x] verify no round-trip patterns remain: `grep -n "from_value.*to_value\|to_value.*from_value" handlers.rs`
- [x] run tests — must pass before next task

### Task 18: Gear wiring — init, DatabaseCapability, RestApiCapability, ClientHub registration

**Files:**
- Modify: `gears/model-registry/model-registry/src/gear.rs`

- [x] implement `Gear::init` (load config, get `DBProvider`, build repo + cache + service, fetch `tenant-resolver` and `authz-resolver` clients from `ClientHub`, register `LocalClient` as `dyn ModelRegistryClientV1`)
- [x] implement `DatabaseCapability::migrations` returning `Migrator::migrations()` and `RestApiCapability::register_rest` calling `routes::register_routes`
- [x] write gear tests (default construction, migrations non-empty) mirroring simple-user-settings
- [x] run `cargo build`/`clippy` for the crate — must pass before next task

### Task 19: End-to-end gear integration tests

**Files:**
- Create: `gears/model-registry/model-registry/tests/integration.rs`

- [x] write an integration test booting the gear (service+repo+cache) against SQLite: provider create → model create → get_tenant_model (cache-first) → list with OData filter → update approval → soft-delete; assert tenant isolation and inheritance across a parent/child tenant pair
- [x] add integration test: create model with full `ModelInfoV1` → the row has all promoted scalar + JSONB sub-object columns populated and no `info` column
- [x] add integration test: read back → reconstructed `ModelInfoV1` matches input (all fields, including nested)
- [x] add integration test: PATCH a single field → columns update correctly, response reflects the change
- [x] add integration test: capability merge — the 4 OData booleans come from columns, the rest from `capabilities_full` JSONB
- [x] build fixtures (`make_create_model_req`) with struct literals
- [x] run `cargo test -p cf-gears-model-registry --features sqlite -- --nocapture` — must pass before next task

### Task 20: Verify acceptance criteria

- [x] verify all P1 requirements implemented (10 endpoints in `routes.rs`, cache-first read in `get_tenant_model`, inheritance+shadowing in `inheritance.rs`, approval resolve/write in `service.rs`, tenant isolation via `AccessScope` + `SecureConn`, OData filtering in `odata_mapper.rs`)
- [x] verify edge cases: unknown-provider raw JSON round-trip, immutable-field rejection, deprecated hidden from default list, `get_tenant_model` returns pending/rejected models with populated status, denormalized filterable columns stay in sync after PATCH, non-allowlisted OData field rejected, DB-default graceful degradation
- [x] verify storage invariants: no `info` column and no `info: Set`/`e.info` references in `mapper.rs`; all 17 scalar + 5 JSONB sub-object columns projected in both `model_create_active_model` and `model_update_active_model`; OData surface is 15 fields over 13 indexes
- [x] verify wire compatibility: `ModelDto { info: JsonValue }` unchanged; SDK `ModelInfoV1<P>` unchanged
- [x] verify the 5 SDK structs no longer carry `#[non_exhaustive]` while the 6 enums and `ModelRegistryError` still do
- [x] run `cargo build -p cf-gears-llm-gateway-sdk -p cf-gears-llm-gateway-demo` and their test suites — must pass
- [x] run `cargo test -p cf-gears-model-registry` (lib + integration) — pass
- [x] run `make test-sqlite` — SQLite end-to-end pass
- [x] run `cargo test --workspace` — 8547 tests pass. ⚠️ `cf-gears-nodes-registry::test_get_node_sysinfo_succeeds_for_existing_node` fails pre-existing (the `sysinfo` crate cannot detect CPU model in this LinuxKit container); confirmed against an unmodified tree, unrelated to this work
- [x] run `cargo fmt --all -- --check` — clean
- [x] run `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::perf` — clean
- [x] run `make dylint` — clean. ⚠️ Only warning is pre-existing DE1201 on `cf-gears-cluster`, unrelated
- [x] run `make gts-docs` — clean (ADR/DESIGN references validate)

### Task 21: Update documentation and finalize

- [x] update `README.md` with the implemented P1 REST surface and build/run notes
- [x] check off the implemented `p1` functional drivers in `gears/model-registry/docs/DESIGN.md` §1.2; note the P1 implementation deviations from the DESIGN
- [x] update `gears/model-registry/docs/DESIGN.md` §3.6 storage layout: no `info` row; document the 17 scalar columns, the 5 JSONB sub-objects, and `provider_settings`; state that scalar columns are the source of truth and `provider_settings` is the only polymorphic JSONB column, identified by `gts_type`
- [x] update `gears/model-registry/docs/ADR/0005-cpt-cf-model-registry-adr-gts-typed-provider-settings.md` Consequences: five JSONB columns now exist (`provider_settings`, `capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`, plus `allow_extra_params`), tagged by the scalar `gts_type`; add a "Consequences (added 2026-07-24)" bullet summarizing the schema decomposition
- [x] confirm no doc change is needed for the DTO idiom — `docs/toolkit_unified_system/04_rest_operation_builder.md:182-197` already documents `UserDto::from(user)`, and this gear now matches it
- [x] update `CLAUDE.md` with the reusable patterns that emerged: "OData Filtering Requires Real Columns" (now strictly followed — no `info` JSONB at all) and "Integration Tests with SQLite + Mocked Clients"
- [x] move this plan to `docs/plans/completed/`

### Task 22: Split `SeaOrmRepository` into per-trait repository implementations (Parnas refactor)

> **Added 2026-07-27.** Follow-up to Tasks 8 + 9. The previous implementation
> co-located both repository traits on a single zero-state unit struct
> (`SeaOrmRepository`) in one 2139-line file. This task gives each trait its
> own dedicated, single-responsibility implementation type and file.

**Files:**
- Create: `gears/model-registry/model-registry/src/infra/storage/provider_repo.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/model_repo.rs`
- Create: `gears/model-registry/model-registry/src/infra/storage/error_mapping.rs`
- Delete: `gears/model-registry/model-registry/src/infra/storage/sea_orm_repo.rs`
- Modify: `gears/model-registry/model-registry/src/infra/storage/mod.rs`
- Modify: `gears/model-registry/model-registry/src/gear.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/handlers.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/routes.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs` (test module only)
- Modify: `gears/model-registry/model-registry/tests/integration.rs`

- [x] create `ProviderRepositoryImpl` (zero-state unit struct) in `provider_repo.rs` implementing `ProviderRepository`. Copy the impl block from the old `sea_orm_repo.rs` unchanged.
- [x] create `ModelRepositoryImpl` (zero-state unit struct) in `model_repo.rs` implementing `ModelRepository`. Copy the impl block unchanged. Move the file-private helpers `filter_references_lifecycle_status`, `approval_status_to_string`, `approval_status_from_string` into this file.
- [x] create `error_mapping.rs` with `pub(super) fn is_fk_violation(&DbErr) -> bool` and `pub(super) fn map_scope_error(ScopeError) -> DomainError`. Both helpers are used by every method in both impls, so they need a single home — `pub(super)` keeps them scoped to `infra::storage`. Do **not** expose them outside the storage layer.
- [x] split the inline `mod tests` block (originally `sea_orm_repo.rs:628-2139`, 1500+ lines covering both traits) between `provider_repo::tests` (owns `setup_provider`, `test_tenant`, `other_tenant`, `scope_for`, `make_create_req`, `make_full_create_req`) and `model_repo::tests` (owns a duplicated copy of the four DB-setup helpers, plus `create_test_provider`, `make_create_model_req`, `create_test_model`). Add a one-line comment at the top of `model_repo::tests` flagging the intentional duplication.
- [x] update `infra/storage/mod.rs`: replace `pub mod sea_orm_repo;` with `pub mod error_mapping;`, `pub mod model_repo;`, `pub mod provider_repo;`.
- [x] delete `sea_orm_repo.rs`.
- [x] update `gear.rs`: replace the single `use … SeaOrmRepository;` with two imports; change `type ConcreteService = Service<SeaOrmRepository, SeaOrmRepository, InMemoryCache>;` to `Service<ProviderRepositoryImpl, ModelRepositoryImpl, InMemoryCache>`; update the two `Arc::new(SeaOrmRepository::new())` calls in `init`.
- [x] update `api/rest/handlers.rs` and `api/rest/routes.rs` with the same import + type-alias change (preserve `type` vs `pub type` visibility).
- [x] update `tests/integration.rs`: replace the import; change `build_service` return type to `Service<ProviderRepositoryImpl, ModelRepositoryImpl, InMemoryCache>`; update `create_provider_direct` (`repo: &ProviderRepositoryImpl`) and `create_model_direct` (`repo: &ModelRepositoryImpl`); in the three tests that bind a single `repo` local and call both helpers, split into two locals (`provider_repo` / `model_repo`) — `child_inherits_provider_and_model_from_parent`, `child_shadows_parent_by_same_canonical_id`, `cache_first_get_returns_cached_model`.
- [x] update `src/domain/service.rs::tests`: replace the single import; update `create_test_provider` (`repo: &ProviderRepositoryImpl`) and `create_test_model` (`repo: &ModelRepositoryImpl`); update `build_service_with_cache` and `build_service` return types; replace the two `Arc::new(SeaOrmRepository)` constructions; bulk-replace `let repo = SeaOrmRepository;` with the dual-local form `let provider_repo = ProviderRepositoryImpl; let model_repo = ModelRepositoryImpl;`, then go through and update each `create_test_provider(&repo, …)` / `create_test_model(&repo, …)` callsite to use the appropriate local. Delete the `model_repo` binding in the 4 tests that only call `create_test_provider` (`test_create_model_success`, `test_create_model_with_initial_approval`, `test_create_model_with_inherited_provider`, `test_create_model_cache_invalidation`).
- [x] decide and document the cross-entity delete guard — **keep it inside `ProviderRepositoryImpl::delete`** (it reads `model::Entity` to enforce the FK pre-check + TOCTOU fallback). Both cross-entity reads stay inside `infra::storage`, so no layer violation. No changes to the `ModelRepository` trait or `Service::delete_provider`.
- [x] run `cargo build -p cf-gears-model-registry` — clean
- [x] run `cargo build --tests -p cf-gears-model-registry` — clean
- [x] run `cargo test -p cf-gears-model-registry --lib` — 244 passed
- [x] run `cargo test -p cf-gears-model-registry --test integration` — 13 passed
- [x] run `cargo clippy -p cf-gears-model-registry --all-targets --all-features -- -D warnings` — clean
- [x] run `cargo dylint --all -p cf-gears-model-registry` — clean
- [x] `grep -rn "SeaOrmRepository" gears/model-registry/model-registry/` — zero hits

**Design decisions (user-confirmed):**

- **Struct names:** `ProviderRepositoryImpl` and `ModelRepositoryImpl`. The `Impl` suffix signals "concrete storage implementation of the trait" without baking the ORM name into the type. File names are `provider_repo.rs` and `model_repo.rs`.
- **Scope:** Only `sea_orm_repo.rs` is split. `mapper.rs` and `odata_mapper.rs` remain mixed in this PR (each carries both provider and model mappers). Splitting them is a separate follow-up if desired.
- **Cross-entity delete guard:** Stays inside `ProviderRepositoryImpl::delete`. The provider impl keeps importing `entity::model` for the FK pre-check + `is_fk_violation` TOCTOU fallback. No service signature changes.

**Why this refactor (Parnas information hiding):** The two traits already exist separately in `domain/repo.rs`, and the `Service<R, M, C>` struct was already generic over them — the wiring in `gear.rs` even created two distinct `Arc<SeaOrmRepository>` instances. But the implementation type was shared, so changing how providers are persisted forced touching the same file as model persistence, and the cross-concern file had grown to 2139 lines. After the split: each trait owns one struct, one file, and one test module; cross-cutting helpers (`is_fk_violation`, `map_scope_error`) have a single home in `error_mapping.rs`; `mod.rs` re-exports both modules explicitly.

## Implementation Notes

1. **NOT NULL DEFAULTs on SQLite** — `display_name`, `ctx_max_input_tokens`, `allow_parameter_override` need DEFAULTs at CREATE time (SQLite cannot `ALTER ADD NOT NULL`): `DEFAULT ''`, `DEFAULT 0`, `DEFAULT 0`. The application layer always overrides them.
2. **Integer width** — `INTEGER` is 32-bit on Postgres/MySQL and overflows `size_bytes` ≥ 2 GiB, so integer columns are `BIGINT` there and `INTEGER` on SQLite (8-byte).
3. **Capability merge** — the read path takes the 4 promoted booleans from columns and the rest from `capabilities_full`; **columns win**.
4. **Update path is verbose** — every PATCH touching `info.*` re-projects all columns. Correct but noisier SQL; optimization is out of scope.
5. **Enums stay `#[non_exhaustive]`** — all `match` expressions over SDK enums in `From` impls and mappers keep a `_ =>` wildcard arm.
6. **Foreign-gear consumers** — only `llm-gateway-demo/src/mock_registry.rs` and `llm-gateway-sdk/src/models/plugin.rs` reference `ModelInfoV1` outside model-registry, and both treat it as an opaque aggregate.
7. **No separate migration for the storage layout** — `initial_001.rs` carries the final schema directly; the gear was not merged/deployed when the `info` column was dropped.
8. **DB-setup helper duplication across repo test modules** (added 2026-07-27, Task 22) — `setup_provider`, `test_tenant`, `other_tenant`, `scope_for` are duplicated between `provider_repo::tests` and `model_repo::tests` (~17 lines) so each test module is self-contained. If a third test module ever needs the same setup, extract them into a `pub(super) mod common;` then.
9. **Cross-entity reads inside the storage layer** (added 2026-07-27, Task 22) — `ProviderRepositoryImpl::delete` queries `entity::model` for the FK pre-check; `ModelRepositoryImpl::create` queries `entity::provider` to validate the slug. Both stay inside `infra::storage`, so no domain/layer violation, but be aware when reading either file that the imports list entities from both tables.

## Post-Completion
*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification**:
- Run the example server (`make example` / quickstart) with the gear enabled and exercise the REST endpoints manually: `POST /model-registry/v1/providers` → `POST /model-registry/v1/models` with a full info payload → inspect the row (`SELECT * FROM models WHERE id = …;`, confirm no `info` column and the promoted columns populated) → `GET /model-registry/v1/models/{canonical_id}` (response carries the full reconstructed `info` JSON) → `PATCH` a single field → approve → delete.
- Spot-check `make openapi` — `ProviderDto` and `ModelDto` response shapes must be unchanged by the `From`-impl refactor.
- Performance: DESIGN NFR `get_tenant_model` < 10ms P99 — benchmark under load once the Redis backend lands.

**External system updates / follow-ups (out of P1 scope)**:
- **LLM Gateway integration**: consume `ModelRegistryClientV1` via ClientHub to resolve model routing.
- **Redis cache backend**: implement the feature-gated `RedisCache` and its integration tests.
- **P2**: model discovery via OAGW, Approval Service delegation (swap the P1 direct approval-write path), bulk approve, discovery trigger endpoint.
- **P3**: provider health monitoring, aliases, tags/model_tags + `tag` OData filter, degraded-mode, tenant reparenting cache invalidation.
- **P4**: user-group and user-level approval overrides.
- **Storage layout follow-ups** (added 2026-07-27): `mapper.rs` and `odata_mapper.rs` are still mixed (each carries both provider and model mappers). The Task 22 split could be extended to disaggregate them into `provider_mapper.rs` / `model_mapper.rs` and `provider_odata_mapper.rs` / `model_odata_mapper.rs` if desired.
