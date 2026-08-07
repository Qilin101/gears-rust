<!-- Created: 2026-08-07 by Constructor Tech -->

# Model Registry — implementation gaps vs DESIGN

Audit of `gears/model-registry/` against [`DESIGN.md`](./DESIGN.md) as of `c42e30c8`.
Scope: what the **code** must change. Doc-level PRD↔DESIGN drift is tracked separately in
[`issues.md`](./issues.md) and is out of scope here.

Everything below traces back to one design change the code predates: **shadowing is keyed on the
provider slug, and model visibility is `model.provider_id ∈ allow_list`** (§3.5 "Tenant Visibility
Resolution"). The shipped code has no notion of a winning provider on any model read path.

Groups are ordered by dependency: **A** unblocks **B/C/F**; **D** and **E** are independent.

---

## A. Shared primitive — `ChainProviders` (blocks B, C, F)

| # | Gap | DESIGN | Code today |
|---|-----|--------|------------|
| A1 | No `ChainProviders` structure. Nothing computes `winner(p)` / `allow_list(T0)`. `inheritance.rs` offers only a generic closest-key-wins merge (`apply_additive_visibility`), which is a *result-set dedupe*, not a provider-resolution primitive. | §3.5 "The shared primitive" — every chain provider tagged `{id, owner_tenant, slug, status, winner}`; `allow_list = winner AND status == active`; `winner` on **ownership alone** (folding status in re-exposes the shadowed models) | `domain/inheritance.rs:165-197` |
| A2 | `ModelV1` has no `provider_id`. The column, FK and SeaORM entity field all exist; the field is dropped on the way out of the mapper, so no service-layer code can apply the visibility rule. | §3.5 "Consequence for the SDK"; §3.6 `models.provider_id` is "the **visibility key**", surfaced on `ModelV1` and on both response DTOs | `model-registry-sdk/src/models/entity.rs:23-33` (missing); `infra/storage/entity/model.rs:30` and `migrations/initial_001.rs:66,128` (present) |
| A3 | `ModelRepository::list` takes no allow-list and has no visibility mode. There is no way to push `provider_id IN (…)` into the query. | §3.5 sub-decision 4 (repository-level mandatory predicate, not an OData field); §3.2 — *one* query parameterized by `Eval \| Management` | `domain/repo.rs`, `infra/storage/model_repo.rs:60-100` |
| A4 | `(tenant_id, provider_id)` index missing. It is the index the new mandatory predicate needs on every per-tenant list query. | §3.6 models **Indexes** | `migrations/initial_001.rs:127-143` — 13 single-column indexes + the two composite uniques, no `(tenant_id, provider_id)` |
| A5 | `ModelDto` must carry `provider_id` once A2 lands. | §3.6 "Surfaced on `ModelV1` and on both response DTOs" | `api/rest/dto.rs:151-157` |

Note on A2/A5: the SDK entity structs are deliberately not `#[non_exhaustive]`, so adding the field
is an intended compile break across mapper, DTO conversions and every test fixture.

## B. `list_tenant_models` (eval listing)

| # | Gap | DESIGN | Code today |
|---|-----|--------|------------|
| B1 | **Merge is keyed on `canonical_id`, not `provider_id ∈ allow_list`.** An ancestor model whose provider slug has been shadowed survives the merge whenever the shadowing tenant has no model of the same name — the ordinary case for a fresh shadow. This is the headline shadowing bug. | §3.5 worked example ("why `canonical_id` is the wrong key"), §1.2 `fr-list-tenant-models`, §4 Technical Debt | `domain/service.rs:490-499` (`\|m\| m.canonical_id.clone()`) |
| B2 | Disabled providers do not hide their models. No read path consults `providers.status`. | §2.1 "Disabling a provider makes every model attached to it unavailable for eval"; §3.3 mandatory predicates | no provider-status check anywhere in `list_tenant_models` |
| B3 | Per-tenant query must AND `provider_id IN slice` and **skip a tenant whose slice is empty**; the merge dedupe (if kept) becomes a redundant safety net, not the mechanism. | §3.5 per-path pseudocode for `list_tenant_models` | `domain/service.rs:486-499` |
| B4 | **Lifecycle escape hatch still shipped.** When the normalized filter text mentions `lifecycle_status`, the `deprecated`/`sunset` exclusion is dropped — a narrowing-looking `$filter` widens visibility, via string matching over the normalized AST. | §3.3 "Lifecycle exclusion (eval, unconditional)" — supersedes the old rule; §4 Technical Debt says delete the branch | `infra/storage/model_repo.rs:42-91` |
| B5 | Ancestor **provider** queries must fail closed; today the merge helper logs-and-skips every ancestor failure and `list_providers` uses it. With ≥2 ancestors a failed parent query un-shadows a grandparent provider — a widening failure mode. Skip-on-error stays correct for ancestor *model* queries only. | §3.4 "Failure behavior in P1"; §3.5 sub-decision 1 | `domain/inheritance.rs:301-312` used by `domain/service.rs:242-251` (providers) and `:490-499` (models) |

## C. `get_tenant_model`

| # | Gap | DESIGN | Code today |
|---|-----|--------|------------|
| C1 | **No slug resolution at all.** The canonical ID is never split; the model cache is probed for every tenant closest-first and returns on the first hit, and the DB fallback walks the chain the same way. A cached ancestor model is served to a subtree that has shadowed its provider, for the rest of the TTL, with no DB query on that path to catch it. | §2.1 "Cache-First Reads" (the difference is load-bearing), §3.5 sub-decision 2, §3.5 pseudocode | `domain/service.rs:411-445` |
| C2 | No `provider_id == winner.id` check after the row is read → stale rows under a superseded provider resolve. | §3.5 pseudocode (`ModelNotFound`) | — |
| C3 | `ProviderNotFoundBySlug` is never produced by this read path (the variant exists and is wired to 404, but nothing raises it here). | §3.5 gate table | `domain/error.rs:25`, `api/rest/error.rs` |
| C4 | No `ProviderDisabled` gate on read, and no gate **ordering**. Order matters: `provider_id` mismatch → lifecycle → provider status, so a disabled provider is never disclosed through a `canonical_id` that resolves to nothing. | §3.5 "The gates run after the row is found"; §4 Error Handling ("`get_tenant_model` must also return `ProviderDisabled`, not only `create_model`") | `domain/service.rs:447-460` |
| C5 | Malformed `canonical_id` (no `::`) has no defined outcome; must be `ModelNotFound`. Split on the **first** `::`. | §3.5 pseudocode | — |

Keep intact: approval is **reported, not enforced** (§3.5, §4) — `ModelNotApproved` stays unreachable
from this path in P1. And the existing drop-stale-key-on-`ModelDeprecated` behavior is correct.

## D. Slug-ownership cache entity

| # | Gap | DESIGN | Code today |
|---|-----|--------|------------|
| D1 | Third cache entity `mr:{tenant_id}:provider_slug:{slug}` does not exist. Without it, C1's per-hop slug resolution costs a DB round-trip per chain hop on the path carrying the `<10ms P99` NFR. | §2.1 (three cache entities), §3.5 sub-decision 3, §4 Cache Invalidation item 3 | `domain/cache.rs:21` + call sites — only `provider` (UUID) and `model` (canonical id) |
| D2 | Must cache **negative** results (tombstone: "this tenant owns no provider under this slug") — the common hop is a miss. TTL by ownership of the tenant it is stored under; dropped by that tenant's existing `invalidate_tenant` prefix sweep (no new invalidation code needed). | §3.5 sub-decision 3 | — |

## E. Model creation ownership

| # | Gap | DESIGN | Code today |
|---|-----|--------|------------|
| E1 | **`create_model` accepts an ancestor-owned provider.** `find_visible_provider` walks own + ancestors and the service then widens the scope to `for_tenants([caller, provider_owner])` so the FK resolves — the exact cross-tenant model this design forbids. Resolution must be own-tenant-only. | §2.1 "Model creation is scoped to the caller's own tenant's providers only"; §3.1 Invariants (`model.tenant_id == provider.tenant_id`); §1.2 `fr-manual-model-management` | `domain/service.rs:508-547`, `:640-659` |
| E2 | `ProviderNotOwned` error does not exist — not in `DomainError`, not in `ModelRegistryError`, not in the REST mapping (403 / `permission_denied`). Distinct from `ProviderNotFound` (nowhere in the chain) and `Forbidden` (a PDP denial). | §4 Error Handling table + note | `domain/error.rs`, `model-registry-sdk/src/errors.rs`, `api/rest/error.rs` |
| E3 | A slug that resolves nowhere in the chain returns `Validation` (400); the error table says `ProviderNotFound` / `ProviderNotFoundBySlug` (404). | §4 Error Handling table | `domain/service.rs:654-658` |

## F. Management listing — `GET /model-registry/v1/admin/models` (feature absent)

| # | Gap | DESIGN |
|---|-----|--------|
| F1 | Route not registered (`routes.rs` registers exactly the ten P1 operations). Needs its own `OperationBuilder` policy + admin grant; `llm-gateway-svc` refused. | §3.3 endpoint table, §4 Authorization ("the one read operation that does not follow the Read row") |
| F2 | `list_tenant_models_management` missing on `Service` and on the `ModelRegistryClientV1` trait + `LocalClient` (it is the eleventh P1 trait method, on the same trait — not a separate admin client). | §3.2 `component-service`, `component-local-client` |
| F3 | `ModelManagementV1` (SDK) / `ModelManagementDto` (REST) missing: `ModelDto` + `shadowed`, `provider_disabled`, `available_for_eval`, all three computed from the same `ChainProviders`. `available_for_eval` includes the approval conjunct even though eval does not enforce it yet — the flag deliberately leads the implementation. | §3.3 "Two listing endpoints", §3.5 per-path pseudocode |
| F4 | `include_deprecated` (bool, default `false`) — a plain query parameter, **not** an OData filter side effect, and management-only. | §3.3 narrowing invariant |
| F5 | Must share one repository query with the eval path parameterized by visibility mode (A3) — the OData field enum, `FieldToColumn` binding, cursor encoding and ancestor merge are shared, not duplicated. Management applies **no** mandatory predicates. | §3.2, §3.5 |

`shadowed` / `provider_disabled` must stay **response-only** — not OData filter fields (§3.3): neither
binds to a real `models` column, and `FieldToColumn` maps one field to exactly one column.

## G. Tests that encode the old behavior (must change with the fix)

| # | Test | Change |
|---|------|--------|
| G1 | `domain/service.rs:2102` `test_create_model_with_inherited_provider` | Invert: expect `ProviderNotOwned` (E1/E2) |
| G2 | `domain/service.rs:1744` `test_list_tenant_models_child_shadows_ancestor`, `:1807` `..._parent_shadows_grandparent` | Currently pass via `canonical_id` dedupe; re-assert through `allow_list` |
| G3 | `tests/integration.rs:720` `child_shadows_parent_by_same_canonical_id` | Add the §3.5 worked example: child shadows the slug and owns **no** colliding model — ancestor's model must still be invisible (the case the current fixture cannot catch) |
| G4 | — | New: disabled provider hides its models on eval get + list, while management marks them; a `disabled` shadow hides both the ancestor's models and its own (§3.5 "why `winner` ignores status") |
| G5 | — | New: slug-cache tombstone hit/miss + invalidation on the create that installs a shadow (D1/D2) |
| G6 | — | New: fail-closed on an ancestor provider query error (B5), vs partial results tolerated on an ancestor model query |

## H. Design-acknowledged debt — decide in/out of this pass

Listed in §4 Technical Debt; none block the shadowing fix.

- **H1** `ModelRegistryConfig::max_page_size` parsed but unused — both repos hard-code `LimitCfg { 20, 100 }`. Thread it through or drop the key.
- **H2** Ancestor merge is not a stable paginated order (cursor anchors on own-tenant rows). Needs a UNION query or a merge-aware cursor.
- **H3** Ancestor fan-out is one query per ancestor; the new `ChainProviders` build adds one provider query per ancestor on top. Worth measuring before/after against the `<10ms P99` NFR.
- **H4** A new shadow does not invalidate descendants — up to 5 minutes (inherited TTL) of a compliance-isolation lever not biting. Options: targeted subtree invalidation on provider create, or a shorter TTL on the slug entity specifically.
- **H5** `mapper.rs` / `odata_mapper.rs` still mix provider and model concerns (repositories are already split per trait).
- **H6** Layering deviation: `domain/repo.rs` takes `&impl DBRunner`, `DomainError` wraps `toolkit_db::DbError` (`DE0301` allowed with a TODO).
- **H7** No benchmark behind the `<10ms P99` NFR.

## I. Open questions

- **I1** `get_provider(id)` probes the chain by UUID and returns an ancestor provider even when a closer tenant shadows its slug. §3.5 governs model visibility and slug resolution only, and the management surface needs those rows — is the current behavior intended, or should the eval-facing get mark/refuse a shadowed provider?
- **I2** §3.3 lists `managed` among the 15 filterable model fields, but the §3.6 index list has thirteen and omits it. Add `idx_models_managed`, or record the omission as deliberate.

---

### Suggested sequencing

1. **A** — `provider_id` on `ModelV1`/DTOs, `ChainProviders` in `inheritance.rs`, repository visibility mode + `provider_id IN (…)`, the `(tenant_id, provider_id)` index.
2. **C + D** — rewrite `get_tenant_model` around slug resolution with the slug cache entity (highest-value correctness fix, on the NFR path).
3. **B** — eval listing predicate, plus the B4 escape-hatch deletion and B5 fail-closed provider reads (both independent, can land first as small isolated commits).
4. **E** — own-tenant-only `create_model` + `ProviderNotOwned`.
5. **F** — management listing on top of the now-shared query.
6. **G** throughout; **H/I** as a follow-up decision.
