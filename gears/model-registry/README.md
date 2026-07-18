# Model Registry

Model catalog with tenant-level availability, approval workflow, and inheritance.

## P1 REST Surface

All P1 endpoints are implemented under `/model-registry/v1/`. Each endpoint requires authentication and license validation.

### Providers

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/model-registry/v1/providers` | List tenant providers with OData filtering |
| `GET` | `/model-registry/v1/providers/{id}` | Get provider by ID (cache-first) |
| `POST` | `/model-registry/v1/providers` | Register a new provider |
| `PATCH` | `/model-registry/v1/providers/{id}` | Partial update (slug immutable) |
| `DELETE` | `/model-registry/v1/providers/{id}` | Delete provider |

### Models

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/model-registry/v1/models` | List tenant models with OData filtering |
| `GET` | `/model-registry/v1/models/{canonical_id}` | Get model by canonical ID (cache-first) |
| `POST` | `/model-registry/v1/models` | Register a new model |
| `PATCH` | `/model-registry/v1/models/{canonical_id}` | Partial update including approval status |
| `DELETE` | `/model-registry/v1/models/{canonical_id}` | Soft-delete (marks deprecated) |

### OData Filterable Fields

**Models**: `canonical_id`, `lifecycle_status`, `approval_status`, `gts_type`, `supported_api`, `provider_model_id`, `vendor`, `family`, `managed`, `architecture`, `format`, `vision`, `function_calling`, `streaming`, `reasoning_effort`

**Providers**: `slug`, `name`, `status`, `gts_type`, `managed`, `discovery_enabled`

## Key Features

- **Cache-first reads**: In-memory cache with TTL (30min own, 5min inherited), tenant-prefixed keys
- **Tenant inheritance**: Additive inheritance with child-shadowing by slug/canonical_id
- **Approval management**: Direct approval status writes via `PATCH /models/{canonical_id}` (P1)
- **Tenant isolation**: `AccessScope`-enforced queries at the repository layer
- **OData filtering**: `$filter`, `$top`, `$skip`, `$orderby`, `$select` on allowlisted fields

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
