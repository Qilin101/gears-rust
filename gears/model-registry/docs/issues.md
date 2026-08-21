# Model Registry: PRD ↔ DESIGN discrepancy status

Open items only. Every item below was verified against both the docs and the shipped code at
`7e3468fe8`. Resolved items have been removed from this list; the original numbering is preserved,
so gaps in the sequence are removed-resolved items and references from elsewhere stay valid.

## Medium — gaps

28. [ ] **`nfr-performance`: the discovery target has no end-to-end allocation.** Mostly resolved.
    - *Closed*: the NFR summary column now carries all four PRD budgets, `approve_model` has a
      design response (PDP decision + one indexed UPDATE + prefix invalidation, no Approval Service
      call on the P1 write path), and §4 Capacity repeats the read, list and approve figures.
    - Deferred by decision: the 30s per-provider discovery target stays allocated to the **OAGW
      call** only. The end-to-end call (OAGW call + reconciliation writes) has no separate figure;
      DESIGN states that allocation explicitly rather than implying the 30s covers both.
30. [ ] **Four open questions routed to DESIGN are still unanswered.** Partially resolved.
    - *Closed*: OQ#6 is now marked Resolved in the PRD with the backoff/re-trigger decision, and
      DESIGN §5 Traceability now enumerates every open question and where it is (or is not)
      answered — including OQ#4, which §4 Security Considerations already tracked.
    - Deferred by decision: OQ#1 (approval concurrency — no version column, optimistic locking or
      lost-update discussion anywhere), OQ#2 (per-endpoint QPS — absent), OQ#3 (provider plugin
      retry policies — near-duplicate of the now-resolved OQ#6; may just need folding into it),
      OQ#5 (discovery-settings GTS namespace — DESIGN leaves the chain plugin-declared, which
      sidesteps the namespace question the PRD asks).
