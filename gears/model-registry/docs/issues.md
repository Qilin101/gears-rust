# Model Registry: PRD ↔ DESIGN discrepancy status

This re-audits the original findings (authored against commit `0aae98a6`) against the current
docs, after `ed178320` (update prd) and `60f9abe4` (update design). **No code changed in either
commit** — everything below is a documentation-only comparison; `[x]` items are resolved
*on paper* and still need an implementation pass.

1. [x] **Per-tenant approval of an inherited model** — no longer a modeling gap. Both docs now
   commit to a single rule: a model's `tenant_id` MUST equal its provider's `tenant_id`
   (DESIGN.md:183, :535; PRD.md:194, :253). A child tenant's only lever over an ancestor's models
   is shadowing the whole provider (which now hides every model attached to it), not per-model
   override. Needs code: same-tenant `provider_slug` resolution + `provider_not_owned` on
   `create_model`; shadow-hides-all-models exclusion in `get_tenant_model`/`list_tenant_models`.
2. [x] **UC-009 / fr-model-pricing had no design response** — resolved. PRD dropped the
   sync/batch/cached tier + media-rate cost model (PRD.md:560-565) in favor of "shape follows the
   provider's own cost structure," matching DESIGN's `OpenAiCost`/`AnthropicCost`. UC-009 (PRD.md
   ~1180) now matches DESIGN: no separate `get_provider_cost` endpoint, cost travels with model
   info, consumer narrows by `gts_type`. *(See item 9 — one stale line remains.)*
3. [x] **`base_url` dropped from Provider silently** — resolved. PRD.md:226-227 now states
   explicitly there is no generic `base_url`; connection/routing settings are provider-type-specific
   and live in GTS-typed `provider_settings`, matching DESIGN.
4. [x] **A disabled provider's models stayed fully resolvable** — resolved at the design level.
   New `models.provider_disabled` shadow column (DESIGN.md:1226-1239) with a defined sync strategy
   and default-exclusion rule; UC-007 AC rewritten (PRD.md ~1140-1160) to require
   `get_tenant_model`/`list_tenant_models` to return `provider_disabled` for any model on a disabled
   provider. Also fixes the `provider_disabled` 404-vs-403 half of item 8 — PRD's error table now
   says 403 (PRD.md:940), matching DESIGN. Needs code: the column, its sync-on-status-change write,
   the index, and the read-path check.
5. [x] **Create-then-disable shadowing race** — resolved. Shadowing now hides the parent's models
   regardless of the shadow's own `status` (DESIGN.md:183; PRD.md:254-255, UC-006 AC), so the
   create(active)-then-PATCH(disabled) gap no longer exposes the parent's models.
6. [x] **PRD names two interfaces (`ModelRegistryClient`/`AdminClient`), DESIGN ships one** —
   resolved. PRD §12 (PRD.md ~975-978) now describes a single `ModelRegistryClient` with two method
   groups (eval-facing vs management) differentiated by authorization, matching DESIGN.
7. [ ] **Approval is never enforced, but the driver was checked off** — partially resolved. The
   dishonest checkbox is fixed: `fr-get-tenant-model`, `fr-list-tenant-models`,
   `fr-manual-model-management`, and `fr-provider-management` are now `[ ]` (DESIGN.md:62-65), not
   `[x]`. **The underlying contradiction is untouched**: DESIGN.md:831 and :1359 still say
   `approval_status` is "returned on the model, not enforced" and that `ModelNotApproved` "is never
   produced by this read," while PRD.md:1014 (UC-001 AC) still requires `Returns model_not_approved
   (403) if not approved` and PRD.md:959 still lists "Approval status always verified from DB (P1 &
   P2 fail-closed)" as a security control. This is the same P1 access-control gap as before — only
   its bookkeeping changed. Still needs a decision: PRD relaxes the fail-closed AC to P2, or DESIGN
   grows an enforcement path.
8. [ ] **Error-mapping divergences** — partially resolved. One of four sub-items is now fixed
   (`ProviderDisabled` is 403 in both docs; the new `ProviderNotOwned` is added consistently in
   both). Still open:
   - `ModelDeprecated` is 404 in DESIGN (DESIGN.md:1358) vs 410 in PRD's UC-001 AC (PRD.md:1015).
   - Duplicate `canonical_id` is `Validation`/400 in DESIGN (DESIGN.md:1363) vs 409 implied by PRD's
     general `already_exists` framing.
   - `InvalidTransition` is 400/`invalid_argument` in DESIGN (error table, DESIGN.md ~1352) vs
     `invalid_transition` 409 in PRD's error table.
   - 503 `discovery_failed` and the "surface 503 to caller" promise (DESIGN.md:1488) appear in no
     error table.
   - `ProviderHasModels` → `already_exists` (DESIGN.md:1136, :1350) is still semantically wrong —
     nothing already exists; it's a precondition conflict (should read as a 409 conflict of a
     different category, or `invalid_argument`).
9. [ ] **Stale `fr-model-pricing` driver line — new drift introduced by the update.** DESIGN.md:67
   still reads "AICredits cost data **per tier (sync/batch/cached)**," the exact model the update
   just retired in favor of provider-specific cost shapes (see item 2 above, and DESIGN.md's own
   `OpenAiCost`/`AnthropicCost` §3.1). One-line wording fix.

## Critical

10. [ ] **Approval state machine is not enforced, but `fr-manual-model-management` is checked
    off.** Unchanged. DESIGN.md:531: "Terminal lifecycle states are read-only... Every other
    lifecycle transition — including demotion — is permitted." PRD.md:521 and UC-020 AC (PRD.md
    ~1475) still require transitions to follow the state machine diagram. `service.rs`'s
    `validate_lifecycle_transition` still only guards terminal states (code unchanged).

## High — internal contradictions in the design

11. [ ] **"TTL by ownership" is not well-defined.** Unchanged. DESIGN.md:822 still writes an
    inherited row under `mr:{ancestor}:model:{id}` — the key has no reader component, so the same
    key gets 30-minute TTL when the owner reads it and 5-minute TTL when a descendant does; last
    writer wins (DESIGN.md:169, :739-740).
12. [ ] **Same key scheme falsifies the descendant-invalidation claim.** Unchanged. DESIGN.md:890,
    :1373 (item 5), and the capacity/consistency notes still assert descendants are not invalidated
    on a parent write and converge only via TTL — but inherited entries live under the writer's own
    prefix, so `invalidate_tenant(parent)` does drop them.
13. [ ] **`mr:{tenant}:models:*` doesn't exist.** Unchanged. DESIGN.md:171 states there are exactly
    two cache entities and "list responses are not cached" — yet DESIGN.md:886, :1013, :1044
    invalidate `mr:{tenant}:models:*`, and §4 items reference "model-list keys" (DESIGN.md:1373,
    :1375).
14. [ ] **"B-tree index on every filterable column" is false.** Unchanged, and now has one more
    exception. `models.managed` is still unindexed (DESIGN.md:1223 lists thirteen indexes for the
    fourteen non-identity shadow columns). On `providers`, only PK + `(tenant_id, slug)` exist
    (DESIGN.md:1134) — `name`, `status`, `gts_type`, `managed`, `discovery_enabled` remain
    unindexed despite the driver claim at DESIGN.md:88.
15. [ ] **Plugin selection keys off the wrong GTS chain.** Unchanged. DESIGN.md:910:
    `serves_gts_type` is the *provider* GTS type, but the same row says "Plugin selection is exact
    match on the provider's `info.gts_type`" — `info.gts_type` is `ModelInfoV1.gts_type`, a
    different chain. The match can never succeed as specified.
16. [ ] **Wrong provider GTS namespace.** Unchanged. DESIGN.md:910 and :1126 still use the plural
    `gts.cf.genai.models.provider.v1~`; PRD (PRD.md:238 and elsewhere) and
    `guidelines/GTS.md:145,855` register the singular `gts.cf.genai.model.provider.v1~`. Code uses
    the plural too (unverified against this pass, per original citation).
17. [ ] **`fr-degraded-mode` is mis-cited; DB-unavailability is undesigned.** Unchanged.
    DESIGN.md:1056-1095 ("Discovery Failure (Degraded-Mode Path)") attributes
    provider-unreachability to `fr-degraded-mode`, but that FR (DESIGN.md:77, PRD's DB-down
    section) is about database unavailability with `service_unavailable`. No 503 row exists in any
    error table, and the read path already serves cache hits with the DB down
    (contradicting PRD.md:883/959 "DB unavailable = requests fail (fail-closed)").

## High — carried over

18. [ ] **Discovery sequence diagram is logically broken.** Unchanged. DESIGN.md:866: `alt auto
    discovert plugin found` (typo), and the `alt` still has no `else` — the flow still falls into
    the reconciliation loop even when no plugin was found, contradicting the reject-before-invocation
    rule stated later (DESIGN.md ~907).
19. [ ] **P2 discovery upsert has no supporting constraint, plus a stray rule.** Unchanged.
    DESIGN.md:1427 still keys the upsert on `(provider_id, provider_model_id)` with no declared
    unique index on that pair (only `(tenant_id, canonical_id)` exists, DESIGN.md:1223), and still
    asserts a newly discovered model inserts with `lifecycle_status = preview`, which appears
    nowhere else — the reconciliation diagram (DESIGN.md ~878) only sets `approval_status = pending`.

## Medium — gaps

20. [ ] **UC-017 job/status mismatch.** Unchanged. PRD.md:1358 still requires the discovery
    trigger to queue a job and return `queued`/`running`/`completed`; DESIGN.md:887 still returns
    `discovery_result` synchronously with no job entity or status endpoint.
21. [ ] **Discovery concurrency/staggering NFR is unverifiable.** Unchanged. PRD.md:593, :1066
    require a "fixed concurrency limit + staggered intervals"; the only trigger remains
    single-provider `POST /providers/{id}/discover` (DESIGN.md:892), with isolation argued from
    per-call independence rather than any concurrency construct.
22. [ ] **Distributed lock service is load-bearing but undeclared.** Unchanged. Referenced at
    DESIGN.md:956, :1434, :1451, :1488, but absent from §3.3/§3.4 (DESIGN.md:764-772) and from the
    gear's dependency list.
23. [ ] **`$filter` by provider is missing.** Unchanged. The 15-field filter surface
    (DESIGN.md:697) still has no `provider_slug`/`provider_id`, and `models` carries no slug
    column, despite PRD.md:1036 requiring `$filter` by provider slug.
24. [ ] **Capabilities/limits domain divergence.** Unchanged. PRD Tier-1 (PRD.md:277) still lists
    `video` I/O among boolean capabilities; PRD Tier-2 (PRD.md:278) still lists
    `max_images_per_request`, `max_image_size_mb`, `max_audio_duration_sec`. DESIGN's
    `ModelCapabilities`/`ContextWindow` (DESIGN.md:341, :391-394) still have no `video` flag and no
    mapping table reconciling the limit fields against `output_vector_size`.
25. [ ] **`fr-input-validation` marked implemented with two rules missing.** Unchanged.
    PRD.md:431-432 still requires provider **name** 1-32 chars lowercase-alnum-hyphen and
    capabilities conforming to a GTS capability schema; code validates slug, discovery interval,
    and lifecycle transition only (unchanged). DESIGN's `name VARCHAR(255)` "Display name" row
    (DESIGN.md ~1121) still contradicts the PRD's name-format rule — one of the two is still wrong
    and neither doc says so.
26. [ ] **`fr-cache-isolation` is checked off P1 while its reparenting handler is P3.**
    Unchanged. DESIGN.md:61 is still `[x]` for `fr-cache-isolation`; the
    `tenant.reparented` handler is still P3 (DESIGN.md:78, :1374) even though PRD's cache-isolation
    FR includes reparenting invalidation as P1.
27. [ ] **`nfr-rate-limiting` is P1 in the PRD, deferred wholesale in DESIGN.** Unchanged.
    PRD.md:920 keeps it P1 with concrete limits; DESIGN.md:89 still says "Not implemented in P1,"
    with no PRD amendment. The `approve_model` 100ms and discovery 30s targets (PRD.md:870-871)
    still get no design response (DESIGN's only timeout figures, DESIGN.md:1450/:1462-1463, are for
    different calls: `get_status`/OAGW discovery).
28. [ ] **Capacity basis is 10× under the PRD envelope, and the PRD contradicts itself.**
    Unchanged. DESIGN.md:1434 still plans "10,000 tenants × 200 models = 2 million rows," treating
    ~2,000 models/tenant as an outlier. PRD's own scale table (PRD.md ~895-899) states 20 providers
    × 100 models/provider × 10,000 tenants, which is 20M, yet the same table's "Total models (worst
    case)" cell still says "~2,000,000" — a 10× internal PRD inconsistency that DESIGN then adopts
    as its planning basis without flagging the discrepancy.
29. [ ] **Open questions routed to DESIGN remain unanswered/unacknowledged.** Unchanged. PRD §18
    (PRD.md:1636 onward) still lists OQ#1 (approval concurrency — DESIGN has no version column or
    optimistic locking), OQ#2 (per-endpoint QPS — absent), OQ#3 (plugin retry policy — only generic
    dependency retries), OQ#5 (discovery-settings GTS namespace — still "plugin-declared,"
    DESIGN.md ~936), OQ#6 (retry/backoff — partially answered at DESIGN.md:1413 area but not marked
    resolved). DESIGN's §5 Traceability still never references the open questions.
30. [ ] **Tag-filter scope stated two ways.** Unchanged. DESIGN.md:700 says tag filtering is
    "scoped to the request tenant," while DESIGN.md:1049 and :1054 describe a
    `JOIN model_tags (tenant chain)` / "scoped to the tenant chain." Whether inherited tag
    assignments are visible is still undecided.
31. [ ] **Audit (PRD §7 is a MUST) — DESIGN P1 has no audit sink.** Unchanged. DESIGN.md:1407:
    "no audit-sink integration exists in this module yet... structured `tracing` output" only.
    PRD.md:1620 (assumption 7) provides the reconciling assumption, but PRD's own audit-fields table
    (PRD.md:836) and the "Core platform" ownership note (PRD.md:207) are still presented normatively
    without cross-referencing that assumption from the FR itself.
32. [ ] **`ProviderV1` has no field list in §3.1.** Unchanged. DESIGN §3.1 Domain Model
    (DESIGN.md:257 onward) still only enumerates `ModelInfoV1`'s ~150 lines of fields; there is no
    equivalent `#### Provider` field list to check PRD §5's Provider fields (PRD.md:222-243)
    against — only the DDL table in §3.6 (DESIGN.md:1114-1131) has the actual columns.

## Minor

33. [ ] DESIGN.md:263 "The six enums and `ModelRegistryError` remain `#[non_exhaustive]`" still
    reads as if six is the SDK's total enum count rather than the `#[non_exhaustive]` subset.
34. [ ] `aliases.canonical_id VARCHAR(512)` (DESIGN.md:1273) vs `models.canonical_id VARCHAR(255)`
    (DESIGN.md:1147) for the same identifier — still unreconciled.
35. [ ] Tags unique index `(tenant_id, lower(name))` (DESIGN.md:1294 area) is a functional index —
    still not portable to the MySQL support claimed at DESIGN.md:750 (needs 8.0.13+); no
    per-backend dispatch note added.
36. [ ] `model_tags.model_id` FK `ON DELETE CASCADE` "so hard-deleting a model removes its
    assignments" (DESIGN.md:1314) — still no hard-delete path for models in any phase.
37. [ ] Cache participant still named `Redis` in three sequence diagrams (DESIGN.md:860, :1034,
    :1075) while §2.1/§3.2 (DESIGN.md:728) insist P1 opens no Redis connection; the other diagrams
    use `CacheService` (DESIGN.md:796, :982, :1006).
38. [ ] `BOOLEAN … DEFAULT 0` on `models` (DESIGN.md:1206, :1212-1215) vs `DEFAULT false` on
    `providers` (DESIGN.md:1125, :1127) and on the new `models.provider_disabled` (DESIGN.md:1233)
    — same rendering, still inconsistent style within the same table.
39. [ ] `get_ancestors` (DESIGN.md:802, :1460) vs `get_ancestor_chain` (DESIGN.md:1450) — still two
    names for what reads as the same `tenant-resolver` call.
40. [ ] The PATCH wholesale-replacement list (DESIGN.md:671) still omits `allow_extra_params` and
    `additional_info`, both stored sub-objects — merge-vs-replace for those two remains unstated.
41. [ ] Dangling forward references: `DECOMPOSITION.md` "once it is generated" (DESIGN.md:1469),
    `features/` "to be created" (DESIGN.md:1550).

## Net-new implementation backlog (not a doc bug — flagging so it isn't lost)

The reconciliation added real P1 scope that has **no code at all** yet, beyond what's noted inline
above:

- [ ] `provider_not_owned` error variant + same-tenant `provider_slug` resolution in `create_model`.
- [ ] Shadow-hides-all-models exclusion logic in `get_tenant_model` / `list_tenant_models`, reusing
  the existing closest-tenant-wins provider resolution.
- [ ] `models.provider_disabled` shadow column, its sync-on-status-change write, its index, and the
  read-path check/default-exclusion.
- [ ] The entire eleventh endpoint/method, `list_tenant_models_management`
  (`cpt-cf-model-registry-fr-list-tenant-models-management`, UC-027) — SDK trait method, REST
  route, service method, and authorization gating to tenant-admin/platform-admin.
