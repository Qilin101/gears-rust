# Model Registry — Demo

A local walkthrough of the twelve endpoints against the running `cf-gears-example-server`. SQLite only, auth disabled, no extra services.

## Run

From the repo root, in one terminal:

```bash
cargo run --bin cf-gears-example-server \
  --features model-registry,static-authn,static-authz,static-tenants,static-credstore \
  -- --config config/quickstart.yaml run
```

The server binds to `http://127.0.0.1:8087`. Watch the logs for `model-registry gear initialized`; the SQLite file is created at `~/.cf-gears/model-registry/model_registry.db`.

## Walk

In a second terminal:

```bash
bash gears/model-registry/scripts/demo.sh
```

The script first registers a demo provider type derived from `gts.cf.genai.model.provider.v1~` in Types Registry (the role an LLM Gateway provider plugin plays), then hits every endpoint in order: create provider, list/get/patch provider, create model, admin list/get, resolve, available list, patch (revoke), delete model, delete provider. It exits 0 on full success. Two steps have deliberately non-2xx outcomes: the resolve after the revoke returns `403` with reason `MODEL_NOT_APPROVED`, and a create with settings that violate the provider type schema returns `400` with `SCHEMA_MISMATCH` violations.

To target a different host:

```bash
BASE_URL=http://my-host:8087/cf bash gears/model-registry/scripts/demo.sh
```

## Endpoints

All routes are prefixed by `/cf` (the `prefix_path` in `config/quickstart.yaml`). Management operations address entities by UUID; resolution addresses a model by its canonical id, `provider_slug::provider_model_id`, percent-encoded as one path segment.

| # | Method | Path | Notes |
|---|--------|------|-------|
| 1 | `POST` | `/cf/model-registry/v1/providers` | Body has `slug`, `name`, `gts_type`; `enabled`, `sharing_mode` (`tenant`/`shared`), `managed`, `metadata` optional. `gts_type` must be a registered type derived from `gts.cf.genai.model.provider.v1~`. 201 with `Location`. |
| 2 | `GET` | `/cf/model-registry/v1/providers` | Own and inherited providers with `inherited`, `shadowed`, `available_for_eval` flags. OData `$filter`, `$orderby`, `$select`, `limit`, `cursor`. |
| 3 | `GET` | `/cf/model-registry/v1/providers/{id}` | |
| 4 | `PATCH` | `/cf/model-registry/v1/providers/{id}` | Any of `name`, `enabled`, `sharing_mode`, `managed`, `metadata` (`null` clears). Immutable fields are rejected with `IMMUTABLE_FIELD`. |
| 5 | `POST` | `/cf/model-registry/v1/models` | Body has `provider_id`, `provider_model_id`, `display_name`, `context_window`, optional `sharing_mode`, `approval_state` (default `approved`), and the other Model Info fields. `canonical_id` and `gts_type` are set by the gear; `provider_settings` is validated against the provider type schema. |
| 6 | `GET` | `/cf/model-registry/v1/models` | Admin list: own and inherited models in every state, with flags. |
| 7 | `GET` | `/cf/model-registry/v1/models/{id}` | |
| 8 | `GET` | `/cf/model-registry/v1/available-models/{canonical_id}` | Resolve for evaluation: visible, provider enabled, approved, permitted. `::` is encoded as `%3A%3A`. The response omits `provider_settings`, `base_url`, and `secret_ref`. |
| 9 | `GET` | `/cf/model-registry/v1/available-models` | Exactly the models that resolve, in the same redacted shape. |
| 10 | `PATCH` | `/cf/model-registry/v1/models/{id}` | Any mutable Model Info field, `sharing_mode`, or `approval_state` (`pending`/`approved`/`rejected`/`revoked`). |
| 11 | `DELETE` | `/cf/model-registry/v1/models/{id}` | Hard delete, 204. |
| 12 | `DELETE` | `/cf/model-registry/v1/providers/{id}` | Deletes the provider and its models, 204. |

OpenAPI is served at `http://127.0.0.1:8087/cf/docs` (Swagger UI).

## Notes

- Auth is disabled; requests act as the root tenant `00000000-df51-5b42-9538-d2b56b7ee953`.
- Errors are RFC 9457 Problem documents. Reason codes: `PROVIDER_DISABLED`, `MODEL_NOT_APPROVED`, `NOT_PERMITTED`, `INHERITED_READ_ONLY`; field reasons: `INVALID_FORMAT`, `TOO_LARGE`, `IMMUTABLE_FIELD`, `INVALID_GTS_TYPE`, `NOT_A_PROVIDER_TYPE`, `GTS_TYPE_NOT_REGISTERED`, `SCHEMA_MISMATCH`.
- To reset state between runs: `rm -rf ~/.cf-gears/model-registry`.
