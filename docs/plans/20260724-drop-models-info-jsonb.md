# Drop `info` JSONB, promote `ModelInfoV1` fields to typed columns

## Overview

Today the `models` table has a JSONB `info` column that is the "authoritative source of truth" for `ModelInfoV1`, plus ~13 scalar columns that denormalize a subset of those fields for OData filtering (`gears/model-registry/docs/DESIGN.md:1050`).

Remove the duplication: drop the `info` JSONB column entirely and store every `ModelInfoV1` field in either a typed scalar column or one of four small JSONB sub-object columns (`capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`). The polymorphic `provider_settings` JSONB column (per-provider routing/pricing, discriminator is `gts_type`) stays as-is.

The public SDK `ModelInfoV1<P>` and the wire DTO `ModelDto { info: JsonValue }` are **unchanged** — only the storage layout moves. `llm-gateway-sdk` plugins that take `ModelInfoV1` continue to work.

### User decisions (captured)
- Scope: drop `info` only, keep `provider_settings`.
- Promote every `ModelInfoV1` field to a typed column (or one of four small JSONB sub-objects).
- OData filter surface stays at the current 15 fields.
- `disabled_capabilities` → JSONB column.
- `allow_extra_params` → JSONB column.
- `performance.{response_latency_ms, tokens_per_second}` → two scalar columns.
- **No separate migration** — modify `initial_001.rs` directly since the model-registry gear is not yet merged/deployed.

## Context (from discovery)

- **Files involved**:
  - `gears/model-registry/model-registry/src/infra/storage/migrations/initial_001.rs` — rewrite CREATE TABLE
  - `gears/model-registry/model-registry/src/infra/storage/entity/model.rs` — drop `info`, add 21 fields
  - `gears/model-registry/model-registry/src/infra/storage/mapper.rs` — rewrite read + write paths
  - `gears/model-registry/model-registry/src/infra/storage/mapper_test.rs` — update `make_model_entity` fixture; targeted test fixes
  - `gears/model-registry/model-registry/src/infra/storage/odata_mapper.rs` — fix the `model_extract_cursor_value_round_trip` entity literal (test only)
  - `gears/model-registry/docs/DESIGN.md` — lines 1024-1058 rewrite
  - `gears/model-registry/docs/ADR/0005-cpt-cf-model-registry-adr-gts-typed-provider-settings.md` — clarifying edits
- **Related patterns**: OData filter columns are real columns (CLAUDE.md "OData Filtering Requires Real Columns"); `#[non_exhaustive]` SDK types are round-tripped via `serde_json::json!{...}` then `serde_json::from_value::<T>`.
- **Dependencies**: none new.
- **No changes needed** in `dto.rs`, `handlers.rs`, `cache.rs`, `service.rs`, `sea_orm_repo.rs`, `dto_test.rs`, `integration.rs`, `llm-gateway-demo/src/mock_registry.rs`.

## Column design

### Scalar columns (17 new + existing)

Add to the `models` CREATE TABLE in `initial_001.rs`:

| Column | Type (PG / MySQL / SQLite) | Source field | Nullable |
|---|---|---|---|
| `display_name` | TEXT / TEXT / TEXT | `info.display_name` | NOT NULL |
| `description` | TEXT / TEXT / TEXT | `info.description` | NULL |
| `size_bytes` | BIGINT / BIGINT / INTEGER | `info.size_bytes` | NULL |
| `region` | VARCHAR(64) / VARCHAR(64) / TEXT | `info.region` | NULL |
| `hosted_by` | VARCHAR(64) / VARCHAR(64) / TEXT | `info.hosted_by` | NULL |
| `last_release_at` | TIMESTAMPTZ / DATETIME(6) / TEXT | `info.last_release_at` | NULL |
| `reasoning_level` | VARCHAR(32) / VARCHAR(32) / TEXT | `info.reasoning_level` | NULL |
| `version` | VARCHAR(64) / VARCHAR(64) / TEXT | `info.version` | NULL |
| `sort_order` | INTEGER / INTEGER / INTEGER | `info.sort_order` | NULL |
| `icon` | TEXT / TEXT / TEXT | `info.icon` | NULL |
| `multiplier_display` | VARCHAR(32) / VARCHAR(32) / TEXT | `info.multiplier_display` | NULL |
| `perf_response_latency_ms` | INTEGER / INTEGER / INTEGER | `info.performance.response_latency_ms` | NULL |
| `perf_tokens_per_second` | INTEGER / INTEGER / INTEGER | `info.performance.tokens_per_second` | NULL |
| `ctx_max_input_tokens` | INTEGER / INTEGER / INTEGER | `info.context_window.max_input_tokens` | NOT NULL |
| `ctx_max_output_tokens` | INTEGER / INTEGER / INTEGER | `info.context_window.max_output_tokens` | NULL |
| `ctx_output_vector_size` | INTEGER / INTEGER / INTEGER | `info.context_window.output_vector_size` | NULL |
| `allow_parameter_override` | BOOLEAN / BOOLEAN / BOOLEAN | `info.allow_parameter_override` | NOT NULL DEFAULT 0 |

### JSONB columns (4 new + 1 existing)

Add four JSONB sub-object columns. Same backend-dispatched type string as the existing `provider_settings` (`jsonb_nullable`):

| Column | Holds | Reason |
|---|---|---|
| `capabilities_full` | `ModelCapabilities` minus the 4 OData booleans (vision mime types, reasoning toggle/resume/budget, response_schema, file_input, image_generation, audio_input, audio_output, code_interpreter, web_search) | Too granular to promote individually |
| `default_parameters` | `DefaultInferenceParametersV1` (~13 mostly-Optional fields) | Sub-object, not worth promoting |
| `additional_info` | `HashMap<String, serde_json::Value>` | Forward-compat escape hatch |
| `disabled_capabilities_full` | `DisabledCapabilities` (mirrors `capabilities`) | Symmetric with `capabilities_full` |

Keep `provider_settings` JSONB unchanged.

### Drop

`info` (JSONB) — line 53 of `initial_001.rs`.

### OData filter surface — unchanged

15 filterable fields, mapped to the same 15 existing columns (`canonical_id`, `lifecycle_status`, `approval_status`, `gts_type`, `supported_api`, `provider_model_id`, `vendor`, `family`, `managed`, `architecture`, `format`, `vision`, `function_calling`, `streaming`, `reasoning_effort`). No new OData fields. Existing 11 indexes (lines 85-96 of `initial_001.rs`) stay.

## Development Approach

- **testing approach**: Regular (code first, then tests within the same task).
- complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task with code changes MUST include new/updated tests** (success + error/edge scenarios) as separate checklist items.
- **CRITICAL: all tests must pass before starting the next task**.
- Build/lint gate per task: `cargo build -p cf-gears-model-registry` and `cargo clippy -p cf-gears-model-registry --all-targets --all-features -- -D warnings -D clippy::perf` must be clean.
- **CRITICAL: update this plan file when scope changes during implementation**.
- maintain backward compatibility with the SDK (do not break `ModelRegistryClientV1`).

## Testing Strategy

- **unit tests**: required for every task — mapper round-trip (read new columns → `ModelInfoV1`; write `ModelInfoV1` → new columns), `make_model_entity` fixture rewrite, OData cursor entity literal, JSONB-null fallback, capability-merge logic (4 scalar booleans override JSONB content).
- **integration tests**: full SeaORM round-trip against SQLite (create → read → patch → delete) to confirm the migration up/down works and read path reconstructs `ModelInfoV1` correctly.
- **e2e**: not applicable for this gear (REST handlers + wire DTOs unchanged; covered by existing `integration.rs`).
- treat integration tests with the same rigor as unit tests (must pass before next task).

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- keep this plan in sync with actual work done

## Solution Overview

Mirror the existing gear layering (DESIGN §1.3). The storage rewrite moves the JSONB "source of truth" into 17 scalar columns + 4 small JSONB sub-objects + the unchanged polymorphic `provider_settings`. The mapper rebuilds the in-memory `ModelInfoV1` JSON on the read path by stitching columns + JSONB sub-objects via `serde_json::json!{...}` then `serde_json::from_value::<ModelV1>(value)` (same pattern as the existing `build_minimal_info` fallback). The four OData-filterable capability booleans come from scalar columns on read and re-derive the `ModelCapabilities` shape; the rest of capability content rides in `capabilities_full` JSONB. The write path re-projects every new column on create/update.

## Technical Details

- **Storage layout** (see tables above): 17 scalar columns + 4 JSONB sub-objects + existing `provider_settings` JSONB. Total: 21 new/derived columns, 1 dropped column.
- **Read path**: `model_entity_to_v1` builds a JSON value from the new columns + `provider_settings` JSONB + the four new JSONB sub-objects, then `serde_json::from_value::<ModelV1>(value)`. New helper `build_capabilities(e: &entity::model::Model) -> ModelCapabilities` merges the 4 scalar bools with `e.capabilities_full` JSONB (columns authoritative).
- **Write path**: `model_create_active_model` and `model_update_active_model` drop `info: Set(...)`. They add `Set(...)` for every new column from `req.info.*`, plus JSONB `Set(...)` for `capabilities_full` (built from `req.info.capabilities` minus the 4 promoted booleans), `default_parameters`, `additional_info`, `disabled_capabilities_full`. `provider_settings` extraction unchanged. Every PATCH that touches an `info.*` field re-projects all new columns.
- **`apply_info_patches`** (mapper.rs lines 320-376): **unchanged** — operates on in-memory `ModelInfoV1`, independent of storage.
- **`build_minimal_info`** (mapper.rs lines 468-589): kept as defensive fallback. Since `display_name` and `ctx_max_input_tokens` will always be populated post-migration (with DEFAULTs), this rarely triggers. Keep the graceful-degradation pattern but it can be simplified.
- **DTO/handlers/cache/service/repo**: **unchanged**. Wire shape preserved. Handler roundtrips via `serde_json::from_value::<ModelInfoV1>(dto.info)` (handlers.rs:244-249) keep working. `cache.rs` stores the full `ModelV1` post-deserialization, so wire/SDK shape is unchanged.

## What Goes Where

- **Implementation Steps** (`[ ]`): all code, tests, docs achievable in this repo.
- **Post-Completion** (no checkboxes): manual smoke testing if needed.

## Implementation Steps

### Task 1: Rewrite `migrations/initial_001.rs`

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/migrations/initial_001.rs`

- [x] replace the `models` CREATE TABLE (lines 46-73) — drop the `info` column (line 53)
- [x] keep all existing 22 columns (identity, lifecycle, the 13 already-promoted columns, `provider_settings`, timestamps)
- [x] add the 17 new scalar columns with `NOT NULL DEFAULT` where required (`display_name DEFAULT ''`, `ctx_max_input_tokens DEFAULT 0`, `allow_parameter_override DEFAULT 0`)
- [x] add the 4 new JSONB columns (`capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`) using the existing `jsonb_nullable` type variable
- [x] verify the existing 11 indexes (lines 85-96) remain unchanged
- [x] update `initial_migration_up_down_roundtrip` test (lines 124-153) — verify migration up/down works with the new column list; the `INSERT INTO providers` doesn't touch `models`, so it should still pass
- [x] write tests asserting the new schema: `info` column is absent, the 21 new columns exist with correct types/nullability
- [x] write tests asserting migration up/down roundtrip succeeds (migrations module test fixture)
- [x] run `cargo test -p cf-gears-model-registry --lib` — must pass before task 2

### Task 2: Update `entity/model.rs`

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/entity/model.rs`

- [x] remove `pub info: Option<serde_json::Value>` (lines 35-36)
- [x] add the 17 scalar fields (typed per the column design table above; match SeaORM `ColumnType` annotations)
- [x] add the 4 JSONB fields (`capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`) using `#[sea_orm(column_type = "JsonBinary", nullable)]` mirroring the existing `provider_settings` (line 39)
- [x] rewrite the doc comment block (lines 8-17) — `info` is gone; scalar columns are now the source of truth, with the four JSONB sub-object columns for fields that don't promote cleanly, plus the polymorphic `provider_settings` keyed by `gts_type`
- [x] write tests asserting entity deserializes a SQLite row with the new column layout (round-trip entity construction)
- [x] run `cargo build -p cf-gears-model-registry` and `cargo test -p cf-gears-model-registry --lib` — must pass before task 3

### Task 3: Rewrite read path in `mapper.rs`

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/mapper.rs`

- [x] rewrite `model_entity_to_v1` (currently lines 153-173): instead of `serde_json::from_value(e.info)`, build a JSON value from the new columns + `provider_settings` JSONB + the four new JSONB sub-objects, then `serde_json::from_value::<ModelV1>(value)` (same JSON-value-then-roundtrip pattern as `build_minimal_info`, line 468)
- [x] ensure the four scalar booleans override whatever is in `capabilities_full.vision.enabled` etc. — columns are authoritative
- [x] add new helper `build_capabilities(e: &entity::model::Model) -> ModelCapabilities` that merges the 4 scalar bools with `e.capabilities_full` JSONB
- [x] write tests for `model_entity_to_v1`: round-trip with all 21 columns populated
- [x] write tests for `build_capabilities`: scalar bools win over JSONB content (3 cases — 4 bools each), JSONB-only fields preserved
- [x] write tests for graceful-degradation when DB defaults are in place (`display_name = ''`, `ctx_max_input_tokens = 0`) — `ModelInfoV1` reconstructs without panic
- [x] run `cargo test -p cf-gears-model-registry --lib` — must pass before task 4

### Task 4: Rewrite write paths in `mapper.rs`

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/mapper.rs`

- [x] rewrite `model_create_active_model` (lines 181-230): drop `info: Set(...)`; add `Set(...)` for every new column from `req.info.*`
- [x] in `model_create_active_model`, add JSONB `Set(...)` for `capabilities_full` (built from `req.info.capabilities` minus the 4 promoted booleans), `default_parameters`, `additional_info`, `disabled_capabilities_full`
- [x] in `model_create_active_model`, extract `provider_settings` (unchanged behavior)
- [x] rewrite `model_update_active_model` (lines 246-314): identical structure — every PATCH that touches an info field re-projects all new columns
- [x] verify `apply_info_patches` (lines 320-376) remains **unchanged** — it operates on in-memory `ModelInfoV1`, independent of storage
- [x] write tests for `model_create_active_model`: every new column is set correctly from a fully-populated `ModelInfoV1` (assert column values match input)
- [x] write tests for `model_create_active_model`: capability sub-object built correctly (4 booleans extracted, rest preserved in JSONB)
- [x] write tests for `model_create_active_model`: `additional_info` map round-trip
- [x] write tests for `model_update_active_model`: PATCH on a single field re-projects all 21 columns correctly
- [x] run `cargo test -p cf-gears-model-registry --lib` — must pass before task 5

### Task 5: Simplify and test `build_minimal_info` fallback

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/mapper.rs`

- [ ] keep `build_minimal_info` (lines 468-589) as defensive fallback — since `display_name` and `ctx_max_input_tokens` will always be populated post-migration (with DEFAULTs if missing), it rarely triggers
- [ ] simplify the graceful-degradation pattern where possible (remove redundant null-handling now that DB defaults are in place)
- [ ] write tests for `build_minimal_info` fallback path: triggered when `display_name` is empty or `ctx_max_input_tokens` is 0
- [ ] run `cargo test -p cf-gears-model-registry --lib` — must pass before task 6

### Task 6: Update test fixtures and targeted test fixes

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/mapper_test.rs`
- Modify: `gears/model-registry/model-registry/src/infra/storage/odata_mapper.rs`

- [ ] rewrite `make_model_entity` fixture (mapper_test.rs line 139) to populate all 21 new fields
- [ ] repurpose `model_entity_to_v1_fallback_when_info_null` (line 381): with `display_name` and `ctx_max_input_tokens` now required scalar columns (with DB defaults), this test no longer tests the JSONB-missing case — rename to test "entity with default fields reconstructs without panic"
- [ ] repurpose `model_entity_to_v1_handles_malformed_jsonb` (line 522): rename to test "missing `provider_settings` JSONB returns null on the wire"
- [ ] update `model_create_denormalized_match_info` (line 404): drop the `am.info` assertion (line 436) — `info` column no longer exists
- [ ] update `model_extract_cursor_value_round_trip` in `odata_mapper.rs` (line 428): update the `model::Model` literal with all required fields
- [ ] write tests confirming all fixture rewires still pass (re-run mapper_test and odata_mapper_test)
- [ ] run `cargo test -p cf-gears-model-registry --lib` — must pass before task 7

### Task 7: Integration test verification

**Files:**
- Modify: `gears/model-registry/model-registry/tests/integration.rs`

- [ ] run existing integration tests to confirm wire DTO round-trip still works (handler `serde_json::from_value::<ModelInfoV1>(dto.info)` path is unchanged)
- [ ] add integration test: create model with full `ModelInfoV1` → DB row has all 21 new columns populated, `info` column absent
- [ ] add integration test: read model back → reconstructed `ModelInfoV1` matches input (all fields, including nested)
- [ ] add integration test: PATCH a single field → DB columns update correctly, response reflects change
- [ ] add integration test: capability merge — OData booleans from columns, rest from JSONB
- [ ] run `cargo test -p cf-gears-model-registry --test integration` — must pass before task 8

### Task 8: Update ADR-0005

**Files:**
- Modify: `gears/model-registry/docs/ADR/0005-cpt-cf-model-registry-adr-gts-typed-provider-settings.md`

- [ ] update line 63 (Consequences): reword — there are now five polymorphic/JSONB columns (`provider_settings`, `capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`), tagged by scalar `gts_type`
- [ ] add a "Consequences (added 2026-07-24)" bullet summarizing the schema decomposition
- [ ] run `make gts-docs` — must pass (ADR references validate)
- [ ] write tests verifying ADR description matches actual schema (if test infra exists for docs; otherwise skip)

### Task 9: Update DESIGN.md

**Files:**
- Modify: `gears/model-registry/docs/DESIGN.md`

- [ ] replace the `info` row in lines 1024-1058 with the new 21 columns
- [ ] rewrite line 1050 to state: "Scalar columns are the source of truth; the four additional JSONB columns hold sub-objects that don't promote cleanly; `provider_settings` is the only polymorphic JSONB column identified by `gts_type`"
- [ ] run `make gts-docs` — must pass (DESIGN references validate)

### Task 10: Verify acceptance criteria

**Files:**
- Modify: `docs/plans/20260724-drop-models-info-jsonb.md`

- [ ] verify `info` column is dropped from the `models` table (grep for `info: Set` and `e.info` in `mapper.rs`)
- [ ] verify all 17 scalar columns + 4 JSONB columns are populated correctly in `model_create_active_model` and `model_update_active_model`
- [ ] verify wire DTO (`ModelDto { info: JsonValue }`) is unchanged — `dto.rs` and `handlers.rs` untouched
- [ ] verify SDK `ModelInfoV1<P>` is unchanged — `llm-gateway-sdk` consumers (`mock_registry.rs`, `plugin.rs`) still compile
- [ ] verify OData filter surface unchanged — 15 fields, 11 indexes
- [ ] run `cargo test -p cf-gears-model-registry --lib` — all mapper/odata/dto unit tests pass
- [ ] run `cargo test -p cf-gears-model-registry --test integration` — all integration tests pass
- [ ] run `cargo test --workspace` — cross-gear sanity
- [ ] run `make test-sqlite` — SQLite end-to-end
- [ ] run `cargo clippy -p cf-gears-model-registry -- -D warnings` — clean
- [ ] run `make dylint` — architectural lints clean
- [ ] run `make gts-docs` — ADR/DESIGN references validate

### Task 11: Update documentation and finalize

**Files:**
- Modify: `docs/plans/20260724-drop-models-info-jsonb.md`

- [ ] update `gears/model-registry/docs/DESIGN.md` — cross-check that the new storage layout is fully documented (lines 1024-1058)
- [ ] verify `CLAUDE.md` patterns are still accurate — the "OData Filtering Requires Real Columns" pattern is now even more strictly followed (no JSONB `info` at all)
- [ ] move this plan to `docs/plans/completed/`

## Key gotchas

1. **NOT NULL DEFAULTs on SQLite** — `display_name`, `ctx_max_input_tokens`, `allow_parameter_override` must have DEFAULTs at CREATE time (SQLite cannot ALTER ADD NOT NULL). Use `DEFAULT ''`, `DEFAULT 0`, `DEFAULT 0` respectively. Application layer can override.
2. **`#[non_exhaustive]` SDK types** — read path uses `serde_json::json!{...}` then `serde_json::from_value::<ModelV1>(value)` (same pattern as `build_minimal_info`).
3. **Capability merge** — read path must take 4 promoted booleans from columns, rest from `capabilities_full` JSONB; columns are authoritative.
4. **Update path becomes verbose** — every PATCH that touches `info.*` re-projects all 21 columns. Correct but slightly noisier SQL. (Optimization is out of scope.)
5. **Foreign-gear consumers** — only `llm-gateway-demo/src/mock_registry.rs` and `llm-gateway-sdk/src/models/plugin.rs` reference `ModelInfoV1` outside model-registry. Both unchanged (SDK shape preserved).

## Post-Completion
*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification** (if applicable):
- Manual smoke test against a freshly-created DB:
  1. `POST /model-registry/v1/models` with full info payload.
  2. Inspect row: `SELECT * FROM models WHERE id = ...;` — confirm `info` column is gone; new columns populated.
  3. `GET /model-registry/v1/models/{canonical_id}` — response carries full reconstructed `info` JSON.
  4. `PATCH /model-registry/v1/models/{canonical_id}` with a single field — DB column updates, response reflects change.
