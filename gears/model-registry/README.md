# Model Registry

Model catalog with tenant-level availability, approval workflow, and eval-path inheritance.

## P1 REST Surface

All P1 endpoints require authentication and license validation, and split into two zones:

- **`/model-registry/v1/…`** — eval-facing reads, open to any authenticated tenant member, keyed by `canonical_id`, resolved across the caller's tenant ancestor chain.
- **`/model-registry/v1/admin/…`** — management surface (tenant-admin / platform-admin), keyed by UUID `id`, bounded by the PDP access scope alone. Additive inheritance does not apply here: the admin surface reads and writes exactly the rows the scope covers.

A `canonical_id` is `{provider_slug}::{provider_model_id}`, and provider slugs shadow down the tenant
hierarchy, so the same string resolves to different rows for different tenants. It is a lookup key
for the eval read, never an addressing key for a write.

### Eval

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/model-registry/v1/models` | List models available for eval, with OData filtering |
| `GET` | `/model-registry/v1/models/{canonical_id}` | Get model by canonical ID. Fail-closed: approved, live, and on an active winning provider, else 403/404 |

### Admin — models

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/model-registry/v1/admin/models` | Management listing with provider-visibility flags |
| `POST` | `/model-registry/v1/admin/models` | Register a new model (body carries `provider_id`; the model takes that provider's tenant) |
| `GET` | `/model-registry/v1/admin/models/{id}` | Get model by ID — no eval gates |
| `PATCH` | `/model-registry/v1/admin/models/{id}` | Partial update including approval status |
| `DELETE` | `/model-registry/v1/admin/models/{id}` | Soft-delete (marks deprecated) |

### Admin — providers

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/model-registry/v1/admin/providers` | List providers in the caller's access scope, with OData filtering |
| `GET` | `/model-registry/v1/admin/providers/{id}` | Get provider by ID |
| `POST` | `/model-registry/v1/admin/providers` | Register a new provider (`slug` is the natural key being created) |
| `PATCH` | `/model-registry/v1/admin/providers/{id}` | Partial update (slug immutable) |
| `DELETE` | `/model-registry/v1/admin/providers/{id}` | Delete provider |

### Management DTO Fields

The `GET /admin/models` response wraps `ModelV1` with two read-only flags in `ModelManagementV1`:

- **`provider_disabled`** (bool) — the model's provider is disabled; the model is excluded from eval listing but retained for audit
- **`available_for_eval`** (bool) — model approved, not deprecated/sunset, provider active

`available_for_eval` does not account for provider shadowing, which is relative to a requester's tenant chain: a model marked `available_for_eval` may still be hidden from a given tenant's eval listing by a closer provider owning the same slug.

Deprecated models are excluded from the default management listing; pass `include_deprecated=true` to include them.

### OData Filterable Fields

**Models**: `canonical_id`, `lifecycle_status`, `approval_status`, `gts_type`, `supported_api`, `provider_model_id`, `vendor`, `family`, `managed`, `architecture`, `format`, `vision`, `function_calling`, `streaming`, `reasoning_effort`

**Providers**: `slug`, `name`, `status`, `gts_type`, `managed`, `discovery_enabled`

## Key Features

- **Cache-first reads**: In-memory cache with a single TTL (`cache_ttl_seconds`, default 10min), tenant-prefixed keys. Only ancestor chains are cached; no model or provider row is
- **Tenant inheritance**: Additive inheritance with child-shadowing by provider slug, applied on the eval reads only
- **Approval management**: Direct approval status writes via `PATCH /admin/models/{id}` (P1)
- **Tenant isolation**: `AccessScope`-enforced queries at the repository layer, with the resource `id` supplied to the PDP on every point operation
- **OData filtering**: `$filter` / `$orderby` on the allowlisted fields published by `model_registry_sdk::odata`, plus cursor-based pagination (`$top` + opaque cursor). `$select` and `$skip` are not supported
- **Typed query building**: SDK consumers build queries with `QueryBuilder::<ModelSchema>` and the `MODEL_*` / `PROVIDER_*` field references instead of `$filter` text — see `model-registry-sdk/src/odata/`

## Build & Run

```bash
# Build
cargo build -p cf-gears-model-registry

# Run unit + integration tests
cargo test -p cf-gears-model-registry

# Lint
cargo clippy -p cf-gears-model-registry --all-targets --all-features -- -D warnings -D clippy::perf

# Format
cargo fmt -p cf-gears-model-registry
```

Enabled via the `model-registry` cargo feature in the example server (`apps/cf-gears-example-server`).

## Documentation

- [PRD.md](docs/PRD.md)
- [DESIGN.md](docs/DESIGN.md)
