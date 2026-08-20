# Model Registry: PRD ↔ DESIGN discrepancy status

Third pass. The original findings were authored against commit `0aae98a6`; the second pass re-read
the docs only. **This pass verifies every item against both the docs and the shipped code at
`7e3468fe8`** — the P1 implementation landed in between (`ChainProviders` visibility resolution,
`provider_not_owned`, the management listing endpoint, the unconditional eval predicates), so
several items that were "resolved on paper, needs an implementation pass" are now resolved in code,
and the whole net-new implementation backlog is closed.

`[x]` = verified resolved (or the premise is no longer true). `[ ]` = still open, with the citation
refreshed to current line numbers and the sub-items that *did* close called out.

1. [x] **Per-tenant approval of an inherited model** — resolved in docs **and code**. Both docs
   commit to `model.tenant_id == provider.tenant_id` (DESIGN.md:180, :527-537; PRD.md:99, :511).
   `create_model` resolves `provider_slug` own-tenant-only and returns `ProviderNotOwned` when the
   slug resolves solely in an ancestor (`service.rs:806-841`); shadow-hides-all-models falls out of
   `allow_list` membership (`inheritance.rs:141-196`, `service.rs:604-668`).
2. [x] **UC-009 / fr-model-pricing had no design response** — resolved. Cost shape follows the
   provider's own structure (`OpenAiCost`/`AnthropicCost`), PRD.md:557-571 ↔ DESIGN.md:67.
3. [x] **`base_url` dropped from Provider silently** — resolved. PRD.md:230, :549 state explicitly
   that there is no generic `base_url`; connection settings are GTS-typed and provider-specific.
4. [x] **A disabled provider's models stayed fully resolvable** — resolved, and the mechanism
   changed for the better. The `models.provider_disabled` shadow column of the previous pass was
   **dropped**: DESIGN.md:539 now states `providers.status` is the only source of truth and is *not*
   denormalized onto `models`. One predicate (`provider_id ∈ allow_list`, where `allow_list` is
   `winner AND status == active`) carries both the shadow gate and the disabled gate
   (DESIGN.md:838-848). Implemented: `ChainProviders` (`inheritance.rs:76-196`), eval predicate
   (`model_repo.rs:87-95`), `get_tenant_model` gate 3 (`service.rs:565-569`).
5. [x] **Create-then-disable shadowing race** — resolved in docs and code. `winner` turns on
   ownership alone and status is ANDed on separately (DESIGN.md:840), so a `disabled` shadow still
   wins its slug and the parent's models stay hidden — asserted at `inheritance.rs:1626-1631`.
6. [x] **PRD names two interfaces (`ModelRegistryClient`/`AdminClient`), DESIGN ships one** —
   resolved. PRD.md:956 describes one client with two method groups split by authorization.
7. [x] **Approval is now enforced on the eval read paths.** Resolved 2026-08-20 in DESIGN, ADR-0002
   and code. The PRD needed no change — UC-001 already required `model_not_approved` (403)
   (PRD.md:996), UC-002 already required "Returns only approved models, unconditionally"
   (PRD.md:1019), and PRD.md:452 / :941 already stated the fail-closed contract. What landed:
   - **DESIGN** replaced every "returned on the model, not enforced" statement with the gate: a
     third eval mandatory predicate `approval_status = 'approved'` (DESIGN.md:708, :734, pseudocode
     :896), a fourth ordered gate on `get_tenant_model` returning `ModelNotApproved` (403) after the
     provider and lifecycle gates (DESIGN.md:870, :924, :1001-1003), the reasons it cannot fail open
     (a column already on the row, no Approval Service call on the read path in **any** phase —
     DESIGN.md:185, :538, :1669, :1679), and `available_for_eval` as the exact conjunction of the
     three predicates instead of a flag that "leads the implementation" (DESIGN.md:914). ADR-0002's
     claim that the module "queries approval status from Approval Service when resolving models"
     was corrected to match.
   - **Code**: `approval_status = 'approved'` ANDed into the `ListVisibility::Eval` predicate set
     (`model_repo.rs:87-99`); gate 4 in `get_tenant_model`, which keeps the cache entry rather than
     evicting it (`service.rs:571-580`); OpenAPI descriptions for both eval endpoints state the
     contract (`routes.rs`). Both endpoints already registered `403`.
   - **Tests**: `pending` / `rejected` / `revoked` each refused by `get_tenant_model`; the eval
     listing excludes them and `$filter=approval_status eq '<non-approved>'` returns an empty page
     rather than re-admitting them (`model_repo.rs` `model_list_eval_hides_non_approved_even_with_filter`,
     `service.rs` `test_get_tenant_model_pending_returns_not_approved` /
     `..._rejected_and_revoked_are_not_approved`); the management listing still returns them with
     `available_for_eval = false`; `full_lifecycle_single_tenant` now walks
     create → refused → approve → resolves. Fixtures on eval-path tests were switched to approved
     models so they assert their own subject rather than passing through the approval gate by
     accident (19 call sites in `service.rs`, 12 in `integration.rs`, 1 in `model_repo.rs`).
   Verified: 296 unit + 24 integration + 36 SDK tests pass; `cargo fmt` / `clippy --all-targets`
   clean for both crates.
8. [ ] **Error-mapping divergences** — one more sub-item closed, four still open. Code matches
   DESIGN exactly (`api/rest/error.rs`), so every remaining row is a PRD-vs-DESIGN divergence.
   - *Closed*: **duplicate `canonical_id`**. DESIGN.md:1577 documents the `Validation`/400 rationale
     and the PRD's general `already_exists` framing is gone — the only `*_already_exists` row left
     is `tag_already_exists` (PRD.md:922), which is P3 and genuinely a collision.
   - `ModelDeprecated` is 404 in DESIGN (DESIGN.md:1558, rationale at :1572) vs 410 in PRD's error
     table (PRD.md:919) and UC-001 AC (PRD.md:997).
   - `InvalidTransition` is 400/`invalid_argument` in DESIGN (DESIGN.md:1565) vs
     `invalid_transition`
     409 in PRD's table (PRD.md:925).
   - 503 `discovery_failed` appears only in a sequence diagram (DESIGN.md:1305) and in no error
     table; the PRD's only 503 is `service_unavailable` for DB-down (PRD.md:928).
   - `ProviderHasModels` → `already_exists` (DESIGN.md:1564) is still semantically wrong — nothing
     already exists; it is a precondition conflict.
   - **New**: PRD.md:923 says `provider_disabled` is "also returned by `get_tenant_model`/
     `list_tenant_models`", but DESIGN.md:1575 is explicit that `list_tenant_models` has no such
     error and silently drops those rows via the allow-list predicate — which is what the code does.
9. [x] **Stale `fr-model-pricing` driver line** — resolved (DESIGN.md:67).

## Critical

10. [ ] **The approval state machine is not enforced, but `fr-manual-model-management` is checked
    off.** Unchanged; citation corrected — this is about *approval* transitions, not the lifecycle
    ones. PRD.md:297-306 defines the approval state machine (`pending → approved|rejected`,
    `approved → revoked`, `rejected|revoked → approved`), PRD.md:520 says transitions "follow the
    approval state machine and are enforced by Model Registry domain logic", and UC-020's AC
    repeats it (PRD.md:1457). DESIGN.md:196 and :1234 say the opposite — "no workflow state
    machine … the only guard is that approval cannot be changed on a model in a terminal lifecycle
    state" — and that is exactly what the code does: `update_model` validates the *lifecycle*
    transition (`service.rs:385-395`, terminal-only) plus the terminal-lifecycle approval freeze
    (`service.rs:889-896`), and accepts any `approval_status` value otherwise.
    DESIGN.md:64 keeps `fr-manual-model-management` `[x]`.

## High — internal contradictions in the design

11. [x] **"TTL by ownership" is not well-defined.** Resolved — one `cache_ttl_seconds` for every
    entry (DESIGN.md:168, :769); no "TTL by ownership" phrasing remains in either doc.
12. [x] **Same key scheme falsifies the descendant-invalidation claim.** Resolved. DESIGN now
    states the correct consequence in all three places: DESIGN.md:1584 ("No subtree walk is needed
    to reach descendants: keys are prefixed by the owning tenant, so descendants read the very
    entries this drops"), DESIGN.md:1109 for the discovery path, and DESIGN.md:1642/:1676 for the
    consistency model — where the residual window is correctly attributed to cross-replica caches
    rather than to descendants.
13. [x] **`mr:{tenant}:models:*` doesn't exist.** Resolved — no reference to the phantom entity
    remains; §2.1 and §4 record why list responses are uncached.
14. [ ] **"B-tree index on every filterable column" is false.** Unchanged, now verified against the
    migration. DESIGN.md:88 still makes the claim. `models` has thirteen single-column indexes
    (DESIGN.md:1442 = `initial_001.rs:131-143`) for **fifteen** filter fields
    (`odata/models.rs`): `canonical_id` rides the `(tenant_id, canonical_id)` unique key, but
    `managed` has no index at all. `providers` has only PK + `(tenant_id, slug)`
    (DESIGN.md:1353 = `initial_001.rs:60-61`) for **six** filter fields — `name`, `status`,
    `gts_type`, `managed`, `discovery_enabled` are all unindexed.
15. [ ] **Plugin selection keys off the wrong GTS chain.** Unchanged. DESIGN.md:1129:
    `serves_gts_type` is the *provider* GTS type, but the same row says "Plugin selection is exact
    match on the provider's `info.gts_type`" — `info.gts_type` is `ModelInfoV1.gts_type`, a
    different chain (`gts.cf.genai.model.info.v1~…`). The match can never succeed as specified.
16. [ ] **Wrong provider GTS namespace** — unchanged, and now confirmed against code. DESIGN.md:1129
    and :1323 plus DEMO.md:56 use the plural `gts.cf.genai.models.provider.v1~`, and so does the
    code everywhere (`entity/provider.rs:21`, `initial_001.rs:190`, `request.rs:286` and the test
    fixtures). PRD.md:146, :225, :410 and `guidelines/GTS.md:146` register the singular
    `gts.cf.genai.model.provider.v1~`. The model-info chain is singular in both
    (`gts.cf.genai.model.info.v1~`), so the plural is an outlier, not a convention.
17. [ ] **`fr-degraded-mode` is mis-cited; DB-unavailability is undesigned.** Unchanged.
    DESIGN.md:1314 attributes provider-unreachability to `cpt-cf-model-registry-fr-degraded-mode`,
    but that FR is about *database* unavailability (DESIGN.md:77; PRD.md:789-798). No 503 row exists
    in any error table (see item 8), and the read path can still answer from cache without touching
    the DB (`service.rs:461-540` probes the cache on every hop), contradicting PRD.md:798/:878
    "DB unavailable = requests fail (fail-closed)".

## High — carried over

18. [ ] **Discovery sequence diagram is logically broken.** Unchanged. DESIGN.md:1085:
    `alt auto discovert plugin found` (typo), and the `alt` still has no `else` — the flow falls
    into the reconciliation loop even when no plugin was found, contradicting the
    reject-before-invocation rule at DESIGN.md:1132.
19. [ ] **P2 discovery upsert has no supporting constraint, plus a stray rule.** Unchanged.
    DESIGN.md:1644 keys the upsert on `(provider_id, provider_model_id)` with no declared unique
    index on that pair (only `(tenant_id, canonical_id)` exists, DESIGN.md:1442 =
    `initial_001.rs:127`), and still asserts a newly discovered model inserts with
    `lifecycle_status = preview`, which appears nowhere else — the reconciliation diagram
    (DESIGN.md:1096-1102) only sets `approval_status = pending`.

## Medium — gaps

20. [ ] **UC-017 job/status mismatch.** Unchanged. PRD.md:1334-1341 still has the registry "queue a
    discovery job" and return `queued`/`running`/`completed`; DESIGN.md:1106 returns
    `discovery_result` synchronously, with no job entity and no status endpoint.
21. [ ] **Discovery concurrency/staggering NFR is unverifiable.** Unchanged. PRD.md:590 and :1049
    require a "fixed concurrency limit + staggered intervals"; DESIGN.md:1113 has one
    single-provider trigger and argues isolation from per-call independence plus the per-provider
    lock — no concurrency construct, and no staggering anywhere.
22. [ ] **Distributed lock service is load-bearing but undeclared.** Unchanged. Referenced at
    DESIGN.md:1113, :1175, :1651, :1668, :1699, but absent from §3.3 External Interfaces
    (DESIGN.md:755-792, which lists only Cache, PostgreSQL, Provider APIs), from §3.4 Internal
    Dependencies (DESIGN.md:793), and from the gear's declared deps (DESIGN.md:753).
23. [ ] **`$filter` by provider is missing.** Unchanged. PRD.md:1017 requires `$filter` by provider
    slug; the 15-field surface (DESIGN.md:727 = `odata/models.rs`) has no `provider_slug` /
    `provider_id`, and `models` carries no slug column. Note the same DESIGN section rules out the
    obvious fixes for the *flag* fields (DESIGN.md:717) but never addresses provider identity —
    `provider_id` is a real column and would bind cleanly.
24. [ ] **Capabilities/limits domain divergence.** Unchanged. PRD Tier-1 (PRD.md:276) still lists
    `video` I/O among the boolean capabilities and PRD Tier-2 (PRD.md:277) still lists
    `max_images_per_request`, `max_image_size_mb`, `max_audio_duration_sec`. DESIGN's
    `ModelCapabilities` (DESIGN.md:343-371) has no `video` capability, and `ContextWindow`
    (DESIGN.md:393-396) is `max_input_tokens` / `max_output_tokens` / `output_vector_size` only —
    with no mapping table reconciling the three per-media limits against it.
25. [ ] **`fr-input-validation` marked implemented with two rules missing.** Unchanged.
    PRD.md:430-431 require provider **name** 1-32 chars lowercase-alnum-hyphen and capabilities
    conforming to a GTS capability schema. `create_provider` validates slug and discovery interval
    only (`service.rs:274-276`) — there is no name check and no capability-schema check anywhere.
    DESIGN.md:60 is `[x]` (its description is at least honest about what it covers), and
    DESIGN.md:1341 `name VARCHAR(255)` "Display name" still contradicts the PRD's name-format rule.
26. [ ] **`fr-cache-isolation` is checked off P1 while its reparenting handler is P3.** Unchanged.
    PRD.md:444 puts "invalidate ALL cache entries on `tenant.reparented`" inside the P1
    cache-isolation FR; DESIGN.md:61 marks the FR `[x]` covering only key format, TTL and
    prefix invalidation, and the reparenting handler stays P3 (DESIGN.md:78, :1589).
27. [x] **`nfr-rate-limiting` was P1 in the PRD, deferred wholesale in DESIGN** — resolved by
    removing the requirement; no `nfr-rate-limiting` id remains in either doc, and DESIGN.md:1747
    records the exclusion.
28. [ ] **`nfr-performance` is allocated for two of its four operations.** Partially resolved.
    PRD.md:866-870 sets targets for `get_tenant_model` (2ms/10ms), `list_tenant_models` (10ms/50ms),
    `approve_model` (—/100ms) and the per-provider discovery job (—/30s).
    - *Closed*: `list_tenant_models` now has a design response — DESIGN.md:86 states list reads are
      uncached and meet their target "through indexed columns and the per-ancestor query path".
    - The NFR row's summary column still reads "get_tenant_model <10ms P99" only, and §4 Capacity
      (DESIGN.md:1651) still repeats just that figure.
    - `approve_model` still gets no design response. (The `approval-service.get_status <100ms`
      budget at DESIGN.md:1657 is a P2 *dependency* budget for a different call.)
    - The 30s discovery target is still answered only indirectly: DESIGN.md:1667/:1658 set 30s on
      the **OAGW call**, not on the end-to-end job (OAGW call + reconciliation writes).
29. [ ] **Capacity basis is 10× under the PRD envelope, and the PRD contradicts itself.**
    Unchanged. DESIGN.md:1651 plans "10 000 tenants × 200 models = 2 million rows". PRD's scale
    table (PRD.md:888-893) states 20 providers × 100 models/provider × 10 000 tenants = 20M, yet the
    same table's "Total models (worst case)" cell says "~2,000,000" — a 10× internal PRD
    inconsistency that DESIGN adopts as its planning basis without flagging.
30. [ ] **Open questions routed to DESIGN remain unanswered/unacknowledged.** Mostly unchanged.
    - *Closed*: OQ#4 (tag access rights) is now explicitly tracked in DESIGN.md:1618.
    - OQ#1 (approval concurrency) — DESIGN still has no version column, optimistic locking or
      lost-update discussion; OQ#2 (per-endpoint QPS) — absent; OQ#3 (plugin retry policy) — only
      the generic dependency retries at DESIGN.md:1666; OQ#5 (discovery-settings GTS namespace) —
      still "plugin-declared" (DESIGN.md:1130); OQ#6 (retry/backoff) — answered in substance at
      DESIGN.md:1666 but never marked resolved in PRD.md:1619-1631.
    - DESIGN §5 Traceability (DESIGN.md:1752-1766) still references no open question.
31. [ ] **Tag-filter scope stated two ways.** Unchanged. DESIGN.md:730 says the `tag` predicate is
    "scoped to the request tenant"; DESIGN.md:1268 and :1273 say `JOIN model_tags (tenant chain)` /
    "scoped to the tenant chain". Whether inherited tag assignments are visible is still undecided.
32. [ ] **Audit (PRD §7 is a MUST) — DESIGN P1 has no audit sink.** Unchanged. DESIGN.md:1624: "no
    audit-sink integration exists in this module yet", structured `tracing` only. PRD.md:829-855
    still presents the audit-fields table normatively, and PRD.md:206 assigns storage/retention to
    "Core platform", without either one referencing the reconciling assumption at PRD.md:1603.
33. [ ] **`ProviderV1` has no field list in §3.1.** Unchanged. §3.1 (DESIGN.md:259-526) enumerates
    `ModelInfoV1`'s fields at length; Provider appears only as a one-line row in the Core Entities
    table. There is still no `#### Provider` field list to check PRD §5's Provider fields
    (PRD.md:221-242) against — only the DDL at DESIGN.md:1332-1349.

## Minor

34. [ ] DESIGN.md:265 "The six enums and `ModelRegistryError` remain `#[non_exhaustive]`". The count
    is right — exactly six SDK enums besides `ModelRegistryError` carry the attribute
    (`ApprovalStatus`, `LifecycleStatus`, `ProviderStatus`, `ReasoningEffort`, `ServiceTier`,
    `SupportedApi`) — but the SDK has 26 enums in total, so "the six enums" still reads as the
    whole set rather than the `#[non_exhaustive]` subset.
35. [ ] `aliases.canonical_id VARCHAR(512)` (DESIGN.md:1487) vs `models.canonical_id VARCHAR(255)`
    (DESIGN.md:1366) for the same identifier — still unreconciled.
36. [ ] Tags unique index `(tenant_id, lower(name))` (DESIGN.md:1508) is a functional index — still
    not portable to the MySQL support claimed at DESIGN.md:779 (needs 8.0.13+); no per-backend
    dispatch note added.
37. [x] `model_tags.model_id` FK `ON DELETE CASCADE` "so hard-deleting a model removes its
    assignments" (DESIGN.md:1528). Premise was wrong: a hard-delete path for models *is* designed —
    the tenant-deletion cascade at DESIGN.md:1636 hard-deletes `providers`, `models`,
    `provider_health`, `aliases`, `tags` and `model_tags` for the affected tenant (P2). The FK
    behavior has a real trigger; no doc change needed.
38. [x] Cache participant named `Redis` in three sequence diagrams — resolved; all five diagrams now
    use `participant Cache as CacheService`, and the Redis framing is gone from §3.3 and ADR-0001.
39. [ ] `BOOLEAN … DEFAULT 0` on `models` (DESIGN.md:1425, :1431-1434) vs `DEFAULT false` on
    `providers` (DESIGN.md:1344, :1346) — same rendering, still inconsistent style. The migration
    emits `DEFAULT 0` for both (`initial_001.rs`), so `providers` is the wrong one.
40. [ ] `get_ancestors` (DESIGN.md:968, :1028, :1677) vs `get_ancestor_chain` (DESIGN.md:1667) —
    still two names for the same call. The code says `get_ancestors`
    (`inheritance.rs:557`), so :1645 is the wrong one.
41. [ ] The PATCH wholesale-replacement list (DESIGN.md:678) still omits both stored sub-objects,
    and the code shows they behave differently: `allow_extra_params` **is** replaced wholesale
    (`UpdateModelRequestV1.allow_extra_params: Option<Vec<String>>`, `model_mapper.rs:405`), while
    `additional_info` is **not patchable at all** — it has no field on `UpdateModelRequestV1`. The
    doc states neither.
42. [ ] Dangling forward references: `DECOMPOSITION.md` "once it is generated" (DESIGN.md:1686,
    :1705, :1744), `features/` "to be created" (DESIGN.md:1762). Neither exists under `docs/`.

43. [x] **DESIGN claimed a cache eviction the code deliberately does not do** — found while
    specifying #7's gate, fixed in the same pass. DESIGN said `get_tenant_model` "deletes that key
    on its way to returning `ModelDeprecated`" and marked it *(implemented)*; the code keeps the
    entry on that gate with a comment explaining why (a terminal state cannot transition out, so
    re-reading can only reproduce the same error — `service.rs:553-562`), and evicts on the two
    gates whose verdict comes from outside the cached row instead. DESIGN now states that rule
    per-gate (a table at §3.5 "Tenant Visibility Resolution", plus DESIGN.md:1596), which is also
    what justifies the new approval gate keeping its entry.
44. [x] **Stale "Designed, not yet implemented"** on the shared eval/management list query
    (DESIGN §3.2) — the `ListVisibility::{Eval, Management}` parameterization ships
    (`model_repo.rs:73-106`). Sentence removed.

## Net-new implementation backlog — closed

Everything the reconciliation added as P1 scope has since landed, or was superseded by a better
design decision:

- [x] `provider_not_owned` error variant + same-tenant `provider_slug` resolution in `create_model`
  — `service.rs:806-841`, `domain/error.rs`, `api/rest/error.rs:48-54`.
- [x] Shadow-hides-all-models exclusion in `get_tenant_model` / `list_tenant_models` — implemented
  as `ChainProviders` + `provider_id ∈ allow_list` (`inheritance.rs:76-196`, `service.rs:487-500`,
  `service.rs:604-668`, `model_repo.rs:87-95`).
- [x] ~~`models.provider_disabled` shadow column, its sync-on-status-change write, its index and the
  read-path check~~ — **superseded**. The column was dropped from the design: `providers.status` is
  the single source of truth and is not denormalized (DESIGN.md:537), the disabled gate rides the
  same `allow_list` predicate as shadowing, and there is no sync path to get wrong.
- [x] The eleventh endpoint/method, `list_tenant_models_management` — SDK trait
  (`model-registry-sdk/src/api.rs:82`), service (`service.rs:682`), `LocalClient`
  (`local_client.rs:69`), route `GET /model-registry/v1/admin/models` (`routes.rs:246`) returning
  `ModelManagementDto` with `shadowed` / `provider_disabled` / `available_for_eval`,
  `include_deprecated` as an explicit query flag, and one repository query parameterized by
  `ListVisibility::{Eval, Management}` (`model_repo.rs:73-106`), gated on the distinct
  `list_management` PDP action (`service.rs:88`).
- [x] Lifecycle-filter escape hatch removed. `model_repo.rs:87-95` ANDs the
  `deprecated`/`sunset` exclusion unconditionally on the eval path — the string-matching branch is
  gone, and `model_repo.rs:1215-1280` covers `$filter=lifecycle_status eq 'deprecated'` returning
  an empty page.
- [x] ~~`provider_disabled` joins the OData filter enum (15 → 16 fields)~~ — **superseded**.
  DESIGN.md:715 rules it out with a reason: `shadowed` and `provider_disabled` are per-request
  computations over `ChainProviders`, not columns, and `FieldToColumn` binds each filter field to
  exactly one real `models` column. Management callers narrow on the flags client-side.
