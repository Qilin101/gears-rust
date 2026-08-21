# Model Registry: PRD ↔ DESIGN discrepancy status

Open items only. Every item below was verified against both the docs and the shipped code at
`7e3468fe8`. Resolved items have been removed from this list; the original numbering is preserved,
so gaps in the sequence are removed-resolved items and references from elsewhere stay valid.

## Medium — gaps

20. [ ] **UC-017 job/status mismatch.** Unchanged. PRD.md:1353-1360 still has the registry "queue a
    discovery job" and return `queued`/`running`/`completed`; DESIGN.md:1106 returns
    `discovery_result` synchronously, with no job entity and no status endpoint.
21. [ ] **Discovery concurrency/staggering NFR is unverifiable.** Unchanged. PRD.md:609 and :1068
    require a "fixed concurrency limit + staggered intervals"; DESIGN.md:1113 has one
    single-provider trigger and argues isolation from per-call independence plus the per-provider
    lock — no concurrency construct, and no staggering anywhere.
22. [ ] **Distributed lock service is load-bearing but undeclared.** Unchanged. Referenced at
    DESIGN.md:1113, :1175, :1651, :1668, :1699, but absent from §3.3 External Interfaces
    (DESIGN.md:755-792, which lists only Cache, PostgreSQL, Provider APIs), from §3.4 Internal
    Dependencies (DESIGN.md:793), and from the gear's declared deps (DESIGN.md:753).
23. [ ] **`$filter` by provider is missing.** Unchanged. PRD.md:1036 requires `$filter` by provider
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
    PRD.md:442-443 require provider **name** 1-32 chars lowercase-alnum-hyphen and capabilities
    conforming to a GTS capability schema. `create_provider` validates slug and discovery interval
    only (`service.rs:274-276`) — there is no name check and no capability-schema check anywhere.
    DESIGN.md:60 is `[x]` (its description is at least honest about what it covers), and
    DESIGN.md:1341 `name VARCHAR(255)` "Display name" still contradicts the PRD's name-format rule.
26. [ ] **`fr-cache-isolation` is checked off P1 while its reparenting handler is P3.** Unchanged.
    PRD.md:456 puts "invalidate ALL cache entries on `tenant.reparented`" inside the P1
    cache-isolation FR; DESIGN.md:61 marks the FR `[x]` covering only key format, TTL and
    prefix invalidation, and the reparenting handler stays P3 (DESIGN.md:78, :1589).
28. [ ] **`nfr-performance` is allocated for two of its four operations.** Partially resolved.
    PRD.md:885-889 sets targets for `get_tenant_model` (2ms/10ms), `list_tenant_models` (10ms/50ms),
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
    table (PRD.md:907-912) states 20 providers × 100 models/provider × 10 000 tenants = 20M, yet the
    same table's "Total models (worst case)" cell says "~2,000,000" — a 10× internal PRD
    inconsistency that DESIGN adopts as its planning basis without flagging.
30. [ ] **Open questions routed to DESIGN remain unanswered/unacknowledged.** Mostly unchanged.
    - *Closed*: OQ#4 (tag access rights) is now explicitly tracked in DESIGN.md:1618.
    - OQ#1 (approval concurrency) — DESIGN still has no version column, optimistic locking or
      lost-update discussion; OQ#2 (per-endpoint QPS) — absent; OQ#3 (plugin retry policy) — only
      the generic dependency retries at DESIGN.md:1666; OQ#5 (discovery-settings GTS namespace) —
      still "plugin-declared" (DESIGN.md:1130); OQ#6 (retry/backoff) — answered in substance at
      DESIGN.md:1666 but never marked resolved in PRD.md:1641-1653.
    - DESIGN §5 Traceability (DESIGN.md:1752-1766) still references no open question.
31. [ ] **Tag-filter scope stated two ways.** Unchanged. DESIGN.md:730 says the `tag` predicate is
    "scoped to the request tenant"; DESIGN.md:1268 and :1273 say `JOIN model_tags (tenant chain)` /
    "scoped to the tenant chain". Whether inherited tag assignments are visible is still undecided.
32. [ ] **Audit (PRD §7 is a MUST) — DESIGN P1 has no audit sink.** Unchanged. DESIGN.md:1624: "no
    audit-sink integration exists in this module yet", structured `tracing` only. PRD.md:848-874
    still presents the audit-fields table normatively, and PRD.md:206 assigns storage/retention to
    "Core platform", without either one referencing the reconciling assumption at PRD.md:1625.
33. [ ] **`ProviderV1` has no field list in §3.1.** Unchanged. §3.1 (DESIGN.md:259-526) enumerates
    `ModelInfoV1`'s fields at length; Provider appears only as a one-line row in the Core Entities
    table. There is still no `#### Provider` field list to check PRD §5's Provider fields
    (PRD.md:221-242) against — only the DDL at DESIGN.md:1332-1349.
