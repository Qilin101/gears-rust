---
status: accepted
date: 2026-09-02
---

<!-- Created: 2026-09-02 by Constructor Tech -->

# Owner-Declared Share Mode for Inherited Providers and Models


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Owner-declared share mode on both providers and models](#owner-declared-share-mode-on-both-providers-and-models)
  - [Provider-level sharing only](#provider-level-sharing-only)
  - [Consumer-side per-model exclusion list](#consumer-side-per-model-exclusion-list)
  - [Push sharing into the PDP](#push-sharing-into-the-pdp)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-model-registry-adr-share-mode`

## Context and Problem Statement

[ADR-0004](./0004-cpt-cf-model-registry-adr-tenant-inheritance.md) gives the platform exactly one
visibility lever, and it points upward: a **child** hides an inherited provider by registering a
colliding slug. Everything an ancestor owns is otherwise inherited automatically, in full.

An owner has no say. A parent tenant cannot keep a provider — or a single model — out of its
descendants' catalogs, so compliance isolation can only be driven from below, by every affected
descendant. How should an owner declare how far down the hierarchy each provider and model reaches?

## Decision Drivers

* `cpt-cf-model-registry-fr-share-mode` — owner-declared sharing scope
* `cpt-cf-model-registry-fr-tenant-isolation` — tenant-scoped operations
* PRD requirement: "Child tenant can only restrict, not expand parent permissions" — a new lever must
  not become a way to widen
* Compliance isolation initiated by the owner, not only by the consumer
* ADR-0004's acknowledged gap: dropping one inherited model requires shadowing its whole provider
* The eval reads carry a `<10ms P99` NFR, so a new gate must add no query and no join

## Considered Options

* Owner-declared share mode on both providers and models
* Provider-level sharing only
* Consumer-side per-model exclusion list
* Push sharing into the PDP

## Decision Outcome

Chosen option: "Owner-declared share mode on both providers and models".

A `share_mode` enum — `private`, `children`, `subtree` — is a NOT NULL column on both `providers` and
`models`, defaulting to `subtree`. The default is what makes this additive: a deployment that never
sets the field behaves exactly as before.

**Scope: the eval reads.** Like shadowing, `share_mode` governs `GET /v1/models` and
`GET /v1/models/{canonical_id}` only. The admin endpoints are PDP-scoped and walk no hierarchy, so an
owner always sees everything it owns.

**Direction of control.** The two levers are complementary and point in opposite directions: the owner
decides what it offers, the consumer decides what it accepts. Both only ever remove rows, so
"restrict only" is preserved.

**The depth rule.** For a requester at distance `d` from a row's owner (`0` is the owner, `1` a direct
child): `private` reaches `d == 0`, `children` reaches `d <= 1`, `subtree` reaches any `d`.

**Composition is narrowest-wins.** A model's effective reach is
`min(provider.share_mode, model.share_mode)` — a `private` provider hides all of its models whatever
they declare. No minimum is computed anywhere: a model's tenant always equals its provider's, so both
sit at the same distance, and reach is monotone in the mode ordering, making `min(a, b).reaches(d)`
identically `a.reaches(d) AND b.reaches(d)`. That equivalence is what lets the provider stage run in
Rust (where the chain is already materialized) and the model stage compile to a plain predicate over
real columns — no extra query, no join.

**A private provider is transparent, not a barrier.** A provider that does not reach the requester is
dropped *before* slug-winner ranking, so it neither appears nor shadows: a farther ancestor owning the
same slug wins instead. "Private" means "does not exist below me". The blocking behaviour is already
available and unchanged — it is what a `disabled` provider does — and giving `private` the same
meaning would have been a second spelling of one mechanism. Distance `0` is reached by every mode, so
a private provider still wins its slug for its own tenant.

### Consequences

* Good, because an owner can withhold a provider or a single model without any descendant's
  cooperation, which is what compliance isolation driven from above requires
* Good, because ADR-0004's "drop one inherited model" gap closes without touching the shadow lever
* Good, because the `subtree` default makes the change behaviour-preserving
* Good, because the two levers cannot conflict: each only removes rows, and they are ANDed
* Good, because unlike shadowing, `share_mode` is a property of the row rather than of the requester's
  view, so it is storable, indexable, and OData-filterable on the admin surface
* Neutral, because the depth rule is encoded twice — once in Rust, once as SQL — held together by a
  drift test rather than by shared code
* Bad, because a descendant now has two reasons a model can vanish, and both surface as
  `ModelNotFound`. The indistinguishability is deliberate, but it makes support diagnosis harder; the
  owner's admin listing is the place to look
* Bad, because a model hidden while its provider stays visible leaves a gap the descendant cannot
  fill: model creation is same-tenant-only, so it must shadow the whole provider to supply its own

### Confirmation

* Unit tests verify the depth rule for every `(share_mode, distance)` pair, and that reach widens
  monotonically with the mode ordering — the property the two-stage AND depends on
* A drift test seeds one model per `(distance, mode)` pair against SQLite and asserts the eval listing
  matches the Rust rule, so the SQL predicate cannot silently diverge
* Unit tests verify narrowest-wins over the full provider × model grid, and that a `private` ancestor
  provider is transparent — the grandparent's same-slug provider wins for the child
* Unit tests verify an owner still sees its own `private` rows on both the eval and admin paths
* Unit tests verify `get_tenant_model` reports `ModelNotFound` for an unshared model that is *also*
  deprecated, unapproved, or on a disabled provider — the gate precedes all three, so none of the more
  specific errors can disclose that the row exists
* Unit tests verify an out-of-domain `share_mode` in the database fails the read rather than defaulting
* A migration test verifies the column is NOT NULL and defaults to `subtree` on both tables

## Pros and Cons of the Options

### Owner-declared share mode on both providers and models

An enum column on each table, evaluated as two independent stages that are ANDed.

* Good, because both granularities are covered by one concept with one meaning
* Good, because it costs no extra query, and the `subtree` default is exactly today's behaviour
* Neutral, because it adds a second lever to a design that deliberately had one — mitigated by the two
  pointing in opposite directions and both being restrict-only
* Bad, because the depth rule lives in two encodings and needs a drift test to stay honest

### Provider-level sharing only

`share_mode` on `providers` alone.

* Good, because a single stage and a single encoding of the depth rule
* Bad, because it leaves ADR-0004's per-model gap exactly where it was
* Bad, because withholding one model would mean splitting a catalog across two providers

### Consumer-side per-model exclusion list

A `(tenant_id, excluded_canonical_id)` table, `NOT IN` on the eval query.

* Good, because it also closes the per-model gap
* Bad, because it points the same direction as the existing lever and still leaves the owner powerless
* Bad, because two consumer-side levers whose interaction must be specified is the exact complexity
  ADR-0004 chose to avoid
* Bad, because the exclusion list is unbounded and needs a cap

### Push sharing into the PDP

Express ancestor visibility as policy in `authz-resolver` instead of a column.

* Good, because no schema change and centrally auditable
* Bad, because the eval path deliberately scopes ancestor rows by tenant alone (DESIGN §3.5), and the
  PDP has no notion of the requester's chain
* Bad, because the provider allow-list machinery stays here regardless, splitting the logic in two

## More Information

```
Root tenant: openai (subtree)  → gpt-4o (subtree), gpt-4o-internal (private)
             bedrock (children) → claude (subtree)
Tenant A:    child of root
Tenant A/B:  grandchild of root

Root       → openai::gpt-4o, openai::gpt-4o-internal, bedrock::claude
Tenant A   → openai::gpt-4o, bedrock::claude
Tenant A/B → openai::gpt-4o
```

`gpt-4o-internal` is withheld from both descendants — and
`GET /v1/models/openai::gpt-4o-internal` is `ModelNotFound` for them, not a distinct error.
`bedrock` reaches distance 1 only, and narrowest-wins caps its `subtree` model with it.

Transparency: if Tenant A instead owned a **private** `openai` provider, Tenant A/B would still
resolve `openai::gpt-4o` — from root's provider, because Tenant A's is invisible below it and so does
not win the slug. To block the slug for its whole subtree, Tenant A disables a shadowing provider
instead (ADR-0004).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **ADR-0004** (amended by this ADR): [Additive Tenant Inheritance with Provider Shadowing](./0004-cpt-cf-model-registry-adr-tenant-inheritance.md)

This decision directly addresses:

* `cpt-cf-model-registry-fr-share-mode` — Owner-declared sharing scope for providers and models
* `cpt-cf-model-registry-fr-provider-management` — Provider inheritance gains an owner-side lever
* `cpt-cf-model-registry-fr-manual-model-management` — A model's owner controls how far it is offered
* `cpt-cf-model-registry-fr-get-tenant-model` — Share-aware single-model resolution
* `cpt-cf-model-registry-fr-list-tenant-models` — Share-aware additive merge across the ancestor chain
* `cpt-cf-model-registry-fr-tenant-isolation` — Restrict-only is preserved
* `cpt-cf-model-registry-principle-additive-inheritance` — Extends the inheritance model
* `cpt-cf-model-registry-usecase-set-share-mode` — Setting the sharing scope of a provider or model
