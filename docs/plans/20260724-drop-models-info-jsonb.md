# Plan: Drop `info` JSONB, promote `ModelInfoV1` fields to typed columns

## Context

Today the `models` table has a JSONB `info` column that is the "authoritative source of truth" for `ModelInfoV1`, plus ~13 scalar columns that denormalize a subset of those fields for OData filtering (`gears/model-registry/docs/DESIGN.md:1050`).

We want to remove the duplication: drop the `info` JSONB column entirely and store every `ModelInfoV1` field in either a typed scalar column or one of four small JSONB sub-object columns (`capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`). The polymorphic `provider_settings` JSONB column (per-provider routing/pricing, discriminator is `gts_type`) stays as-is.

The public SDK `ModelInfoV1<P>` and the wire DTO `ModelDto { info: JsonValue }` are **unchanged** — only the storage layout moves. `llm-gateway-sdk` plugins that take `ModelInfoV1` continue to work.

### User decisions (captured)
- Scope: drop `info` only, keep `provider_settings`.
- Promote every `ModelInfoV1` field to a typed column (or one of four small JSONB sub-objects).
- OData filter surface stays at the current 15 fields.
- `disabled_capabilities` → JSONB column.
- `allow_extra_params` → JSONB column.
- `performance.{response_latency_ms, tokens_per_second}` → two scalar columns.
- **No separate migration** — modify `initial_001.rs` directly since the model-registry gear is not yet merged/deployed.

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

---

## Critical files

- `gears/model-registry/model-registry/src/infra/storage/migrations/initial_001.rs` — rewrite CREATE TABLE
- `gears/model-registry/model-registry/src/infra/storage/entity/model.rs` — drop `info`, add 21 fields
- `gears/model-registry/model-registry/src/infra/storage/mapper.rs` — rewrite read + write paths
- `gears/model-registry/model-registry/src/infra/storage/mapper_test.rs` — update `make_model_entity` fixture; targeted test fixes
- `gears/model-registry/model-registry/src/infra/storage/odata_mapper.rs` — fix the `model_extract_cursor_value_round_trip` entity literal (test only)
- `gears/model-registry/docs/DESIGN.md` — lines 1024-1058 rewrite
- `gears/model-registry/docs/ADR/0005-cpt-cf-model-registry-adr-gts-typed-provider-settings.md` — clarifying edits

No changes needed in `dto.rs`, `handlers.rs`, `cache.rs`, `service.rs`, `sea_orm_repo.rs`, `dto_test.rs`, `integration.rs`, `llm-gateway-demo/src/mock_registry.rs`.

---

## Implementation steps

### 1. Rewrite `migrations/initial_001.rs`

Replace the existing `models` CREATE TABLE (lines 46-73) with one that:
- Drops the `info` column (line 53).
- Keeps all existing 22 columns (identity, lifecycle, the 13 already-promoted columns, `provider_settings`, timestamps).
- Adds the 17 new scalar columns with `NOT NULL DEFAULT` where required (`display_name DEFAULT ''`, `ctx_max_input_tokens DEFAULT 0`, `allow_parameter_override DEFAULT 0`).
- Adds the 4 new JSONB columns (`capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`) using the existing `jsonb_nullable` type variable.

Indexes (lines 85-96) stay — they cover the existing OData columns.

Update the test (`initial_migration_up_down_roundtrip`, lines 124-153) to reflect the new schema: the `INSERT INTO providers` doesn't touch `models`, so it should still pass; verify the migration up/down roundtrip works with the new column list.

### 2. Update `entity/model.rs`

- Remove `pub info: Option<serde_json::Value>` (lines 35-36).
- Add the 17 scalar fields + 4 JSONB fields (`capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`). Each JSONB column uses `#[sea_orm(column_type = "JsonBinary", nullable)]` mirroring the existing `provider_settings` (line 39).
- Rewrite the doc comment block (lines 8-17) — `info` is gone; scalar columns are now the source of truth, with the four JSONB sub-object columns for fields that don't promote cleanly, plus the polymorphic `provider_settings` keyed by `gts_type`.

### 3. Rewrite `mapper.rs`

**`model_entity_to_v1` (currently lines 153-173)**: instead of `serde_json::from_value(e.info)`, build a JSON value from the new columns + `provider_settings` JSONB + the four new JSONB sub-objects, then `serde_json::from_value::<ModelV1>(value)`. Same JSON-value-then-roundtrip pattern as `build_minimal_info` (line 468). The four scalar booleans override whatever is in `capabilities_full.vision.enabled` etc. to keep OData columns authoritative.

**New helper `build_capabilities(e: &entity::model::Model) -> ModelCapabilities`**: merge the 4 scalar bools with `e.capabilities_full` JSONB.

**`model_create_active_model` (lines 181-230)**: drop `info: Set(...)`. Add `Set(...)` for every new column from `req.info.*`. Add JSONB `Set(...)` for `capabilities_full` (built from `req.info.capabilities` minus the 4 promoted booleans), `default_parameters`, `additional_info`, `disabled_capabilities_full`. Extract `provider_settings` (unchanged).

**`model_update_active_model` (lines 246-314)**: identical structure, but now EVERY PATCH that touches an info field re-projects all new columns. `apply_info_patches` (lines 320-376) is **unchanged** — it operates on in-memory `ModelInfoV1`, independent of storage.

**`build_minimal_info` (lines 468-589)**: keep as defensive fallback. Since `display_name` and `ctx_max_input_tokens` will always be populated post-migration (with DEFAULTs if missing), this rarely triggers. Keep the graceful-degradation pattern but it can be simplified.

### 4. DTO layer, handlers, cache, service, repo — no changes

Confirmed:
- `dto.rs` (`ModelDto { info: JsonValue }`, `CreateModelRequestDto { info: JsonValue }`, `UpdateModelRequestDto` opaque-JSON fields): wire shape preserved. Handler roundtrips via `serde_json::from_value::<ModelInfoV1>(dto.info)` (handlers.rs:244-249) keep working.
- `cache.rs` stores the full `ModelV1` (post-deserialization), so the wire/SDK shape is unchanged.
- All `make_create_model_req` test helpers build `ModelInfoV1` and don't touch DB column layout — unchanged.
- `llm-gateway-demo/src/mock_registry.rs` builds `ModelV1` via JSON strings — unchanged.

### 5. Test updates

- **`mapper_test.rs::make_model_entity` (line 139)** — rewrite to populate all 21 new fields.
- **`mapper_test.rs::model_entity_to_v1_fallback_when_info_null` (line 381)** — repurpose. With `display_name` and `ctx_max_input_tokens` now required scalar columns (with DB defaults), this test no longer tests the JSONB-missing case. Rename to test "entity with default fields reconstructs without panic".
- **`mapper_test.rs::model_entity_to_v1_handles_malformed_jsonb` (line 522)** — repurpose to test "missing `provider_settings` JSONB returns null on the wire".
- **`mapper_test.rs::model_create_denormalized_match_info` (line 404)** — drop the `am.info` assertion (line 436).
- **`odata_mapper.rs::model_extract_cursor_value_round_trip` (line 428)** — update the `model::Model` literal with all required fields.
- All other tests in `mapper_test.rs`, `sea_orm_repo.rs`, `service.rs`, `integration.rs`, `dto_test.rs` — unchanged.

### 6. ADR-0005

Targeted edits only — the ADR justifies `provider_settings` (polymorphic, kept), not `info`:
- Line 63 (Consequences): update wording — there are now five polymorphic/JSONB columns (`provider_settings`, `capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`), tagged by scalar `gts_type`.
- Add a "Consequences (added 2026-07-24)" bullet summarizing the schema decomposition.

### 7. DESIGN.md

Lines 1024-1058: replace the `info` row with the new 21 columns; rewrite line 1050 to state "Scalar columns are the source of truth; the four additional JSONB columns hold sub-objects that don't promote cleanly; `provider_settings` is the only polymorphic JSONB column identified by `gts_type`."

---

## Key gotchas

1. **NOT NULL DEFAULTs on SQLite** — `display_name`, `ctx_max_input_tokens`, `allow_parameter_override` must have DEFAULTs at CREATE time (SQLite cannot ALTER ADD NOT NULL). Use `DEFAULT ''`, `DEFAULT 0`, `DEFAULT 0` respectively. Application layer can override.
2. **`#[non_exhaustive]` SDK types** — read path uses `serde_json::json!{...}` then `serde_json::from_value::<ModelV1>(value)` (same pattern as `build_minimal_info`).
3. **Capability merge** — read path must take 4 promoted booleans from columns, rest from `capabilities_full` JSONB; columns are authoritative.
4. **Update path becomes verbose** — every PATCH that touches `info.*` re-projects all 21 columns. Correct but slightly noisier SQL. (Optimization is out of scope.)
5. **Foreign-gear consumers** — only `llm-gateway-demo/src/mock_registry.rs` and `llm-gateway-sdk/src/models/plugin.rs` reference `ModelInfoV1` outside model-registry. Both unchanged (SDK shape preserved).

---

## Verification

```bash
cargo test -p cf-gears-model-registry --lib                            # mapper, odata_mapper, dto unit tests
cargo test -p cf-gears-model-registry --test integration               # full integration
cargo test --workspace                                                  # cross-gear sanity
make test-sqlite                                                        # SQLite end-to-end
make gts-docs                                                           # ADR/DESIGN references validate
cargo clippy -p cf-gears-model-registry -- -D warnings
make dylint                                                             # architectural lints
```

Manual smoke test against a freshly-created DB:
1. `POST /model-registry/v1/models` with full info payload.
2. Inspect row: `SELECT * FROM models WHERE id = ...;` — confirm `info` column is gone; new columns populated.
3. `GET /model-registry/v1/models/{canonical_id}` — response carries full reconstructed `info` JSON.
4. `PATCH /model-registry/v1/models/{canonical_id}` with a single field — DB column updates, response reflects change.
