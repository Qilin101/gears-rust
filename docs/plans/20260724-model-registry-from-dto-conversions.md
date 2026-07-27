# Plan: Replace serde round-trips in model-registry handlers with `From` impls

## Overview

`gears/model-registry/` REST handlers currently convert SDK entities into DTOs via `serde_json::from_value(serde_json::to_value(...))` — a serialize→deserialize round-trip. The pattern exists because the SDK entity structs are `#[non_exhaustive]`, which prevents struct-literal construction outside the SDK crate.

This plan removes `#[non_exhaustive]` from the SDK entity structs and replaces each round-trip with a normal `impl From<SourceSdkType> for TargetDto`. After the change, the model-registry gear follows the same conversion idiom used by ~13 other gears in the workspace (file-parser, types-registry, nodes-registry, usage-collector, bss/ledger, users-info, etc.).

**Scope:** structs only. The 6 enums and `ModelRegistryError` stay `#[non_exhaustive]`.

## Context (from discovery)

**Files involved:**

- `gears/model-registry/model-registry-sdk/src/models/entity.rs` — `ModelV1<P>` (line 23), `ProviderV1` (line 79)
- `gears/model-registry/model-registry-sdk/src/models/info.rs` — `ModelInfoV1<P>` (line 50)
- `gears/model-registry/model-registry-sdk/src/models/common.rs` — `ModelCapabilities` (line 206), `DisabledCapabilities` (line 305)
- `gears/model-registry/model-registry/src/api/rest/dto.rs` — DTO definitions (290 lines), doc comment at lines 7–18 explains the round-trip
- `gears/model-registry/model-registry/src/api/rest/handlers.rs` — 8 round-trip sites at lines 38, 57, 103, 141, 169, 189, 259, 397
- `gears/model-registry/model-registry/src/infra/storage/mapper_test.rs` — tests at line 105 use `serde_json::from_value` for `ModelInfoV1` (commented as workaround)
- `gears/model-registry/model-registry/src/infra/storage/sea_orm_repo.rs` — test helper `make_create_model_req` at line 1355
- `gears/model-registry/model-registry/src/domain/service.rs` — test helpers at lines 1113, 1414, 2070
- `gears/model-registry/model-registry/tests/integration.rs` — `make_create_model_req` at line 361

**Patterns observed:**

- All 13 other REST gears in the workspace use `impl From<DomainType> for DtoType` + `.into()` (surveyed: file-parser/mappers.rs, types-registry, nodes-registry, usage-collector/dto.rs, file-storage/dto.rs, bss/ledger/dto.rs, mini-chat, account-management, oagw, users-info/users.rs, simple-user-settings)
- `docs/toolkit_unified_system/04_rest_operation_builder.md:182-197` documents `UserDto::from(user)` as the canonical idiom
- No dylint lint enforces or prohibits the round-trip pattern
- External consumers (`llm-gateway-sdk`, `llm-gateway-demo`) use `ModelInfoV1` only as opaque aggregates — they don't construct SDK types, so removing `#[non_exhaustive]` does not break them

**Why it was originally added:** `#[non_exhaustive]` on structs allows the SDK to add fields in minor versions without breaking downstream code that constructs the type. This is reasonable for some libraries, but here it forces a runtime failure mode (extra JSON round-trip) in every request handler — and the SDK types are already versioned via the `V1` suffix and GTS schema versioning. The benefits don't justify the cost.

## Development Approach

- **testing approach:** Regular (code first, then tests)
- complete each task fully before moving to the next
- make small, focused changes
- **every task MUST include new/updated tests** for code changes in that task
- **all tests must pass before starting next task** — no exceptions
- **update this plan file when scope changes during implementation**
- run `cargo test -p cf-gears-model-registry-sdk` and `cargo test -p cf-gears-model-registry` after each task
- maintain backward compatibility for wire formats and SDK trait signatures

## Testing Strategy

- **unit tests:** required for every task
  - each `From` impl gets a unit test in `dto.rs` (or wherever it lives)
  - each handler gets a test verifying it returns the expected DTO shape
  - existing tests that round-tripped `ModelInfoV1` via JSON should be migrated to struct literals and stay green
- **e2e tests:** project doesn't have a separate e2e suite — integration tests in `model-registry/tests/integration.rs` cover end-to-end flow
- **verify no regression in consumers:** `cargo build -p cf-gears-llm-gateway-sdk -p cf-gears-llm-gateway-demo` after the SDK change

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

1. Drop `#[non_exhaustive]` from 5 SDK structs.
2. Add `impl From<…> for …Dto` for `ProviderDto` and `ModelDto` in `dto.rs` (or a sibling file). The DTO status fields are already `String`, so enum-to-string mapping already exists in `mapper.rs` and can be reused — or the `From` impl can call a small helper.
3. Replace 8 round-trip call sites in `handlers.rs` with `.into()` / `Type::from(...)`.
4. Simplify test helpers in `mapper_test.rs`, `sea_orm_repo.rs`, `service.rs`, `integration.rs` to use struct literals (optional cleanup, keeps tests more readable).
5. Update the doc comment in `dto.rs:7-18` to describe the new pattern.

`ModelInfoV1<P>` and `ModelCapabilities` / `DisabledCapabilities` also lose `#[non_exhaustive]` so the `From<ModelV1> for ModelDto` impl can be written in the gear crate (since it needs to access `info.provider_settings` to produce the DTO's `info: JsonValue` field).

## Technical Details

**DTO `info` field for ModelDto** (dto.rs:138-146):

```rust
pub struct ModelDto {
    pub id: Uuid,
    pub canonical_id: String,
    pub lifecycle_status: String,
    pub approval_status: String,
    pub info: JsonValue,
}
```

The `From<ModelV1> for ModelDto` impl needs to produce `info: JsonValue` from `source.info: ModelInfoV1<P>`. Since `ModelInfoV1` already derives `Serialize`, this is just `serde_json::to_value(&source.info).unwrap_or(JsonValue::Null)` (or `.expect("ModelInfoV1 serialization")` — but the gear crate allows `unwrap` only in tests per CLAUDE.md; in production code use a proper error). Since `From` cannot return a `Result`, we use `serde_json::to_value(&source.info).unwrap_or(JsonValue::Null)` — `ModelInfoV1` is fully owned data with no failure modes that would arise from re-serialization, so this is safe.

**Status enum → String mapping:**

The DTO uses `String` for `status`, `lifecycle_status`, `approval_status`. The conversion can use the existing string formatters from `mapper.rs` (`provider_status_str`, `lifecycle_status_str`, `approval_status_str`) — but those live in `infra/storage/`. A cleaner approach: extract a tiny `enum_to_str` helper into the SDK (or a `domain/` module) and use it both from `mapper.rs` and the new `From` impls. Alternatively, do the mapping inline in the `From` impl using `match` (since the enum is `#[non_exhaustive]`, keep the wildcards in the new impls too).

Recommended approach: keep the mapping local to the `From` impl using `match`, mirroring `mapper.rs`. Don't move shared helpers — duplication of 4 match expressions is cheaper than a new shared module.

**Builder for `ModelV1` / `ProviderV1` construction in tests:**

`CreateProviderRequestV1` and `UpdateProviderRequestV1` already have builders (`request.rs`). After removing `#[non_exhaustive]`, tests can construct SDK entities directly via struct literals — much more readable than the current JSON fixture approach. This is optional cleanup but recommended.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code changes, tests, documentation updates
- **Post-Completion** (no checkboxes): manual verification

## Implementation Steps

### Task 1: Drop `#[non_exhaustive]` from SDK entity structs

**Files:**
- Modify: `gears/model-registry/model-registry-sdk/src/models/entity.rs`
- Modify: `gears/model-registry/model-registry-sdk/src/models/info.rs`
- Modify: `gears/model-registry/model-registry-sdk/src/models/common.rs`

- [x] remove `#[non_exhaustive]` from `ModelV1<P>` at entity.rs:23
- [x] remove `#[non_exhaustive]` from `ProviderV1` at entity.rs:79
- [x] remove `#[non_exhaustive]` from `ModelInfoV1<P>` at info.rs:50
- [x] remove `#[non_exhaustive]` from `ModelCapabilities` at common.rs:206
- [x] remove `#[non_exhaustive]` from `DisabledCapabilities` at common.rs:305
- [x] run `cargo build -p cf-gears-model-registry-sdk` — must compile (no external consumers construct these types via struct literal yet)
- [x] run `cargo build -p cf-gears-llm-gateway-sdk -p cf-gears-llm-gateway-demo` — verify consumers still compile
- [x] run SDK inline tests: `cargo test -p cf-gears-model-registry-sdk` — must pass

### Task 2: Add `From` impls in `dto.rs`

**Files:**
- Modify: `gears/model-registry/model-registry/src/api/rest/dto.rs`

- [x] add `impl From<ProviderV1> for ProviderDto` near the `ProviderDto` definition — maps `status: ProviderStatus` → `"active"|"disabled"` (with `_ =>` wildcard since the enum is `#[non_exhaustive]`), `created_at`/`updated_at` `DateTime<Utc>` → `String` via `.to_rfc3339()`, `metadata` passthrough
- [x] add `impl From<ModelV1> for ModelDto` — maps `lifecycle_status: LifecycleStatus` and `approval_status: ApprovalStatus` to strings with `match`+wildcard, `info: ModelInfoV1<P>` → `info: JsonValue` via `serde_json::to_value(&source.info).unwrap_or(JsonValue::Null)`
- [x] update doc comment at dto.rs:7-18 to describe the new `From`-based pattern and remove the round-trip explanation
- [x] write unit tests for `From<ProviderV1> for ProviderDto`: covers happy path + status string mapping (each `ProviderStatus` variant)
- [x] write unit tests for `From<ModelV1> for ModelDto`: covers happy path + lifecycle/approval string mapping (each variant) + info serialization
- [x] run `cargo test -p cf-gears-model-registry --lib api::rest::dto` — must pass

### Task 3: Replace round-trip sites in `handlers.rs`

**Files:**
- Modify: `gears/model-registry/model-registry/src/api/rest/handlers.rs`

- [x] replace lines 37-41 in `get_provider` with `Ok(Json(ProviderDto::from(provider)))`
- [x] replace lines 53-62 in `list_providers` with `.map(ProviderDto::from).collect::<Vec<_>>()`
- [x] replace lines 102-106 in `create_provider` with `let dto: ProviderDto = provider.into();`
- [x] replace lines 140-144 in `update_provider` with `let dto: ProviderDto = provider.into();`
- [x] replace lines 169-173 in `get_model` with `Ok(Json(ModelDto::from(model)))`
- [x] replace lines 185-194 in `list_models` with `.map(ModelDto::from).collect::<Vec<_>>()`
- [x] replace lines 259-263 in `create_model` with `let dto: ModelDto = model.into();`
- [x] replace lines 397-401 in `update_model` with `let dto: ModelDto = model.into();`
- [x] verify no remaining `serde_json::from_value(to_value(...))` patterns in `handlers.rs` (`grep -n "from_value.*to_value\|to_value.*from_value"`)
- [x] run `cargo build -p cf-gears-model-registry` — must compile
- [x] run full gear tests: `cargo test -p cf-gears-model-registry` — must pass

### Task 4: Migrate test helpers from JSON round-trip to struct literals

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/mapper_test.rs`
- Modify: `gears/model-registry/model-registry/src/infra/storage/sea_orm_repo.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`
- Modify: `gears/model-registry/model-registry/tests/integration.rs`

- [ ] migrate `mapper_test.rs:105` from `serde_json::from_value` to direct `ModelInfoV1 { ... }` literal (or builder)
- [ ] migrate `sea_orm_repo.rs:1355` `make_create_model_req` helper to construct `ModelInfoV1` directly
- [ ] migrate `service.rs:1113, 1414, 2070` test helpers to direct construction
- [ ] migrate `integration.rs:361` `make_create_model_req` to direct construction
- [ ] update stale comment at `mapper_test.rs:17-20` ("all SDK types are #[non_exhaustive]") to reflect that the attribute is gone
- [ ] update stale comment at `sea_orm_repo.rs:1282-1283` similarly
- [ ] run full gear tests: `cargo test -p cf-gears-model-registry` — must pass
- [ ] run integration tests: `cargo test -p cf-gears-model-registry --test integration` — must pass

### Task 5: Verify SDK consumer crates still compile and pass

**Files:**
- (no file changes — verification only)

- [ ] run `cargo build -p cf-gears-llm-gateway-sdk -p cf-gears-llm-gateway-demo` — must succeed
- [ ] run `cargo test -p cf-gears-llm-gateway-sdk` — must pass
- [ ] run `cargo test -p cf-gears-llm-gateway-demo` — must pass

### Task 6: Workspace-wide verification

**Files:**
- (no file changes — verification only)

- [ ] run `cargo fmt --all` — format all touched files
- [ ] run `cargo clippy -p cf-gears-model-registry-sdk -p cf-gears-model-registry --all-targets -- -D warnings` — zero warnings
- [ ] run `make dylint` — no new lints triggered (DTOs already live in `api/rest/`, no `Serialize`/`Deserialize` in contract layer changes expected)
- [ ] run `cargo build --workspace` — must compile
- [ ] run `cargo test --workspace` — must pass
- [ ] grep workspace for any remaining `serde_json::from_value(serde_json::to_value(...)` outside test fixtures: `grep -rn "from_value(serde_json::to_value\|serde_json::from_value(serde_json::to_value" gears/ examples/ libs/` — only acceptable hits are in `model-registry-sdk` itself if any fixture still uses it (should be zero)

### Task 7: Verify acceptance criteria

- [ ] all 8 round-trip sites in `handlers.rs` replaced with `.into()` / `Type::from(...)`
- [ ] 5 SDK structs no longer have `#[non_exhaustive]`
- [ ] 2 `From` impls exist in `dto.rs` with unit tests
- [ ] test helpers in 4 files migrated to struct literals
- [ ] no consumer crate broken
- [ ] workspace test suite green
- [ ] doc comment at `dto.rs:7-18` updated
- [ ] stale comments at `mapper_test.rs:17-20` and `sea_orm_repo.rs:1282-1283` updated

### Task 8: Update documentation

- [ ] if a new pattern emerged (a documented rationale for why this gear's structs are NOT non_exhaustive despite other gears potentially using the attribute), update `docs/toolkit_unified_system/04_rest_operation_builder.md` — otherwise no doc changes needed
- [ ] move this plan to `docs/plans/completed/` with `mkdir -p docs/plans/completed && git mv docs/plans/20260724-model-registry-from-dto-conversions.md docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification:**

- spot-check the OpenAPI spec at `make openapi` — confirm response shapes for `ProviderDto` and `ModelDto` are unchanged (the wire format must not change)
- run the example server: `make example` and hit `/model-registry/v1/providers` + `/model-registry/v1/models` to confirm responses match pre-change output

**External system updates:**

- `llm-gateway-sdk` and `llm-gateway-demo` rebuild automatically as workspace deps — no manual step needed
- any external consumer outside this workspace that pattern-matches SDK types will see the same behavior (`#[non_exhaustive]` on enums is unchanged); external consumers that construct `ModelV1` / `ProviderV1` via struct literals will start to work but don't need to change unless they want to
