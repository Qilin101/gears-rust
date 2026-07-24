---
status: accepted
date: 2026-05-06
---

# GTS-typed provider settings with raw JSON default carrier


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
    - [Consequences (added 2026-07-24)](#consequences-added-2026-07-24)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [GTS-typed envelope with raw JSON default carrier](#gts-typed-envelope-with-raw-json-default-carrier)
  - [Tagged enum carrier (`AnyProviderSettings`)](#tagged-enum-carrier-anyprovidersettings)
  - [`Box<dyn ProviderSettings>` trait object](#boxdyn-providersettings-trait-object)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-model-registry-adr-gts-typed-provider-settings`

## Context and Problem Statement

The model registry stores and transmits provider-specific settings (routing aliases, default inference parameters, token pricing, override policy) for different providers. Each provider's settings shape is structurally different, yet a heterogeneous list endpoint (`list_tenant_models`) and a single polymorphic JSONB storage column have to carry all of them through one envelope. We need a discriminator that survives REST and JSONB round-trips, lets new providers be wired in without an SDK release, and does not require a second source of truth alongside the typed Rust structs.

## Decision Drivers

* `cpt-cf-model-registry-fr-list-tenant-models` — heterogeneous catalog listing across providers
* `cpt-cf-model-registry-fr-get-tenant-model` — typed read of a single model's settings
* `cpt-cf-model-registry-fr-provider-management` — provider CRUD with provider-specific routing/pricing fields
* `cpt-cf-model-registry-component-sdk` — SDK crate is the public contract; persistence and REST DTOs depend on it
* `cpt-cf-model-registry-dbtable-models` — one polymorphic JSONB column (`models.provider_settings`) is the storage target for provider-specific settings; five additional fixed-shape JSONB columns (`capabilities_full`, `default_parameters`, `additional_info`, `disabled_capabilities_full`, `allow_extra_params`) store non-polymorphic `ModelInfoV1` sub-objects, all keyed by the top-level `gts_type` scalar column on the same row (added 2026-07-24 — see "Consequences (added 2026-07-24)")
* Forward compatibility — operators register a brand-new provider before the SDK ships typed settings for it
* Project standard — GTS (`gts.<vendor>.<org>.<package>.<type>.<version>~`) is the global type-identifier scheme already enforced repo-wide and validated by `make dylint` + `make gts-docs`

## Considered Options

* GTS-typed envelope with raw JSON default carrier
* Tagged enum carrier (`AnyProviderSettings`)
* `Box<dyn ProviderSettings>` trait object

## Decision Outcome

Chosen option: "GTS-typed envelope with raw JSON default carrier", because the GTS schema id becomes the single source of truth for the provider variant — eliminating a parallel Rust enum — and unknown providers automatically ride as opaque JSON until the SDK ships a typed leaf for them, removing the SDK release from the critical path of onboarding a new provider.

Concretely:

* `ModelInfoV1<P: gts::GtsSchema = serde_json::Value>` is declared as the GTS base envelope (`gts.cf.genai.model.info.v1~`).
* Each shipped per-provider settings type (e.g. `OpenAiSettingsV1`, `AnthropicSettingsV1`; the shipped set is open-ended and lives in `models/providers/`) is decorated with `#[struct_to_gts_schema(base = ModelInfoV1, …)]` and chains off that base via schema ids of the form `gts.cf.genai.model.info.v1~cf.genai._.<provider>.v1~`.
* `serde_json::Value` is the default `P` on the SDK's public API (`Model` / `ModelInfoV1` with no type parameter). It implements `gts::GtsSchema` upstream in the `gts` crate, so there is no hand-written newtype carrier in this SDK.
* `gts_type: gts::GtsTypeId` (top-level field of `ModelInfoV1`) is the canonical discriminator — `info.gts_type` is no longer a thing; `gts_type` lives at the envelope root and is mirrored 1:1 as the `models.gts_type` scalar column. Consumers narrow with `Model::try_into_typed::<OpenAiSettingsV1>()`, which delegates to `gts::try_narrow` — checking `gts_type == <Target>::TYPE_ID` and deserializing the payload — returning `Result<Model<Q>, gts::NarrowError>`.

### Consequences

* The SDK layer participates in serde — `#[struct_to_gts_schema]` emits `serde::Serialize` / `serde::Deserialize` / `schemars::JsonSchema` impls on the typed settings and their inner sub-structs. This is an explicit exception to the project rule "no serde on contract types"; GTS by design needs serde for runtime schema reflection.
* The default `Model` (`P = serde_json::Value`) is the public shape returned by `ModelRegistryClientV1`; the previous `Model<AnyProviderSettings>` tagged-enum carrier and its `ProviderKind` discriminant are removed.
* Persistence is one polymorphic JSONB column (`models.provider_settings`) whose shape is identified by the row's `gts_type` (top-level scalar column on `models`, mirroring the top-level `ModelInfoV1::gts_type` field — **not** nested under `info`). On-disk tag and runtime resolution key are identical (`<TypedSettings>::TYPE_ID`), so there is no impedance mismatch between the typed Rust view and the JSONB shape.
* Adding a new provider is a kit-level operation: register the new GTS leaf (`gts.cf.genai.model.info.v1~cf.genai._.<vendor>.v1~`); models targeting the new provider keep flowing through the SDK as raw JSON (`serde_json::Value`) until a typed leaf ships.
* OData filtering migrates from the flat enum-discriminated `info.provider_settings.kind` to exact / prefix match on `gts_type` (e.g. `gts_type eq 'gts.cf.genai.model.info.v1~cf.genai._.openai.v1~'`). Per-provider parameter and cost fields remain non-filterable in v1 because the JSONB shape varies per provider.
* Typed-narrowing errors are surfaced from the `gts` crate: `try_into_typed` returns `gts::NarrowError { SchemaId { expected, actual }, Deserialize(serde_json::Error) }`, distinguishing schema-id mismatches from JSON-shape mismatches (replacing the earlier provider-local `ProviderKindMismatch` / `ProviderSchemaMismatch` errors).

### Consequences (added 2026-07-24)

The single polymorphic JSONB column above is no longer the only JSONB column on `models`. As of the schema decomposition in `docs/plans/20260724-drop-models-info-jsonb.md` (Tasks 1-7), the table carries **six JSONB columns** total. Only one is polymorphic; the other five are fixed-shape sub-objects of `ModelInfoV1`:

| Column | Shape | Discriminator / role |
|---|---|---|
| `provider_settings` | Polymorphic (provider-dependent) | `gts_type` (top-level scalar) — **the original polymorphic JSONB from this ADR** |
| `capabilities_full` | Fixed (`ModelCapabilities` minus the 4 OData-promoted booleans) | None — content matches `ModelCapabilities`; columns are authoritative for the 4 promoted booleans |
| `default_parameters` | Fixed (`DefaultInferenceParametersV1`) | None — sub-object promoted as-is |
| `additional_info` | Fixed (`HashMap<String, serde_json::Value>`) | None — forward-compat escape hatch |
| `disabled_capabilities_full` | Fixed (`DisabledCapabilities`) | None — symmetric with `capabilities_full` |
| `allow_extra_params` | Fixed (`Vec<String>` of caller-supplied parameter names) | None — flat array per the plan's `allow_extra_params` user decision |

Of these six columns, only `provider_settings` carries a polymorphic payload. The other five hold sub-objects whose shape is fully described by `ModelInfoV1`; they are not keyed by `gts_type`, not extensible per provider, and not interpretable as anything other than the named `ModelInfoV1` field. They exist because the corresponding `ModelInfoV1` sub-objects (nested capabilities, parameter defaults, free-form maps, etc.) are too granular or too nested to promote to scalar columns without exploding the schema.

The original `info` JSONB column (the "full source of truth") has been dropped. The `models` table now stores every `ModelInfoV1` field as either a typed scalar column (17 promoted columns), one of the five fixed-shape JSONB sub-object columns listed above, or the polymorphic `provider_settings`. The on-disk tag (`gts_type`) and runtime resolution key (`<TypedSettings>::TYPE_ID`) remain identical, so the GTS-typed-envelope decision is unaffected — the discriminator still identifies the polymorphic column's shape; the fixed-shape sub-object columns are simply additional storage for non-polymorphic `ModelInfoV1` content.

### Confirmation

* Unit tests in `model-registry-sdk/src/models/entity.rs` exercise `try_into_typed` resolution by GTS id and the `serde_json::Value` default carrier path.
* `make dylint` validates GTS schema id format on Rust source.
* `make gts-docs` validates GTS ids referenced from markdown / JSON / YAML.
* DESIGN.md §3 (Domain Model) and §6 (Data Model) describe the envelope, the leaf schema chain, and the JSONB column shape.

## Pros and Cons of the Options

### GTS-typed envelope with raw JSON default carrier

`ModelInfoV1<P: gts::GtsSchema = serde_json::Value>` with `#[struct_to_gts_schema]` leaves per provider; discrimination by the top-level `gts_type` field of the envelope.

* Good, because GTS schema id is the single source of truth — no parallel Rust enum to keep in sync with the on-disk tag.
* Good, because new providers are wired in without an SDK release: unknown `gts_type` values keep flowing as raw JSON (`serde_json::Value`), and the routing layer can still dispatch on the schema id.
* Good, because the on-disk JSONB tag and the runtime resolution key are the same string (`<P>::TYPE_ID`), so there is no translation layer between storage and the SDK.
* Good, because it composes with the rest of the platform — GTS is already the global type-identifier scheme, validated by `make dylint` and `make gts-docs`.
* Good, because each typed leaf publishes its own JSON schema (via `schemars`), which the REST and OData layers can expose per provider rather than collapsing them under one tagged-enum variant.
* Good, because it follows the established SDK idiom in this repo (`oagw-sdk::ServerEventsStream<T = ServerEvent>`, `types-registry-sdk::GtsEntity<C = serde_json::Value>` / `DynGtsEntity`).
* Neutral, because consumers wanting a typed view must call `Model::try_into_typed::<P>()` once at the boundary; they cannot pattern-match on a Rust enum.
* Bad, because the SDK layer must derive `Serialize` / `Deserialize` / `JsonSchema` on contract types — an explicit exception to the project's "no serde on contract types" rule.
* Bad, because the default carrier is `serde_json::Value` at the SDK boundary; consumers that forget to narrow operate on opaque JSON.

### Tagged enum carrier (`AnyProviderSettings`)

The pre-decision approach: a tagged enum with one variant per typed settings struct plus a `Custom { api_family, raw: serde_json::Value }` arm, discriminated at runtime via `ProviderSettings::kind() -> ProviderKind`.

* Good, because pattern matching gives compile-time exhaustiveness on the four shipped providers.
* Good, because the SDK contract types stay free of serde derives — only the `Custom { raw }` arm imports `serde_json::Value`.
* Bad, because two sources of truth coexist: the Rust enum variant and the JSONB `kind` tag must be kept in lockstep — every new provider needs an SDK release that adds a variant before tenants can register typed settings.
* Bad, because the `Custom { api_family, raw }` arm is a special case that downstream code (REST DTO, JSONB read, OData filter) must each handle separately, doubling the routing logic.
* Bad, because the discriminator (`ProviderKind::Other(api_family)`) is a free-form string that does not survive central validation — there is no equivalent of `make dylint` / `make gts-docs` for `api_family` strings.
* Bad, because schemars cannot publish a per-provider JSON schema cleanly: the tagged enum collapses all four shapes under one tagged-union schema rather than four independent leaves.

### `Box<dyn ProviderSettings>` trait object

Erase the provider type behind a trait object and dispatch dynamically.

* Good, because the public API stays uniform without a generic parameter.
* Bad, because `Clone` and `PartialEq` are not object-safe in the form the SDK domain models need; the rest of the registry derives both, and the only `Box<dyn …>` usages elsewhere in the SDK layer (`oagw-sdk` streaming wrappers) are ephemeral, never stored fields.
* Bad, because serialisation of a trait object requires a manual tag-and-payload protocol that re-invents what GTS already gives us for free.
* Bad, because storage cannot persist a trait object — it would still need a schema-id tag in the JSONB blob, putting us back in the GTS-id-as-discriminator design without any of its tooling support.

## More Information

Implementation reference: commit `95bef876` ("Add gts") on branch `feature/model-registry-sdk`. The commit:

* Adds `gts` and `gts-macros` workspace dependencies to `model-registry-sdk/Cargo.toml`.
* Decorates `ModelInfoV1` with `#[struct_to_gts_schema(base = true, schema_id = "gts.cf.genai.model.info.v1~", …)]` and each per-provider settings type with `#[struct_to_gts_schema(base = ModelInfoV1, schema_id = "gts.cf.genai.model.info.v1~cf.genai._.<provider>.v<n>~", …)]` (`<n>` = the per-provider schema generation, currently `1` across all providers).
* Replaces `AnyProviderSettings` and `ProviderKind` with a raw-JSON default carrier (`serde_json::Value`) and the `gts::NarrowError` typed-narrowing error. (The initial implementation introduced a `RawProviderSettings` newtype + a local `ProviderSchemaMismatch` error; both were later dropped in favor of `serde_json::Value` and `gts::try_narrow` / `gts::NarrowError` once `gts` provided them directly.)
* Updates `DESIGN.md` §3 (Domain Model) and §6 (Data Model) to describe the envelope, the leaf schema chain, and the JSONB column shape.

GTS schema chain shipped with this decision (extensible — additional provider leaves can be added without touching the base):

```text
gts.cf.genai.model.info.v1~                              (base envelope: ModelInfoV1)
gts.cf.genai.model.info.v1~cf.genai._.openai.v1~         (OpenAiSettingsV1 leaf)
gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~      (AnthropicSettingsV1 leaf)
```

Established repo idioms aligned with this decision:

* `oagw-sdk::ServerEventsStream<T: FromServerEvent = ServerEvent>` — generic with default carrier
* `types-registry-sdk::GtsEntity<C = serde_json::Value>` with `DynGtsEntity` alias — JSON default + typed narrowing
* GTS validation via `make dylint` (Rust source) and `make gts-docs` (markdown / JSON / YAML)

Related decisions:

* [`cpt-cf-model-registry-adr-oagw-provider-access`](0003-cpt-cf-model-registry-adr-oagw-provider-access.md) — keeps credentials at OAGW; only the routing alias (`oagw_alias`) lives in `*Connection` sub-structs of the typed settings.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-model-registry-fr-list-tenant-models` — heterogeneous catalog ride on the default `Model` shape (`P = serde_json::Value`)
* `cpt-cf-model-registry-fr-get-tenant-model` — typed view via `Model::try_into_typed::<P>()`, resolved by GTS schema id
* `cpt-cf-model-registry-fr-provider-management` — provider-specific routing / parameters / cost stored in the typed per-provider settings leaves
* `cpt-cf-model-registry-fr-model-pricing` — token pricing lives in each provider's nested cost sub-struct, validated through its GTS leaf schema
* `cpt-cf-model-registry-component-sdk` — public SDK contract is the default `Model` (`P = serde_json::Value`) with typed-narrowing helpers
* `cpt-cf-model-registry-dbtable-models` — one polymorphic JSONB column (`provider_settings`) tagged by the top-level `gts_type` scalar column, plus five fixed-shape JSONB sub-object columns for non-polymorphic `ModelInfoV1` content (see "Consequences (added 2026-07-24)")
