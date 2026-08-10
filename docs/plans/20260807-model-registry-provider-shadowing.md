# Model Registry — Provider Shadowing & Visibility Resolution

## Overview

Close the implementation gaps catalogued in [`gears/model-registry/docs/impl-gaps.md`](../../gears/model-registry/docs/impl-gaps.md)
(groups **A–G**), bringing `gears/model-registry/` in line with
[`DESIGN.md`](../../gears/model-registry/docs/DESIGN.md) §3.5 "Tenant Visibility Resolution".

**The problem.** Everything traces back to one design change the code predates: shadowing is keyed on
the **provider slug**, and model visibility is `model.provider_id ∈ allow_list(T0)`. The shipped code
has no notion of a winning provider on any model read path — it dedupes merged results by
`canonical_id` instead. The two keys agree only when the closer tenant happens to own a model of the
same name. In the ordinary case — a tenant installs a shadow provider and has not yet created models
under it — the ancestor's models **survive the merge and are served to the tenant the shadow exists
to hide them from**. That is the headline bug (B1), and it is a compliance-isolation failure, not a
cosmetic one.

**What this fixes:**

- Shadowed-provider models are excluded from eval reads (list and get).
- Disabled providers hide their models on every eval read path.
- `get_tenant_model` resolves the provider slug before reading the model, so a cached ancestor model
  is never served to a subtree that has since shadowed its provider.
- A `$filter` mentioning `lifecycle_status` no longer widens visibility (B4).
- An ancestor **provider** query failure fails the read closed instead of silently un-shadowing (B5).
- `create_model` accepts only the caller's own tenant's providers, with a distinct `ProviderNotOwned`
  403 (E1–E3).
- The management listing (`GET /model-registry/v1/admin/models`) ships, sharing one repository query
  with the eval path and marking `shadowed` / `provider_disabled` / `available_for_eval` per row.

**Integration.** All changes are internal to `gears/model-registry/`. The one wire-visible change is
additive: `ModelV1.provider_id` and `ModelDto.provider_id` (v1's additive-only promise holds), plus a
new endpoint and a new SDK trait method. Group **H** (acknowledged technical debt) and group **I**
(open questions) are explicitly out of scope; see Post-Completion.

## Context (from discovery)

**Files/components involved:**

- `gears/model-registry/model-registry-sdk/src/` — `models/entity.rs` (`ModelV1`), `models/mod.rs`
  (re-exports), `errors.rs` (`ModelRegistryError`), `api.rs` (`ModelRegistryClientV1`, 10 methods →
  11)
- `gears/model-registry/model-registry/src/domain/` — `service.rs` (2663 lines; the three read paths
  plus `create_model`), `inheritance.rs` (`InheritanceContext`, `find_in_chain`,
  `merge_inherited_page`), `cache.rs` (`CacheService`, `cache_key`), `repo.rs` (repository traits),
  `error.rs` (`DomainError`), `local_client.rs`
- `gears/model-registry/model-registry/src/infra/storage/` — `model_repo.rs`, `provider_repo.rs`,
  `mapper.rs` (`model_entity_to_v1`), `migrations/initial_001.rs`
- `gears/model-registry/model-registry/src/api/rest/` — `dto.rs`, `routes.rs` (10 registered
  operations), `handlers.rs`, `error.rs`, `parse.rs`
- Tests: `domain/service.rs` in-file `mod tests`, `tests/integration.rs` (SQLite + mocked clients),
  `api/rest/dto_test.rs`, `infra/storage/mapper_test.rs`

**Related patterns found:**

- Repositories take `&impl DBRunner` + `&AccessScope`; tenant isolation is enforced by
  `SecureEntityExt::secure().scope_with(scope)`.
- Ancestor scopes are always **constructed** (`AccessScope::for_tenant(ancestor_id)`), never widened
  from the caller's scope — except in `create_model`, which is exactly gap E1.
- Cache keys are `mr:{tenant_id}:{entity}:{id}` via `cache_key()`; `invalidate_tenant` sweeps the
  `mr:{tenant_id}:` prefix. TTL is chosen by `Ownership` (own 30 min / inherited 5 min).
- Authorization is one `PolicyEnforcer::access_scope(ctx, resource, action, None)` call per service
  method, with action constants in `domain::service::actions`.
- OData filtering maps each field to exactly one real column via `FieldToColumn` — no JSONB paths, no
  joins, no set literals (hence A3's repository-level `provider_id IN (…)`).

**Dependencies identified:**

- `tenant-resolver` (`get_ancestors`) and `authz-resolver` (`PolicyEnforcer`) via ClientHub — both
  already wired, no new gear dependency.
- `ModelV1` struct-literal sites the new field breaks — **six**: `infra/storage/mapper.rs:234`,
  `sdk/models/entity.rs:62` and `:144`, `api/rest/dto_test.rs:547`, `domain/service.rs:1407` and
  `:2171`. (`tests/integration.rs:430` and `service.rs:1129` are function *return types*, not
  literals — they build through `ModelRepository::create` and need no Task 1 edit.) `ModelDto` gains
  a field and breaks two more literals (`dto_test.rs:169`, `:351`) plus its `From` impl. A bounded,
  intended compile break — the SDK entity structs are deliberately not `#[non_exhaustive]`.
- **Cross-crate consumer**: `gears/llm-gateway/llm-gateway-demo/` implements `ModelRegistryClientV1`
  (`src/mock_registry.rs:61`) and is a workspace member. The trait has no default bodies, so the 11th
  method is a compile error there. Worse, that mock builds `ModelV1` via `serde_json::from_str` over
  two `const` fixtures (`mock_registry.rs:285`, `:375`) — a required `provider_id` is a **runtime**
  deserialization failure, not a compile error. Both are handled in Tasks 1 and 10.
- `ProviderRepository::list` is paginated (`LimitCfg { default: 20, max: 100 }`,
  `provider_repo.rs:104-107`). `ChainProviders` needs each tenant's **complete** provider set — a
  truncated fetch silently corrupts the allow-list. Task 3 adds an unpaginated fetch.
- `merge_inherited_page`'s ancestor closure receives only an `AccessScope`, which exposes no tenant
  accessor. The per-tenant allow-list slice therefore cannot be computed inside the closure — the
  helper's signature has to change (Task 8).
- Only one migration exists (`initial_001`) and it defines the whole schema, so the gear is
  pre-release: the A4 index is amended into it rather than added as a second migration.

## Development Approach

- **testing approach**: Regular (code first, then tests)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- maintain backward compatibility (all wire changes here are additive)

### Project-specific verification

Scope verification to the affected crates — do **not** run workspace-wide:

```bash
cargo test -p cf-gears-model-registry -p cf-gears-model-registry-sdk -p cf-gears-llm-gateway-demo
cargo clippy -p cf-gears-model-registry -p cf-gears-model-registry-sdk -p cf-gears-llm-gateway-demo \
  --all-targets -- -D warnings
cargo fmt -p cf-gears-model-registry -p cf-gears-model-registry-sdk -- --check
```

`llm-gateway-demo` is included because it implements `ModelRegistryClientV1` and deserializes
`ModelV1` from JSON fixtures — omitting it means CI is the first place both breaks surface.

Run `make dylint` and `make openapi` once at the acceptance-criteria task (routes/DTO changes affect
the generated spec; layer-separation lints affect the new SDK/domain/REST types).

## Testing Strategy

- **unit tests**: required for every task (see Development Approach above). Domain-layer tests live
  in the in-file `mod tests` of the module under change (`service.rs`, `inheritance.rs`, `cache.rs`).
- **integration tests**: `gears/model-registry/model-registry/tests/integration.rs` — SQLite +
  mocked `TenantResolverClient` / `AuthZResolverClient`, following the existing
  `setup_db()` → mocks → `build_service(db, resolver)` pattern documented in CLAUDE.md. Every
  multi-tenant shadowing scenario (G3, G4) belongs here, not in unit tests: the bug only reproduces
  with a real repository against a real chain.
- **e2e tests**: not applicable — this project has no UI-based e2e suite.
- The group **G** items from `impl-gaps.md` are distributed across the tasks that cause them rather
  than batched into one task, so each task's tests prove that task's behavior:
  G1→Task 9, G2/G3→Task 8, G4→Tasks 7/8/11, G5→Task 6, G6→Task 5.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

**One primitive, three consumers.** DESIGN §3.5 specifies a single structure — `ChainProviders(T0)` —
from which every read path derives its answer. The architecture of this change is: build the
primitive once (group A), then rewrite each read path to consume it instead of re-deriving the rule.

```text
ChainProviders(T0) = every provider owned by any tenant in [T0, parent(T0), …, root],
                     each tagged { id, owner_tenant, slug, status, winner }

  winner(p)          := p.owner_tenant is the closest chain tenant owning slug p.slug
                        -- ownership ONLY; status deliberately not a factor
  allow_list(T0)     = { p.id : winner(p) AND p.status == active }   -- eval visibility
  shadowed(p)        = NOT winner(p)                                 -- management flag
  provider_disabled  = p.status == disabled                          -- management flag
```

**Key design decisions and rationale:**

1. **`winner` ignores status; `allow_list` ANDs it on top.** This is load-bearing, not incidental. A
   `disabled` shadow must still *win* its slug so the ancestor's models are excluded (they lost), and
   the child's own models are excluded too (the winner is not active). Folding status into `winner`
   hands the slug back to the ancestor and re-exposes exactly the models the shadow exists to hide.

2. **`get_tenant_model` resolves one slug; the listings build the whole map.** The get path carries
   the `<10ms P99` NFR (§1.2), so it resolves the single slug from the `canonical_id` closest-first
   and stops at the first owner, rather than materializing `ChainProviders`. It then probes the cache
   **under the winning tenant only** — the current closest-first probe has no shadow check on the hit
   path, so a cached ancestor model is served for the rest of its TTL with no DB query to catch it.

3. **`ListVisibility` enum on `ModelRepository::list`** (chosen over an options struct or a second
   method): the eval/management split becomes a type, the mandatory predicates stay inside the
   repository where §3.5 sub-decision 4 puts them, and "forgot the allow-list" is not representable.

4. **Provider reads fail closed; model reads tolerate partial results.** Deliberately inverse rules.
   Dropping ancestor *model* rows only narrows what the caller sees; skipping a closer tenant's
   *provider* row **widens** it by silently un-shadowing an ancestor. `merge_inherited_page` gains an
   explicit `AncestorFailure` mode rather than growing a second near-duplicate helper.

5. **The slug cache stores tombstones.** The common chain hop is "this tenant owns nothing under this
   slug". An uncached absence is a DB round-trip per hop on every `get_tenant_model`. Both polarities
   take the ownership TTL of the tenant they are stored under and are dropped by that tenant's
   existing `invalidate_tenant` prefix sweep — no new invalidation code.

6. **`shadowed` / `provider_disabled` stay response-only.** Neither binds to a real `models` column
   and `FieldToColumn` maps one field to exactly one column, so they cannot be OData filter fields
   (§3.3). Management callers narrow client-side.

**Sequencing** follows the dependency order in `impl-gaps.md`: A unblocks B/C/F; D and E are
independent. Within that, C+D (the get path) lands before B (the list path) because it is the
highest-value correctness fix and the one on the latency NFR.

## Technical Details

### New / changed data structures

```rust
// domain/inheritance.rs — the shared primitive (A1)
pub struct ChainProvider {
    pub id: Uuid,
    pub owner_tenant: Uuid,
    pub slug: String,
    pub status: ProviderStatus,
    pub winner: bool,
}

pub struct ChainProviders {
    by_id: HashMap<Uuid, ChainProvider>,
    allow_list: Vec<Uuid>,            // winner AND status == active
}

impl ChainProviders {
    pub fn get(&self, provider_id: Uuid) -> Option<&ChainProvider>;
    pub fn allow_list(&self) -> &[Uuid];
    pub fn allow_slice_for(&self, tenant_id: Uuid) -> Vec<Uuid>;  // per-tenant slice
    pub fn is_allowed(&self, provider_id: Uuid) -> bool;
}

// domain/repo.rs — ChainProviders needs each tenant's COMPLETE provider set.
// `ProviderRepository::list` is paginated (default 20 / max 100); a truncated
// fetch silently corrupts the allow-list, which is the predicate this whole
// change introduces. Unpaginated by construction, not by a large $top.
async fn list_all_for_tenant(
    &self,
    conn: &impl DBRunner,
    scope: &AccessScope,
) -> Result<Vec<ProviderV1>, DomainError>;

// domain/inheritance.rs — explicit ancestor-failure policy (B5)
pub enum AncestorFailure {
    Skip,        // ancestor model queries — partial results narrow, never widen
    FailClosed,  // ancestor provider queries — a skip would un-shadow
}

// domain/inheritance.rs — the ancestor closure must learn WHICH tenant it is
// querying, so the caller can supply that tenant's allow-list slice and skip
// an empty one. `AccessScope` has no tenant accessor, so the id is passed
// explicitly and the closure may return `None` to mean "skip this tenant".
L: Fn(Uuid, AccessScope, ODataQuery) -> Fut,
Fut: Future<Output = Option<Result<Page<T>, DomainError>>>,

// domain/repo.rs — shared query parameterization (A3, F5)
pub enum ListVisibility<'a> {
    Eval { allow_list: &'a [Uuid] },          // ANDs provider_id IN (…) + lifecycle exclusion
    Management { include_deprecated: bool },  // no mandatory predicates
}

// model-registry-sdk/src/models/entity.rs (A2)
pub struct ModelV1<P: gts::GtsSchema = serde_json::Value> {
    pub id: Uuid,
    pub provider_id: Uuid,   // NEW — the visibility key (§3.6)
    pub canonical_id: String,
    // … unchanged
}

// model-registry-sdk/src/models/entity.rs (F3)
pub struct ModelManagementV1<P: gts::GtsSchema = serde_json::Value> {
    pub model: ModelV1<P>,
    pub shadowed: bool,
    pub provider_disabled: bool,
    pub available_for_eval: bool,
}

// domain/error.rs + sdk/errors.rs (E2)
ProviderNotOwned { slug: String, owner_tenant_id: Uuid }   // → 403 permission_denied
```

### Cache

Third entity alongside `provider` (UUID) and `model` (canonical id):

```text
mr:{tenant_id}:provider_slug:{slug}  →  SlugOwnership::Owned(ProviderV1) | SlugOwnership::None
```

Serialized as a two-variant enum so the tombstone is a cache **hit** carrying "no owner", not a miss.
TTL from `cache_ttl_seconds(inheritance.classify(tenant_id), &config)` — the ownership class of the
tenant the entry is stored under, not of the requester. Dropped by that tenant's existing
`invalidate_tenant`, including the provider create that installs a shadow and flips a tombstone.

### Processing flow — `get_tenant_model` (rewritten, C1–C5)

```text
1. access_scope(ctx, model, "get")            -- unchanged
2. resolve_ancestors                           -- unchanged
3. slug := canonical_id.split_once("::")?.0    -- None => ModelNotFound (C5)
4. for T in [T0, parent, …, root]:             -- closest first, stop at first owner
       cache mr:{T}:provider_slug:{slug}
         hit Owned(p)  -> winner = (T, p); break
         hit None      -> continue
         miss          -> provider_repo.find_by_slug(scope=T)
                          err (non-not-found) -> Internal   -- fail closed (B5)
                          found  -> cache Owned; winner = (T, p); break
                          absent -> cache None; continue
   no winner -> ProviderNotFoundBySlug                       (C3)
5. model := cache mr:{winner.tenant}:model:{canonical_id}
            else model_repo.find_by_canonical(scope = winner.tenant ONLY)   (C1)
   none -> ModelNotFound
6. gates, in this order:                                     (C4)
   model.provider_id != winner.id            -> ModelNotFound          (C2)
   lifecycle in {deprecated, sunset}         -> drop key; ModelDeprecated
   winner.status == disabled                 -> drop key; ProviderDisabled
7. cache the row under mr:{winner.tenant}:model:{canonical_id}, TTL by ownership
```

Gate **order** is normative: a disabled provider must never be disclosed through a `canonical_id`
that resolves to nothing. Approval stays **reported, not enforced** — `ModelNotApproved` remains
unreachable from this path in P1.

### Processing flow — the two listings

```text
eval (B1–B3):                          management (F2–F5):
  chain := ChainProviders(T0)            chain := ChainProviders(T0)
  for T in chain order:                  for T in chain order:
    slice := chain.allow_slice_for(T)      rows(T) := list(T, Management {
    if slice.is_empty(): skip T                        include_deprecated })
    rows(T) := list(T, Eval { slice })    merge in chain order, truncate
  merge in chain order, truncate         per row p := chain[row.provider_id]:
                                           shadowed          = !p.winner
                                           provider_disabled = p.status == disabled
                                           available_for_eval = chain.is_allowed(p.id)
                                                                AND !terminal_lifecycle
                                                                AND approved
```

The `canonical_id` dedupe in `merge_inherited_page` is **kept** for the eval path but demoted to a
redundant safety net — the allow-list membership test is the mechanism. The management path must
**not** dedupe by `canonical_id`: two chain tenants owning the same slug is precisely the case it
exists to display.

### API surface

| Change | Where |
|---|---|
| `ModelDto.provider_id: Uuid` (additive) | `api/rest/dto.rs` |
| `ModelManagementDto` = `ModelDto` + 3 bools | `api/rest/dto.rs` |
| `GET /model-registry/v1/admin/models` | `api/rest/routes.rs` |
| `include_deprecated` (bool, default `false`) — plain query param, **not** OData | `api/rest/parse.rs`, handler |
| `actions::LIST_MANAGEMENT = "list_management"` | `domain/service.rs` |
| `ProviderNotOwned` → 403 `permission_denied` | `api/rest/error.rs`, `domain/error.rs`, `sdk/errors.rs` |
| `list_tenant_models_management` (11th trait method) | `sdk/api.rs`, `domain/local_client.rs` |

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): tasks achievable within this codebase — code changes,
  tests, documentation updates
- **Post-Completion** (no checkboxes): items requiring external action — the authz-resolver policy
  entry for the new action, load verification against the latency NFR, and the deferred H/I decisions

## Implementation Steps

### Task 1: Surface `provider_id` on `ModelV1` and both response DTOs (A2, A5)

The column, FK and SeaORM entity field all exist already; the field is dropped on the way out of the
mapper. This is a deliberate, bounded compile break across ten struct-literal sites in five files.

**Files:**
- Modify: `gears/model-registry/model-registry-sdk/src/models/entity.rs`
- Modify: `gears/model-registry/model-registry/src/infra/storage/mapper.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/dto.rs`
- Modify: `gears/model-registry/model-registry/src/infra/storage/mapper_test.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/dto_test.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs` (test fixtures)
- Modify: `gears/llm-gateway/llm-gateway-demo/src/mock_registry.rs` (JSON fixtures)

- [x] add `pub provider_id: Uuid` to `ModelV1<P>` in `entity.rs`, documented as the visibility key (§3.6)
- [x] carry `provider_id` through `ModelV1::try_into_typed` so the typed narrowing preserves it
- [x] project `e.provider_id` in `mapper::model_entity_to_v1` (typed struct-literal projection, per the
      gear's no-`json!`-round-trip rule)
- [x] add `pub provider_id: Uuid` to `ModelDto` and its `From<ModelV1<P>>` impl in `dto.rs`
- [x] fix the six `ModelV1` struct-literal sites and the two `ModelDto` literal sites
- [x] add `"provider_id"` to `OPENAI_FIXTURE` and `ANTHROPIC_FIXTURE` in
      `llm-gateway-demo/src/mock_registry.rs` — these deserialize `ModelV1` from JSON, so a missing
      required field is a **runtime** failure no compiler catches
- [x] write a mapper test asserting `provider_id` round-trips entity → `ModelV1` with the row's value
- [x] write a DTO test asserting `provider_id` appears in the serialized `ModelDto` JSON
- [x] write an SDK test asserting `try_into_typed` preserves `provider_id`
- [x] run `cargo test -p cf-gears-llm-gateway-demo` — `MockModelRegistry::new()` must still succeed
- [x] run tests - must pass before task 2

### Task 2: Add the `(tenant_id, provider_id)` index (A4)

The index the new mandatory predicate needs on every per-tenant list query. B-tree, not GIN — GIN
would break the SQLite dev/test path.

**Files:**
- Modify: `gears/model-registry/model-registry/src/infra/storage/migrations/initial_001.rs`

- [x] add `CREATE INDEX IF NOT EXISTS idx_models_tenant_provider ON models (tenant_id, provider_id);`
      to the models index block
- [x] confirm the statement is emitted for both the SQLite and PostgreSQL rendering of the migration SQL
- [x] write a migration test asserting the index exists after `run_migrations_for_testing` on SQLite
- [x] run tests - must pass before task 3

### Task 3: Build the `ChainProviders` primitive (A1)

Nothing currently computes `winner(p)` / `allow_list(T0)`. This is the structure every subsequent task
consumes.

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/inheritance.rs`
- Modify: `gears/model-registry/model-registry/src/domain/repo.rs`
- Modify: `gears/model-registry/model-registry/src/infra/storage/provider_repo.rs`
- Modify: `gears/model-registry/model-registry/src/domain/local_client.rs` (mock repo)

- [x] add `ProviderRepository::list_all_for_tenant` returning the tenant's **complete** provider set.
      `list` is paginated (`LimitCfg { default: 20, max: 100 }`), and a truncated fetch silently
      corrupts the allow-list — the exact predicate this change introduces
- [x] add `ChainProvider { id, owner_tenant, slug, status, winner }` and `ChainProviders` with
      `get`, `allow_list`, `allow_slice_for`, `is_allowed`
- [x] implement the winner computation over the chain: closest tenant owning a slug wins it,
      **on ownership alone** — status must not be a factor (document why inline: folding status in
      re-exposes the shadowed models)
- [x] implement `allow_list` as `winner AND status == active`
- [x] add a `build_chain_providers` constructor taking the `InheritanceContext` and a per-tenant
      provider-fetch closure, failing closed on any query error (§3.5 sub-decision 1)
- [x] write tests: single tenant, child shadows parent, parent shadows grandparent, unrelated slugs
      coexist
- [x] write a test asserting a **disabled shadow** marks the ancestor a loser *and* is itself absent
      from `allow_list` (the §3.5 "why `winner` ignores status" case)
- [x] write a test asserting `build_chain_providers` returns `Internal` when any tenant's provider
      query errors, rather than skipping that tenant
- [x] write a test asserting `allow_slice_for` returns an empty slice for a tenant whose providers
      all lost or are disabled
- [x] write a repository test with **more providers than the default page size** (>20), asserting
      `list_all_for_tenant` returns them all and the resulting `allow_list` is complete
- [x] run tests - must pass before task 4

### Task 4: Parameterize the repository query by visibility mode (A3, B4, F5)

One query serving both listing paths, with the mandatory predicates inside the repository. Deletes the
lifecycle escape hatch in the same edit — it lives in the function being rewritten.

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/repo.rs`
- Modify: `gears/model-registry/model-registry/src/infra/storage/model_repo.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs` (call site + mock repos)
- Modify: `gears/model-registry/model-registry/src/domain/local_client.rs` (mock repos in tests)

- [x] add `ListVisibility<'a> { Eval { allow_list: &'a [Uuid] }, Management { include_deprecated: bool } }`
      to `domain/repo.rs` and thread it through `ModelRepository::list`
- [x] in `model_repo.rs`, AND `provider_id IN allow_list` and the unconditional lifecycle exclusion
      onto the `Eval` query; apply **no** mandatory predicates on `Management` beyond the
      `include_deprecated=false` terminal-lifecycle drop
- [x] **delete** `filter_references_lifecycle_status` and its call site — the lifecycle exclusion is
      now unconditional on the eval path (B4, §3.3 "Lifecycle exclusion (eval, unconditional)")
- [x] pass `ListVisibility::Management { include_deprecated: false }` at **both** existing
      `list_tenant_models` call sites (own page and the ancestor closure) as the interim value.
      ⚠️ Do **not** pass `Eval` here: no allow-list exists until Task 8 builds `ChainProviders`, and
      the ancestor closure shares the same `list` call, so a caller's own-tenant slice would filter
      out every ancestor row and break four existing inheritance tests. `Management` with
      `include_deprecated: false` is exactly behavior-preserving *plus* B4
- [x] update the mock `ModelRepository` impls in `service.rs` and `local_client.rs` tests
- [x] write repository tests: `Eval` with a non-empty allow-list returns only matching rows; `Eval`
      with an empty allow-list returns an empty page
- [x] write a repository test asserting `$filter=lifecycle_status eq 'deprecated'` on `Eval` returns
      an **empty page** (the escape hatch is gone) and that `Management { include_deprecated: true }`
      returns those same rows
- [x] write a repository test asserting `Management` returns rows whose `provider_id` is outside the
      allow-list
- [x] run tests - must pass before task 5

### Task 5: Fail closed on ancestor provider queries (B5, G6)

Skip-on-error is correct for ancestor *model* queries and wrong for ancestor *provider* queries: with
≥2 ancestors, a failed parent query un-shadows a grandparent provider.

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/inheritance.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`

- [x] add `AncestorFailure { Skip, FailClosed }` and change `merge_inherited_page` to take it and
      return `Result<Page<T>, DomainError>`
- [x] pass `FailClosed` from `list_providers`; pass `Skip` from `list_tenant_models`
- [x] document the asymmetry inline: dropping ancestor model rows narrows, skipping an ancestor
      provider row widens
- [x] add a failing-ancestor `ProviderRepository` mock to `service.rs` tests — `build_service` /
      `build_service_with_cache` (`service.rs:1145-1170`) hard-wire the concrete
      `ProviderRepositoryImpl` and there is no such mock today; `Service` is constructible by struct
      literal from the in-module test mod
- [x] write a unit test asserting `FailClosed` propagates an ancestor error as `Internal` rather than
      returning a partial page
- [x] write a unit test asserting `Skip` still yields partial results and logs, for model queries (G6)
- [x] write a service test asserting `list_providers` fails when an ancestor provider query errors
- [x] run tests - must pass before task 6

### Task 6: Slug-ownership cache entity with tombstones (D1, D2, G5)

Without this, Task 7's per-hop slug resolution costs a DB round-trip per chain hop on the path
carrying the `<10ms P99` NFR.

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/cache.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`

- [ ] add a `SlugOwnership` enum (`Owned(ProviderV1)` / `None`) serializable through `CacheService`,
      so a tombstone is a cache **hit** carrying "no owner" rather than a miss
- [ ] use the existing `cache_key(&tenant_id, "provider_slug", slug)` — it already produces
      `mr:{tenant}:provider_slug:{slug}`; no new key helper is needed
- [ ] add a service helper that resolves one `(tenant, slug)` hop cache-first, writing **both**
      polarities on a DB miss with the TTL of that tenant's ownership class
- [ ] confirm no new invalidation code is needed — the key sits under the owning tenant's prefix and
      is swept by the existing `invalidate_tenant`
- [ ] write cache tests: positive hit, tombstone hit (distinguished from a miss), TTL expiry
- [ ] write a `service.rs` in-module test asserting a tombstone is written on a DB miss and consulted
      on the next call (no second DB query). Keep it at service level: the helper is private and
      `get_tenant_model` does not call it until Task 7, so there is no end-to-end path yet — the
      shadow-install/invalidation assertion (G5) lands in Task 7's integration tests
- [ ] run tests - must pass before task 7

### Task 7: Rewrite `get_tenant_model` around slug resolution (C1–C5, G4-get)

The highest-value correctness fix. Today the cache is probed for every tenant closest-first and returns
on the first hit — a cached ancestor model is served to a subtree that has shadowed its provider, for
the rest of the TTL, with no DB query on that path to catch it.

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`
- Modify: `gears/model-registry/model-registry/tests/integration.rs`

- [ ] split `canonical_id` on the **first** `::`; a malformed id (no `::`) yields `ModelNotFound` (C5)
- [ ] resolve the slug closest-first via the Task 6 helper, stopping at the first owner; **fail closed**
      on any non-not-found query error at any hop
- [ ] return `ProviderNotFoundBySlug` when no tenant in the chain owns the slug (C3)
- [ ] replace the chain-wide cache probe with a single probe under the winner's tenant, and the
      `find_in_chain` DB fallback with a lookup scoped to the winner's tenant only (C1)
- [ ] scope the model read correctly: winner == T0 → the PDP-derived `own_scope` (preserving the
      PDP's compiled constraints); winner is an ancestor → a constructed `AccessScope::for_tenant`.
      Do not use `for_tenant` uniformly — that silently drops the PDP constraints on own-tenant reads
- [ ] apply the gates **in order** (C2, C4): `provider_id != winner.id` → `ModelNotFound`; then
      terminal lifecycle → drop key, `ModelDeprecated`; then `winner.status == disabled` → drop key,
      `ProviderDisabled`
- [ ] keep approval **reported, not enforced** — `ModelNotApproved` stays unreachable from this path
- [ ] ⚠️ update three existing tests whose outcome legitimately changes — a fixture with no provider
      row now yields `ProviderNotFoundBySlug` (still 404 `not_found` on the wire) instead of
      `ModelNotFound`/`ModelDeprecated`: `test_get_tenant_model_not_found` (`service.rs:1320`),
      `test_get_tenant_model_deprecated_in_cache_returns_error` (`:1398` — its cached `ModelV1` also
      needs a `provider_id` matching the winner to reach the lifecycle gate), and
      `test_get_tenant_model_cross_tenant_not_found` (`:2000`). Not listed in `impl-gaps.md` group G
- [ ] write unit tests for each gate and its ordering, including the malformed-`canonical_id` case
- [ ] write a unit test asserting a stale row whose `provider_id` no longer matches the winner yields
      `ModelNotFound`, not the row
- [ ] write an integration test: an ancestor model is cached, the child then shadows the provider slug,
      and the next `get_tenant_model` must **not** serve the cached ancestor row (C1's headline case)
- [ ] write an integration test asserting the provider create that installs a shadow drops the
      tombstone, so the next resolution finds the new owner (G5, deferred from Task 6)
- [ ] write an integration test asserting a disabled winning provider yields `ProviderDisabled` for a
      resolvable id, and `ModelNotFound` for an unresolvable one (G4, gate-ordering disclosure rule)
- [ ] run tests - must pass before task 8

### Task 8: Rewrite `list_tenant_models` around the allow-list (B1–B3, G2, G3, G4-list)

The headline shadowing bug: the merge is keyed on `canonical_id`, so an ancestor model whose provider
slug has been shadowed survives whenever the shadowing tenant has no model of the same name.

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/inheritance.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`
- Modify: `gears/model-registry/model-registry/tests/integration.rs`

- [ ] change `merge_inherited_page`'s ancestor closure to `Fn(Uuid, AccessScope, ODataQuery)` and let
      it return `Option<Result<…>>`, so the caller can look up that tenant's allow-list slice and
      return `None` to skip it. Required: `AccessScope` exposes no tenant accessor, so the slice
      cannot be computed inside today's closure
- [ ] ⚠️ **TDD exception** — before changing `list_tenant_models`, add the G3 fixture to
      `integration.rs` (near `:720`): child shadows the slug and owns **no** colliding model; assert
      the ancestor's model is invisible. Run it and confirm it **fails** against the current code.
      This is the one test whose value depends on observing the red state; the plan is otherwise
      code-first
- [ ] build `ChainProviders(T0)` at the top of `list_tenant_models`, fail-closed
- [ ] query each chain tenant with `ListVisibility::Eval { allow_list: chain.allow_slice_for(T) }`,
      **skipping** any tenant whose slice is empty (B3)
- [ ] handle the own-tenant skip: when T0's slice is empty there is no `own_page`, so synthesize an
      empty `Page` carrying the caller's limit rather than propagating a missing `page_info`
- [ ] keep the `canonical_id` dedupe as a documented redundant safety net, not the mechanism (B1)
- [ ] confirm disabled providers now hide their models via the same predicate, with no separate
      status check on the read path (B2)
- [ ] update `test_list_tenant_models_child_shadows_ancestor` and `..._parent_shadows_grandparent`
      (`service.rs:1744`, `:1807`) to assert through the allow-list rather than passing incidentally
      via `canonical_id` dedupe (G2)
- [ ] confirm the G3 test now passes
- [ ] write an integration test asserting a disabled provider hides its models from the eval listing,
      and that a `disabled` shadow hides both the ancestor's models and its own (G4)
- [ ] write a test asserting a tenant with an empty allow-list slice is skipped entirely rather than
      queried
- [ ] run tests - must pass before task 9

### Task 9: Own-tenant-only `create_model` and `ProviderNotOwned` (E1–E3, G1)

`find_visible_provider` walks own + ancestors and the service then widens the scope to
`for_tenants([caller, provider_owner])` so the FK resolves — the exact cross-tenant model the design
forbids (§3.1 Invariants: `model.tenant_id == provider.tenant_id`).

**Files:**
- Modify: `gears/model-registry/model-registry/src/domain/error.rs`
- Modify: `gears/model-registry/model-registry-sdk/src/errors.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/error.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/error_test.rs`

- [ ] add `ProviderNotOwned { slug }` to `DomainError` with a constructor. Carry the ancestor's
      `owner_tenant_id` in the log/trace only, not in the variant — DESIGN §4's table does not
      require it and a 403 body should not leak another tenant's UUID
- [ ] add the matching `ModelRegistryError::ProviderNotOwned` and the `DomainError` → SDK mapping
- [ ] map it to 403 / `permission_denied` in `api/rest/error.rs`, distinct from `ProviderNotFound`
      (nowhere in the chain) and `Forbidden` (a PDP denial)
- [ ] resolve the provider **own-tenant-only** in `create_model`; drop the
      `AccessScope::for_tenants([caller, provider_owner])` widening entirely
- [ ] return `ProviderNotOwned` when the slug resolves only in an ancestor (E1/E2), and
      `ProviderNotFoundBySlug` (404) — not `Validation` (400) — when it resolves nowhere (E3)
- [ ] remove or narrow `find_visible_provider` to its remaining callers, if any
- [ ] decide the repository's own unresolved-slug error (`model_repo.rs:160-163` raises
      `Validation`): align it with E3's 404 or record in-code that it is deliberately left, being
      unreachable behind the service pre-check (a TOCTOU-only path)
- [ ] invert `test_create_model_with_inherited_provider` (`service.rs:2102`) to expect
      `ProviderNotOwned` (G1)
- [ ] write a test asserting an unknown slug yields `ProviderNotFoundBySlug`, not `Validation`
- [ ] write a REST error-mapping test asserting `ProviderNotOwned` → 403 `permission_denied`
- [ ] write an integration test asserting a model created against an own-tenant provider still succeeds
- [ ] run tests - must pass before task 10

### Task 10: `list_tenant_models_management` on the service, trait and client (F2, F3-SDK, F5)

The eleventh P1 trait method, on the same `ModelRegistryClientV1` trait — not a separate admin client.

**Files:**
- Modify: `gears/model-registry/model-registry-sdk/src/models/entity.rs`
- Modify: `gears/model-registry/model-registry-sdk/src/models/mod.rs`
- Modify: `gears/model-registry/model-registry-sdk/src/api.rs`
- Modify: `gears/model-registry/model-registry/src/domain/service.rs`
- Modify: `gears/model-registry/model-registry/src/domain/local_client.rs`
- Modify: `gears/llm-gateway/llm-gateway-demo/src/mock_registry.rs` (implement the 11th method)

- [ ] add `ModelManagementV1<P>` to the SDK: `ModelV1` plus `shadowed`, `provider_disabled`,
      `available_for_eval`; re-export from `models/mod.rs`
- [ ] add `actions::LIST_MANAGEMENT = "list_management"` and use it for this method's `access_scope`
      call, so the admin grant is a first-class PDP subject
- [ ] implement `Service::list_tenant_models_management`: build `ChainProviders` fail-closed, query
      every chain tenant with `ListVisibility::Management { include_deprecated }`, merge in chain
      order **without** `canonical_id` dedupe, truncate to the page size
- [ ] suppress the dedupe by passing `key_fn = |m| m.id` to `merge_inherited_page` — model ids are
      unique, so `apply_additive_visibility` collapses nothing while chain ordering is preserved.
      Reuses the one helper rather than adding a no-dedupe mode. Two chain tenants owning the same
      slug is exactly what this endpoint exists to display
- [ ] `AncestorFailure::Skip` applies here too — this path keeps §3.4's partial-results rule for
      ancestor *model* queries; only the `ChainProviders` build is fail-closed
- [ ] implement the new trait method on `MockModelRegistry` in `llm-gateway-demo` — the trait has no
      default bodies, so omitting it is a workspace compile error
- [ ] compute the three flags per row from the same `ChainProviders`; include the approval conjunct in
      `available_for_eval` even though eval does not enforce it yet (the flag deliberately leads the
      implementation)
- [ ] add `list_tenant_models_management` to `ModelRegistryClientV1` and implement it on `LocalClient`
- [ ] write a test asserting a shadowed ancestor row comes back with `shadowed = true` and
      `available_for_eval = false`, while the eval listing omits it entirely
- [ ] write a test asserting a disabled provider's rows come back with `provider_disabled = true`
- [ ] write a test asserting a non-approved model on a winning active provider reports
      `available_for_eval = false` while `list_tenant_models` still returns it
- [ ] write a test asserting `include_deprecated` gates terminal-lifecycle rows and defaults to `false`
- [ ] run tests - must pass before task 11

### Task 11: Register `GET /v1/admin/models` (F1, F3-REST, F4, G4-management)

**Files:**
- Modify: `gears/model-registry/model-registry/src/api/rest/dto.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/routes.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/handlers.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/parse.rs`
- Modify: `gears/model-registry/model-registry/src/api/rest/dto_test.rs`
- Modify: `gears/model-registry/model-registry/tests/integration.rs`

- [ ] add `ModelManagementDto` (= `ModelDto` + the three bools) and `ModelManagementListDto` with the
      standard `page_info`
- [ ] add an `include_deprecated` query-parameter extractor (bool, default `false`) in `parse.rs` —
      a plain query parameter, **not** an OData filter side effect, and management-only
- [ ] add the `list_management_models` handler
- [ ] register `GET /model-registry/v1/admin/models` with its own `OperationBuilder` policy:
      `.authenticated()`, license features, the shared `ModelFilterField` OData filter/orderby,
      `.error_400/401/403/422/500`
- [ ] declare `include_deprecated` to OpenAPI via `OperationBuilder::query_param_typed` — the
      `parse.rs` extractor alone leaves it undocumented in the spec `make openapi` regenerates
- [ ] confirm `shadowed` / `provider_disabled` are **not** added to `ModelFilterField` — response-only
      (§3.3)
- [ ] add an **action-aware denying** `AuthZResolverClient` mock — both existing mocks
      (`service.rs:701`, `integration.rs:88`) are unconditionally permissive, and this mock is the
      only thing that can prove the service actually asks for `list_management`
- [ ] write a DTO test asserting the three flags serialize on `ModelManagementDto`
- [ ] keep a regression guard asserting `$filter=shadowed eq true` is rejected as an unknown field
      (it passes today — it proves the field was not added, not that anything new works)
- [ ] write a **service-level** integration test: shadowed and disabled-provider rows present and
      marked via `list_tenant_models_management`, absent from `list_tenant_models` (G4-management).
      ⚠️ Not an HTTP test — this gear has no `axum`/`Router`/`oneshot` harness and
      `tests/integration.rs` drives `Service` directly; adding one is out of scope
- [ ] write a test with the denying mock asserting a caller without the `list_management` grant is
      refused (`DomainError::Forbidden` → 403)
- [ ] run tests - must pass before task 12

### Task 12: Verify acceptance criteria

- [ ] verify all requirements from Overview are implemented — walk `impl-gaps.md` groups A–G and
      confirm each numbered gap is closed
- [ ] verify the §3.5 worked example test was observed failing before Task 8's change (recorded there)
- [ ] verify gate ordering in `get_tenant_model` matches §3.5 exactly (provider mismatch → lifecycle →
      provider status)
- [ ] verify no read path consults `providers.status` outside `ChainProviders`
- [ ] verify `merge_inherited_page` callers pass the correct `AncestorFailure` mode
- [ ] verify no `ListVisibility::Management` remains on the eval path (the Task 4 interim value)
- [ ] run the crate test suite:
      `cargo test -p cf-gears-model-registry -p cf-gears-model-registry-sdk -p cf-gears-llm-gateway-demo`
- [ ] run clippy over the same three crates with `--all-targets -- -D warnings`
- [ ] run `make dylint` (layer separation on the new SDK/domain/REST types)
- [ ] run `make openapi` and confirm the regenerated spec carries the new endpoint, `provider_id` (on
      both the request and response shapes of `ModelDto`, which is `#[api_dto(request, response)]`),
      the `include_deprecated` parameter, and `ModelManagementDto`
- [ ] run `make test-sqlite`

### Task 13: [Final] Update documentation

- [ ] update `gears/model-registry/docs/impl-gaps.md`: mark groups A–G closed, leave H/I as the
      remaining open items
- [ ] update DESIGN §4 Technical Debt — the entries describing the `canonical_id` merge bug and the
      lifecycle escape hatch as shipped debt are no longer accurate
- [ ] update the DESIGN §3.3 "implementation scope" note and the endpoint table row for
      `GET /v1/admin/models` — it is now implemented, not "P1 (design)"
- [ ] update DESIGN §4 Authorization: the action list reads `get, list, create, update, delete` and
      must now include `list_management`
- [ ] run `cfs validate` on the edited DESIGN.md (part of `make check`)
- [ ] run `make gts-docs` if any GTS reference changed
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion
*Items requiring manual intervention or external systems - no checkboxes, informational only*

**External system updates:**

- **`authz-resolver` policy entry for `list_management`.** Task 10 introduces a sixth action on the
  `model_registry.model` resource. The gear only *asks* the PDP; the grant itself is configured in
  `authz-resolver` per DESIGN §4 Authorization: `platform-admin` over own + descendants,
  `tenant-admin` over own tenant + inherited, `llm-gateway-svc` **refused**. Until that entry exists,
  the endpoint denies every caller — fail-closed, which is the correct interim posture, but it means
  the endpoint is non-functional in any environment whose policy set has not been updated.
- **Consumers of `ModelV1` and `ModelRegistryClientV1` outside this workspace.** The added
  `provider_id` is a compile break for struct-literal constructors and a **runtime** deserialization
  break for anything building `ModelV1` from stored JSON; the 11th trait method is a compile break
  for any external implementor. Both are handled for the in-workspace consumer
  (`gears/llm-gateway/llm-gateway-demo/`) in Tasks 1 and 10, but out-of-workspace consumers need the
  same two changes. Wire-additive for read-only JSON consumers.

**Manual verification:**

- **Latency against the `<10ms P99` NFR.** `get_tenant_model` gains per-hop slug resolution, and both
  listings gain a `ChainProviders` build (one provider query per ancestor). The slug cache with
  tombstones is designed to keep the common path off the DB, but H3/H7 note no benchmark exists behind
  the NFR. Measure before/after on a chain of realistic depth.
- **Shadow propagation window.** A new shadow still does not invalidate descendants (H4) — up to 5
  minutes of inherited TTL during which a subtree's compliance-isolation lever is not biting. Unchanged
  by this work, but more visible now that shadowing actually functions.

**Deferred — group H (acknowledged debt, none blocking):**

- H1 `ModelRegistryConfig::max_page_size` parsed but unused; both repos hard-code `LimitCfg { 20, 100 }`
- H2 ancestor merge is not a stable paginated order (cursor anchors on own-tenant rows)
- H3 ancestor fan-out is one query per ancestor, plus one provider query per ancestor after this change
- H4 a new shadow does not invalidate descendants
- H5 `mapper.rs` / `odata_mapper.rs` still mix provider and model concerns
- H6 layering deviation: `domain/repo.rs` takes `&impl DBRunner`, `DomainError` wraps `toolkit_db::DbError`
- H7 no benchmark behind the `<10ms P99` NFR

**Deferred — group I (open questions):**

- I1 `get_provider(id)` returns an ancestor provider even when a closer tenant shadows its slug. §3.5
  governs model visibility and slug resolution only, and the management surface needs those rows — is
  the current behavior intended, or should the eval-facing get mark/refuse a shadowed provider?
- I2 §3.3 lists `managed` among the 15 filterable model fields but §3.6's index list omits it. Add
  `idx_models_managed`, or record the omission as deliberate. (Explicitly excluded from Task 2.)
