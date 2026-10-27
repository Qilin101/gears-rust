# Model Registry

Tenant-aware catalog of AI providers and models. LLM Gateway and other consumer gears resolve models for evaluation through `ModelRegistryClientV1`; tenant administrators manage their own rows through the REST API. Inheritance and shadowing across the tenant hierarchy, provider enabling, sharing modes, and owner-tenant approval are specified in the design.

- Requirements: [docs/PRD.md](docs/PRD.md)
- Design: [docs/DESIGN.md](docs/DESIGN.md)
- SDK crate: [model-registry-sdk](model-registry-sdk) (`cf-gears-model-registry-sdk`)
- Gear crate: [model-registry](model-registry) (`cf-gears-model-registry`)
- Demo walkthrough: [DEMO.md](DEMO.md)

## REST surface

All paths are versioned under `/model-registry/v1/` and map one-to-one to the SDK trait methods.

| Method | Path | SDK method |
|--------|------|------------|
| `GET` | `/available-models/{canonical_id}` | `resolve_model` |
| `GET` | `/available-models` | `list_available_models` |
| `POST` | `/providers` | `create_provider` |
| `GET` | `/providers/{id}` | `get_provider` |
| `GET` | `/providers` | `list_providers` |
| `PATCH` | `/providers/{id}` | `update_provider` |
| `DELETE` | `/providers/{id}` | `delete_provider` |
| `POST` | `/models` | `create_model` |
| `GET` | `/models/{id}` | `get_model` |
| `GET` | `/models` | `list_models` |
| `PATCH` | `/models/{id}` | `update_model` |
| `DELETE` | `/models/{id}` | `delete_model` |

Lists accept `$filter`, `$orderby`, `$select`, `limit` (`$top`), and `cursor` (`$skiptoken`); the filter fields are the `ModelFilterField` and `ProviderFilterField` allowlists of the SDK.

## Configuration

Keys of the gear's `config` section; absent keys take the defaults. The gear refuses to start when a check fails.

| Key | Default | Meaning |
|-----|---------|---------|
| `chain_cache_ttl_seconds` | 30 | Ancestor-chain cache TTL; 0 disables the cache; at most 300 |
| `chain_cache_max_entries` | 100000 | Chain cache capacity |
| `default_page_size` | 20 | Page size when the request has no limit |
| `max_page_size` | 100 | Largest accepted page size |
| `max_json_document_bytes` | 65536 | Size limit for each of `provider_settings`, `metadata`, `additional_info`, `default_parameters` |
| `pdp_deadline_ms` | 1000 | Deadline for one PDP decision |
| `tenant_resolver_deadline_ms` | 1000 | Deadline for one `get_ancestors` call |
| `types_registry_deadline_ms` | 3000 | Deadline for one Types Registry schema read on writes |
| `max_pdp_constraint_values` | 1000 | Largest value list in one PDP row-constraint filter; a larger list denies |

Dependencies: `tenant-resolver`, `authz-resolver`, `types-registry`, and the platform database (PostgreSQL in production, SQLite in dev/test).

## Development

```bash
cargo test -p cf-gears-model-registry
cargo clippy -p cf-gears-model-registry --tests
```
