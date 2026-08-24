# Model Registry — Demo

A local walkthrough of all 10 P1 endpoints against the running `cf-gears-example-server`. SQLite only, auth disabled, no extra services.

## Run

From the repo root, in one terminal:

```bash
cargo run --bin cf-gears-example-server \
  --features model-registry,static-authn,static-authz,static-tenants,static-credstore \
  -- --config config/quickstart.yaml run
```

The server binds to `http://127.0.0.1:8087`. Watch the logs for `model-registry gear initialized successfully`; the SQLite file is created at `~/.cf-gears/model-registry/model_registry.db`.

## Walk

In a second terminal:

```bash
bash gears/model-registry/scripts/demo.sh
```

The script hits every endpoint in order — create provider, list/get/patch, create model, list/get/patch, soft-delete model, delete provider — and prints each request's response. It exits 0 on full success; the final `DELETE /providers/{id}` is allowed to return either `204` (clean state) or `400` `failed_precondition` (because the soft-deleted model still holds the FK).

To target a different host:

```bash
BASE_URL=http://my-host:8087/cf/model-registry/v1 bash gears/model-registry/scripts/demo.sh
```

## Endpoints

All routes are prefixed by `/cf` (the `prefix_path` in `config/quickstart.yaml:94`) and live under `/model-registry/v1/`:

| # | Method | Path | Notes |
|---|--------|------|-------|
| 1 | `POST`   | `/cf/model-registry/v1/providers` | Body has `slug`, `name`, `gts_type`; `managed`, `metadata`, `discovery_enabled` optional. |
| 2 | `GET`    | `/cf/model-registry/v1/providers` | OData: `$filter`, `$limit`, `$orderby`. |
| 3 | `GET`    | `/cf/model-registry/v1/providers/{id}` | UUID. |
| 4 | `PATCH`  | `/cf/model-registry/v1/providers/{id}` | Slug is immutable. `metadata: null` clears the field. |
| 5 | `DELETE` | `/cf/model-registry/v1/providers/{id}` | 204 on success. Fails if models reference the provider. |
| 6 | `POST`   | `/cf/model-registry/v1/models` | Body has `provider_slug`, `lifecycle_status`, optional `approval_status`, and the full `info` envelope. |
| 7 | `GET`    | `/cf/model-registry/v1/models` | OData on `canonical_id`, `lifecycle_status`, `approval_status`, `gts_type`, `vendor`, etc. |
| 8 | `GET`    | `/cf/model-registry/v1/models/{canonical_id}` | `canonical_id = {provider_slug}::{provider_model_id}` — `::` must be URL-encoded as `%3A%3A`. |
| 9 | `PATCH`  | `/cf/model-registry/v1/models/{canonical_id}` | Update display, capabilities, or flip `approval_status` (`pending` / `approved` / `rejected` / `revoked`). |
| 10 | `DELETE` | `/cf/model-registry/v1/models/{canonical_id}` | Soft-delete — sets `lifecycle_status` to `deprecated`. |

OpenAPI spec is served at `http://127.0.0.1:8087/cf/docs` (Swagger UI).

## Notes

- Auth is disabled; requests use the root tenant `00000000-df51-5b42-9538-d2b56b7ee953` automatically.
- `lifecycle_status` and `approval_status` are lowercase strings: `production`, `preview`, `experimental`, `deprecated`, `sunset`; `pending`, `approved`, `rejected`, `revoked`.
- `gts_type` must end with `~`. The base schemas (`gts.cf.genai.model.info.v1~`, `gts.cf.genai.model.provider.v1~`) are valid; provider-specific leaves like `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~` are typed more strictly.
- To reset state between runs: `rm -rf ~/.cf-gears/model-registry`.


## running example server
  cargo run --bin cf-gears-example-server \
    --features model-registry,static-authn,static-authz,static-tenants,static-credstore \
    -- --config config/quickstart.yaml run