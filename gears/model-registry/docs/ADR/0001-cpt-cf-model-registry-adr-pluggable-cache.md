---
status: accepted
date: 2026-02-18
amended: 2026-08-20
---

# Pluggable Cache Backend with TTL Strategy


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Pluggable Cache Backend](#pluggable-cache-backend)
  - [Redis-only Distributed Cache](#redis-only-distributed-cache)
  - [In-memory Cache per Instance](#in-memory-cache-per-instance)
  - [No Cache (Database Only)](#no-cache-database-only)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-model-registry-adr-pluggable-cache`

## Context and Problem Statement

Model Registry must achieve <10ms P99 latency for `get_tenant_model` operations at scale (10K tenants, 2M models, 1000:1 read:write ratio). How should we implement caching to meet these performance requirements while allowing deployment flexibility?

## Decision Drivers

* `cpt-cf-model-registry-nfr-performance` — P99 latency <10ms for model resolution
* `cpt-cf-model-registry-nfr-scale` — Support 10K tenants, 2M models
* `cpt-cf-model-registry-fr-cache-isolation` — Tenant data must be isolated in cache
* Deployment flexibility — single-node to multi-node clusters (no cache infrastructure required for moderate scale, where DB-only plus a local cache suffices)

## Considered Options

* Pluggable Cache Backend (local in-memory in P1; distributed backend deferred)
* Redis-only distributed cache
* In-memory cache per instance
* No cache (database only)

## Decision Outcome

Chosen option: "Pluggable Cache Backend", because it keeps the read path behind one trait while letting each deployment pick the backend its scale justifies, and lets vendors substitute their own.

**P1 scope**: only the **local, in-process `InMemoryCache`** is implemented. The trait is the seam; no distributed backend is written, designed, or selected.

**Open question (deferred beyond P1)**: whether to add a distributed backend at all, and which technology it would be. Candidates are not limited to Redis — a platform-provided cache service, or a coordination-free design that leans on a shorter TTL, are both live options. The `redis = []` feature currently declared in the crate manifest is an empty placeholder whose name pre-judges this decision and should be revisited when a backend is actually chosen. Until then, multi-replica deployments run per-replica caches bounded by the TTL, which is the documented limit on the scale NFR.

### Consequences

* Good, because different deployment scenarios are supported (lightweight single-node with no cache infrastructure; high-load clusters once a distributed backend exists)
* Good, because testing is simplified with the in-memory backend
* Good, because the backend decision can be deferred without reworking any call site
* Bad, because additional abstraction layer adds complexity
* Bad, because cache behavior may differ slightly between backends
* Bad, because P1 ships only the in-memory backend, so horizontal scale-out stays blocked until the deferred decision is made

### Confirmation

* Code review verifies `CacheService` trait is implemented correctly
* Integration tests run against every implemented backend (P1: `InMemoryCache` only)
* Performance benchmarks confirm <10ms P99 at scale — pending the distributed-backend decision, and not yet measured for P1

## Pros and Cons of the Options

### Pluggable Cache Backend

Cache abstraction with compiled-in implementations selected via Cargo feature flags:
- `InMemoryCache` — the only implementation in P1: lightweight deployments and testing, and valid for moderate-scale production where DB query caching suffices
- A distributed backend for horizontal scaling — not implemented; technology undecided (see the open question above)

Cache key format: `mr:{tenant_id}:{entity}:{id}`, where `tenant_id` is the tenant that **owns** the row — not the tenant reading it.

TTL Strategy (common across backends): a single `cache_ttl_seconds`, default 10 minutes, applied to every entry.

* Good, because deployment flexibility (single-node → cluster)
* Good, because vendor customization supported
* Good, because simpler testing with in-memory backend
* Good, because the distributed-backend choice stays deferrable behind a stable trait
* Neutral, because requires trait abstraction
* Bad, because slight complexity increase

### Redis-only Distributed Cache

Hardcoded Redis implementation without abstraction.

* Good, because simpler implementation
* Good, because proven horizontal scaling
* Bad, because no flexibility for lightweight deployments
* Bad, because harder to test without Redis infrastructure
* Bad, because no vendor customization

### In-memory Cache per Instance

Local cache in each application instance.

* Good, because fastest (no network hop)
* Good, because simplest implementation
* Bad, because cache inconsistency between instances
* Bad, because memory pressure on instances
* Bad, because cold start penalty

### No Cache (Database Only)

Direct PostgreSQL queries with indexes, no caching layer.

* Good, because simplest architecture
* Good, because always consistent
* Bad, because cannot meet <10ms P99 at scale
* Bad, because database load increases linearly with read traffic

## More Information

Configuration example — P1 surface:
```yaml
model_registry:
  cache:
    backend: memory      # the only backend implemented in P1
    cache_ttl_seconds: 600
```

A distributed backend would add its own `backend:` value and a connection block. Neither the value nor the block shape is specified here — both land with the deferred backend decision.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-model-registry-nfr-performance` — cache-first reads target <10ms P99 latency
* `cpt-cf-model-registry-nfr-scale` — horizontal scaling requires the deferred distributed backend; P1's in-memory cache does not deliver it
* `cpt-cf-model-registry-fr-cache-isolation` — Cache key format ensures tenant isolation
* `cpt-cf-model-registry-principle-cache-first` — Establishes cache-first read pattern
