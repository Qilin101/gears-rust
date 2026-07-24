<!-- @cf:root-agents -->
```toml
cf-studio-path = ".cf-studio"
```

ALWAYS resolve and enforce prerequisites of skills/workflows/commands BEFORE applying user intent.
<!-- /@cf:root-agents -->

# CLAUDE.md — CF/Gears

## Project Overview

**Constructor Fabric (CF/Gears)** is a modular, high-performance Rust platform for building enterprise SaaS services. Everything is a **Gear** — a self-contained business capability discovered at build time via feature flags. The main binary is `cf-gears-example-server`.

### Terminology

- **Gear** — individual business capability (e.g., `api-gateway`, `tenant-resolver`, `file-parser`). Lives under `gears/`.
- **System Gear** — core platform infrastructure under `gears/system/`.
- **ToolKit** — the framework libraries under `libs/toolkit*` (replaces the old `modkit*` naming).
- **SDK Pattern** — each gear (usually) has an `*-sdk` crate with its public API surface.

---

## Essential Commands

```bash
# Setup
make setup                         # Install required dev tools

# Build & Run
make quickstart                    # Run server with SQLite (config/quickstart.yaml)
make example                       # Run with example gears (users-info, etc.)
cargo run --bin cf-gears-example-server -- --config config/quickstart.yaml run

# Testing
cargo test --workspace             # All tests
cargo test -p <crate_name>         # Single crate tests
cargo test -p <crate> -- <test_fn> # Single test function
make test-sqlite                   # SQLite integration tests
make test-pg                       # PostgreSQL integration tests

# Linting & Formatting
make fmt                           # Check formatting (rustfmt)
make clippy                        # Clippy linting
make dylint                        # Custom architectural lints
make deny                          # License & dependency checks (cargo-deny)
make lychee                        # Check links in docs

# Full CI
make check                         # fmt + cfs-validate + clippy + lychee + security + dylint + gts-docs + test
make openapi                       # Generate OpenAPI specs
make gts-docs                      # Validate GTS references in docs

# Tools
make dev                           # dev-fmt + dev-clippy + dev-test (quick iteration)
make safety                        # deny + fips-policy
```

## Workspace Layout

- **`apps/cf-gears-example-server/`** — Main binary, wires all gears via feature flags
- **`libs/toolkit*`** — Core framework libraries (ToolKit):
  - `toolkit` — Bootstrap, lifecycle, ClientHub, config, context, contracts
  - `toolkit-http` — HTTP/REST utilities
  - `toolkit-http-middleware` — HTTP middleware
  - `toolkit-db` / `toolkit-db-macros` — SeaORM-based secure DB access
  - `toolkit-auth` — Authentication helpers
  - `toolkit-security` — Security context
  - `toolkit-macros` — Proc macros
  - `toolkit-gts` / `toolkit-gts-macros` — Global Type System support
  - `toolkit-odata` / `toolkit-odata-macros` — OData $filter/$select/$orderby
  - `toolkit-sdk` — SDK trait patterns
  - `toolkit-canonical-errors` / `toolkit-canonical-errors-macro` — Standardized error types
  - `toolkit-transport-grpc` — gRPC transport for out-of-process gears
  - `toolkit-node-info` — Node identity
  - `toolkit-utils` — Shared utilities
- **`gears/`** — All business gears (file-parser, chat-engine, model-registry, etc.)
- **`gears/system/`** — Core platform gears (api-gateway, tenant-resolver, types-registry, grpc-hub, etc.)
- **`examples/`** — Example gears (toolkit/users-info, oop-gears/calculator, cf-gears-fips-probe)
- **`tools/`** — Development tools (gts-analyze, xtask, dylint_lints, fuzz)
- **`config/`** — YAML config files
- **`docs/`** — Documentation (especially `docs/toolkit_unified_system/`)
- **`guidelines/`** — Coding guidelines (DNA, DEPENDENCIES.md, SECURITY.md)

## Gear Pattern

Each gear typically follows an SDK pattern:
- **`<gear>-sdk/`** — Public API surface: traits, models, errors. Minimal dependencies.
- **`<gear>/`** (or inline) — Implementation: domain logic, REST handlers, DB entities, gear registration.

Gear internal layout (DDD-light):
- `src/api/rest/` — REST handlers and DTOs (serde + ToSchema derives only here)
- `src/domain/` — Business logic, domain models, contracts (no serde/HTTP types)
- `src/infrastructure/` — Database entities, repositories, migrations
- `src/gateways/` — Inter-gear clients via ClientHub

Gears are registered in `apps/cf-gears-example-server/src/registered_gears.rs` and wired via cargo feature flags.

## Key Architectural Rules

- **SDK pattern is the public API**: Inter-gear communication uses `<gear>-sdk` traits via `ClientHub`. Never depend on another gear's internals.
- **GTS for type identity**: Use `gts.<vendor>.<org>.<package>.<type>.<version>~` for globally unique type identifiers. Schema IDs always end with `~`. Validated by `make gts-docs`.
- **RFC-9457 errors**: Use canonical error types (Problem). Never use raw axum error responses.
- **Type-safe REST**: Use OperationBuilder with `.require_auth()` and `.standard_errors()` for route registration.
- **Layer separation enforced by dylint**: DTOs only in `api/rest/`, no serde/ToSchema in contract layer, API endpoints must have version prefix (e.g., `/gear/v1/resource`).
- **`unsafe` is forbidden** (`#![forbid(unsafe_code)]` workspace-wide).
- **`unwrap()`/`expect()` are denied** in non-test code.

## Configuration

YAML config with env var overrides. Key config files:
- `config/quickstart.yaml` — Development with SQLite
- `config/no-db.yaml` — No database mode
- `config/server.yaml` — Full server config

## Guidelines — When to Read What

Start with `guidelines/README.md`. Then:

| Task | Read |
|------|------|
| New gear | `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md` |
| REST endpoints | `docs/toolkit_unified_system/04_rest_operation_builder.md` |
| DB/persistence | `docs/toolkit_unified_system/06_authn_authz_secure_orm.md` |
| OData $filter/$select | `docs/toolkit_unified_system/07_odata_pagination_select_filter.md` |
| Errors | `docs/toolkit_unified_system/05_errors_rfc9457.md` |
| ClientHub/plugins | `docs/toolkit_unified_system/03_clienthub_and_plugins.md` |
| Dependencies changes | `guidelines/DEPENDENCIES.md` |
| REST API design | `guidelines/DNA/REST/API.md` |

## Recurring Patterns

### OData Filtering Requires Real Columns

The toolkit OData layer (`FieldToColumn::map_field` in `libs/toolkit-db/src/odata/sea_orm_filter.rs`) maps each filter field to exactly one real SeaORM `Column`. There is **no JSONB-path filtering** and no join support. If a gear needs OData filtering on fields stored inside a JSONB column, those fields must be **promoted to real columns** on the table, with B-tree indexes (not GIN, which would break SQLite dev/test). The model-registry gear took this rule to its logical end in 2026-07-24: the `models.info` JSONB column was dropped entirely and **every** `ModelInfoV1` field that promotes cleanly is now a typed scalar column. Fields that don't promote cleanly live in one of four small JSONB sub-object columns (`capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`); the polymorphic `provider_settings` JSONB column is the only remaining JSONB blob, keyed by `gts_type`. See `gears/model-registry/model-registry/src/infra/storage/mapper.rs` for the projection pattern.

### Integration Tests with SQLite + Mocked Clients

Integration tests against SQLite follow this pattern:
1. `setup_db()` — create in-memory SQLite DB with migrations applied via `run_migrations_for_testing`
2. Define mock structs implementing SDK client traits (e.g., `TenantResolverClient`, `AuthZResolverClient`)
3. `build_service(db, resolver)` — construct the `Service` with the real repo, cache, and mocked clients
4. Write tests using `service.create_provider/get_provider/...` through the real service layer
5. For cache tests, clone `DBProvider` before service construction to hold a separate connection for direct repo bypass

See `gears/model-registry/model-registry/tests/integration.rs` for a complete example.

## Code Style

- Rust Edition 2024, stable toolchain, MSRV 1.96.0 (`rust-toolchain.toml`)
- `rustfmt`: Unix newlines (follow workspace defaults)
- Clippy: pedantic + perf denied
- YAML: use `serde-saphyr` (not `serde_yaml` which is deprecated)
- Commits: `<type>(<scope>): <description>` (types: feat, fix, tech, refactor, test, docs, chore, perf)
- Commits must be signed off (DCO): `git commit -s`

## GTS (Global Type System)

The project uses GTS for globally unique type identifiers. Format: `gts.<vendor>.<org>.<package>.<type>.<version>~`. Schema IDs always end with `~`. Validated by `make gts-docs`.
