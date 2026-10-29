//! Mock [`ModelRegistryClientV1`] holding a set of pre-built models keyed by
//! `canonical_id`.
//!
//! Ships three models: `openai::gpt-4o` (approved, served by the demo
//! plugin), `openai::gpt-4o-mini` (revoked, to show the approval gate), and
//! `anthropic::claude-sonnet-4` (approved, no plugin registered).
//!
//! `resolve_model` applies the registry's approval gate and reports the same
//! canonical errors the gear does; `list_available_models` evaluates the
//! caller's `$filter` AST in memory over the approved fixtures and returns
//! them as `AvailableModelV1`, resolving each
//! identifier through [`ModelFilterField::from_name`], the same allowlist the
//! real gear applies before mapping a field to a column. The management
//! methods are not part of the demo.

use std::collections::HashMap;

use async_trait::async_trait;
use model_registry_sdk::odata::ModelFilterField;
use model_registry_sdk::reason::permission;
use model_registry_sdk::{
    ApprovalState, AvailableModelV1, CreateModelRequestV1, CreateProviderRequestV1, ModelEntryV1,
    ModelRegistryClientV1, ModelV1, ProviderEntryV1, SupportedApi, UpdateModelRequestV1,
    UpdateProviderRequestV1,
};
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_odata::ast::{CompareOperator, Expr, Value};
use toolkit_odata::filter::FilterField as _;
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Page size applied when the caller sets no `limit`, matching the gear's own
/// default.
const DEFAULT_PAGE_SIZE: u64 = 20;

/// Provider type of the `OpenAI` fixtures.
pub const OPENAI_PROVIDER_TYPE: &str = "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~";

/// Provider type of the `Anthropic` fixture.
pub const ANTHROPIC_PROVIDER_TYPE: &str = "gts.cf.genai.model.provider.v1~cf.genai._.anthropic.v1~";

/// The same error scope the gear uses for model-addressed outcomes; its
/// literal equals `model_registry_sdk::gts::MODEL_RESOURCE_TYPE`.
#[resource_error(gts_id!("cf.genai.model_registry.model.v1~"))]
struct ModelScope;

/// A mock registry holding pre-built models in a map keyed by `canonical_id`.
pub struct MockModelRegistry {
    by_id: HashMap<String, ModelV1>,
}

impl MockModelRegistry {
    /// Build the mock from JSON fixtures.
    ///
    /// # Errors
    ///
    /// Returns the underlying `serde_json` error if any fixture fails to
    /// deserialize.
    pub fn new() -> Result<Self, serde_json::Error> {
        let mut by_id = HashMap::new();
        for fixture in [OPENAI_FIXTURE, OPENAI_MINI_FIXTURE, ANTHROPIC_FIXTURE] {
            let model: ModelV1 = serde_json::from_str(fixture)?;
            by_id.insert(model.canonical_id.clone(), model);
        }
        Ok(Self { by_id })
    }

    /// Every fixture, for the schema check in `main`.
    pub fn models(&self) -> impl Iterator<Item = &ModelV1> {
        self.by_id.values()
    }
}

#[async_trait]
impl ModelRegistryClientV1 for MockModelRegistry {
    async fn resolve_model(
        &self,
        _ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, CanonicalError> {
        let model = self.by_id.get(canonical_id).ok_or_else(|| {
            ModelScope::not_found("model not found")
                .with_resource(canonical_id)
                .create()
        })?;
        if model.approval.state != ApprovalState::Approved {
            return Err(ModelScope::permission_denied()
                .with_reason(permission::MODEL_NOT_APPROVED)
                .create());
        }
        Ok(model.clone())
    }

    async fn list_available_models(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<AvailableModelV1>, CanonicalError> {
        // Sorted by `canonical_id`, the default order of the real gear.
        let mut candidates: Vec<&ModelV1> = self
            .by_id
            .values()
            .filter(|m| m.approval.state == ApprovalState::Approved)
            .collect();
        candidates.sort_by(|a, b| a.canonical_id.cmp(&b.canonical_id));

        let mut items = Vec::new();
        for model in candidates {
            let keep = match query.filter() {
                Some(expr) => eval(expr, model)?,
                None => true,
            };
            if keep {
                items.push(AvailableModelV1::from(model.clone()));
            }
        }

        let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
        items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));

        Ok(Page {
            items,
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
            },
        })
    }

    async fn create_provider(
        &self,
        _ctx: &SecurityContext,
        _req: CreateProviderRequestV1,
    ) -> Result<ProviderEntryV1, CanonicalError> {
        Err(unsupported())
    }

    async fn get_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ProviderEntryV1, CanonicalError> {
        Err(unsupported())
    }

    async fn list_providers(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ProviderEntryV1>, CanonicalError> {
        Err(unsupported())
    }

    async fn update_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _req: UpdateProviderRequestV1,
    ) -> Result<ProviderEntryV1, CanonicalError> {
        Err(unsupported())
    }

    async fn delete_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), CanonicalError> {
        Err(unsupported())
    }

    async fn create_model(
        &self,
        _ctx: &SecurityContext,
        _req: CreateModelRequestV1,
    ) -> Result<ModelEntryV1, CanonicalError> {
        Err(unsupported())
    }

    async fn get_model(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ModelEntryV1, CanonicalError> {
        Err(unsupported())
    }

    async fn list_models(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ModelEntryV1>, CanonicalError> {
        Err(unsupported())
    }

    async fn update_model(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _req: UpdateModelRequestV1,
    ) -> Result<ModelEntryV1, CanonicalError> {
        Err(unsupported())
    }

    async fn delete_model(&self, _ctx: &SecurityContext, _id: Uuid) -> Result<(), CanonicalError> {
        Err(unsupported())
    }
}

fn unsupported() -> CanonicalError {
    CanonicalError::internal("management operations are not part of the demo").create()
}

fn invalid_filter(message: impl Into<String>) -> CanonicalError {
    ModelScope::invalid_argument()
        .with_field_violation("$filter", message, "INVALID_FILTER")
        .create()
}

// ===========================================================================
// In-memory `$filter` evaluation
// ===========================================================================

/// The scalar one filter field resolves to for a given model.
enum Scalar {
    Text(Option<String>),
    Flag(bool),
    Int(Option<i64>),
}

/// Project a model onto one filterable field, as the gear's promoted columns
/// do: capability fields are the effective values (declared and not
/// disabled), `api_*` the `supported_api` members.
#[allow(clippy::too_many_lines)]
fn project(model: &ModelV1, field: ModelFilterField) -> Scalar {
    use ModelFilterField as F;

    let info = &model.info;
    let caps = &info.capabilities;
    let off = &info.disabled_capabilities;
    let text = |s: &str| Scalar::Text(Some(s.to_owned()));
    let api = |a: SupportedApi| Scalar::Flag(info.supported_api.contains(&a));
    match field {
        F::Id => text(&model.id.to_string()),
        F::TenantId => text(&model.tenant_id.to_string()),
        F::ProviderId => text(&model.provider_id.to_string()),
        F::CanonicalId => text(&model.canonical_id),
        F::ProviderModelId => text(&info.provider_model_id),
        F::GtsType => text(info.gts_type.as_ref()),
        F::SharingMode => text(model.sharing_mode.as_str()),
        F::ApprovalState => text(model.approval.state.as_str()),
        F::ApprovalDecidedAt => text(&model.approval.decided_at.to_rfc3339()),
        F::Vision => Scalar::Flag(caps.vision.enabled && !off.vision.disabled),
        F::ReasoningEffort => Scalar::Flag(caps.reasoning.effort && !off.reasoning.effort),
        F::Tools => Scalar::Flag(caps.tools.enabled && !off.tools.disabled),
        F::JsonSchema => Scalar::Flag(caps.output.json_schema && !off.output.json_schema),
        F::Streaming => Scalar::Flag(caps.streaming && !off.streaming),
        F::FileInput => Scalar::Flag(caps.file_input.enabled && !off.file_input.disabled),
        F::ImageGeneration => {
            Scalar::Flag(caps.image_generation.enabled && !off.image_generation.disabled)
        }
        F::AudioInput => Scalar::Flag(caps.audio_input.enabled && !off.audio_input.disabled),
        F::AudioOutput => Scalar::Flag(caps.audio_output.enabled && !off.audio_output.disabled),
        F::CodeInterpreter => Scalar::Flag(caps.code_interpreter && !off.code_interpreter),
        F::WebSearch => Scalar::Flag(caps.web_search.enabled && !off.web_search.disabled),
        F::ApiCompletion => api(SupportedApi::Completion),
        F::ApiEmbedding => api(SupportedApi::Embedding),
        F::ApiBatch => api(SupportedApi::Batch),
        F::DisplayName => text(&info.display_name),
        F::Family => Scalar::Text(info.family.clone()),
        F::Vendor => Scalar::Text(info.vendor.clone()),
        F::HostedBy => Scalar::Text(info.hosted_by.clone()),
        F::Region => Scalar::Text(info.region.clone()),
        F::Version => Scalar::Text(info.version.clone()),
        F::Managed => Scalar::Flag(info.managed),
        F::Architecture => Scalar::Text(info.architecture.clone()),
        F::Format => Scalar::Text(info.format.clone()),
        F::SortOrder => Scalar::Int(Some(i64::from(info.sort_order))),
        F::LastReleaseAt => Scalar::Text(info.last_release_at.map(|t| t.to_rfc3339())),
        F::MaxInputTokens => Scalar::Int(Some(i64::from(info.context_window.max_input_tokens))),
        F::MaxOutputTokens => Scalar::Int(info.context_window.max_output_tokens.map(i64::from)),
        F::CreatedAt => text(&model.created_at.to_rfc3339()),
        F::UpdatedAt => text(&model.updated_at.to_rfc3339()),
    }
}

/// Evaluate one `$filter` node against a model.
///
/// Covers the subset the demo needs: `and` / `or` / `not` over
/// `<field> eq|ne <literal>`. Anything else is an explicit error rather than a
/// silent mismatch.
fn eval(expr: &Expr, model: &ModelV1) -> Result<bool, CanonicalError> {
    match expr {
        Expr::And(lhs, rhs) => Ok(eval(lhs, model)? && eval(rhs, model)?),
        Expr::Or(lhs, rhs) => Ok(eval(lhs, model)? || eval(rhs, model)?),
        Expr::Not(inner) => Ok(!eval(inner, model)?),
        Expr::Compare(lhs, op, rhs) => compare(lhs, *op, rhs, model),
        _ => Err(invalid_filter(
            "demo mock supports only and/or/not over `field eq|ne literal`",
        )),
    }
}

/// Evaluate a single `<field> <op> <literal>` comparison.
fn compare(
    lhs: &Expr,
    op: CompareOperator,
    rhs: &Expr,
    model: &ModelV1,
) -> Result<bool, CanonicalError> {
    let (Expr::Identifier(name), Expr::Value(literal)) = (lhs, rhs) else {
        return Err(invalid_filter("demo mock expects `<field> <op> <literal>`"));
    };

    // The SDK's allowlist is the gate: an unknown name never reaches the data.
    let field = ModelFilterField::from_name(name)
        .ok_or_else(|| invalid_filter(format!("`{name}` is not a filterable field on models")))?;

    let equal = match (project(model, field), literal) {
        (Scalar::Text(actual), Value::String(want)) => actual.as_deref() == Some(want.as_str()),
        (Scalar::Flag(actual), Value::Bool(want)) => actual == *want,
        (Scalar::Int(actual), Value::Number(want)) => {
            actual.is_some_and(|a| want.to_string() == a.to_string())
        }
        _ => {
            return Err(invalid_filter(format!(
                "literal type does not match the `{name}` field kind"
            )));
        }
    };

    match op {
        CompareOperator::Eq => Ok(equal),
        CompareOperator::Ne => Ok(!equal),
        _ => Err(invalid_filter("demo mock supports only `eq` and `ne`")),
    }
}

/// One `OpenAI` model as it comes back from the registry: an approved,
/// shared model with a `base_url`, a `secret_ref`, and settings in the shape
/// of the demo plugin's provider type.
const OPENAI_FIXTURE: &str = r#"{
  "id": "00000000-0000-0000-0000-000000000010",
  "tenant_id": "11111111-1111-1111-1111-111111111111",
  "provider_id": "00000000-0000-0000-0000-000000000001",
  "canonical_id": "openai::gpt-4o",
  "sharing_mode": "shared",
  "approval": { "state": "approved", "decided_by": "22222222-2222-2222-2222-222222222222", "decided_at": "2026-10-26T12:00:00Z" },
  "info": {
    "gts_type": "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~",
    "provider_model_id": "gpt-4o",
    "base_url": "https://api.openai.com/v1",
    "secret_ref": "platform-openai-key",
    "display_name": "GPT-4o (demo)",
    "description": null,
    "family": "gpt-4o",
    "vendor": "OpenAI",
    "managed": false,
    "architecture": null,
    "size_bytes": null,
    "format": null,
    "region": null,
    "hosted_by": null,
    "last_release_at": null,
    "reasoning_level": null,
    "version": null,
    "sort_order": 1,
    "icon": null,
    "multiplier_display": null,
    "performance": {},
    "additional_info": {},
    "supported_api": ["completion"],
    "capabilities": {
      "vision": { "enabled": true, "supported_mime_types": ["image/png", "image/jpeg"] },
      "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
      "tools": {
        "enabled": true, "parallel_tool_calls": true, "max_tool_calls": true,
        "tool_choice_required": true, "tool_choice_none": true, "tool_choice_named": true
      },
      "output": { "json_mode": true, "json_schema": true, "strict_schema": true, "verbosity": true },
      "sampling": {
        "temperature": true, "top_p": true, "seed": true,
        "presence_penalty": true, "frequency_penalty": true, "logprobs": true
      },
      "streaming": true,
      "file_input": { "enabled": false, "supported_mime_types": [] },
      "image_generation": { "enabled": false, "supported_mime_types": [] },
      "audio_input": { "enabled": false, "supported_mime_types": [] },
      "audio_output": { "enabled": false, "supported_mime_types": [] },
      "code_interpreter": false,
      "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
    },
    "disabled_capabilities": {
      "vision": { "disabled": false, "disabled_mime_types": [] },
      "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
      "streaming": false,
      "file_input": { "disabled": false, "disabled_mime_types": [] },
      "image_generation": { "disabled": false, "disabled_mime_types": [] },
      "audio_input": { "disabled": false, "disabled_mime_types": [] },
      "audio_output": { "disabled": false, "disabled_mime_types": [] },
      "code_interpreter": false,
      "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
    },
    "context_window": { "max_input_tokens": 128000, "max_output_tokens": 16384, "output_vector_size": null },
    "default_parameters": { "temperature": 0.7 },
    "allow_parameter_override": true,
    "allow_extra_params": [],
    "provider_settings": {
      "oagw_alias": "openai-prod",
      "cost": { "input_per_1k_micro": 2500, "output_per_1k_micro": 10000 }
    }
  },
  "created_at": "2026-10-26T12:00:00Z",
  "created_by": "22222222-2222-2222-2222-222222222222",
  "updated_at": "2026-10-26T12:00:00Z",
  "updated_by": "22222222-2222-2222-2222-222222222222"
}"#;

/// A second `OpenAI` model whose approval was revoked: the registry refuses
/// to resolve it and leaves it out of the available list.
const OPENAI_MINI_FIXTURE: &str = r#"{
  "id": "00000000-0000-0000-0000-000000000011",
  "tenant_id": "11111111-1111-1111-1111-111111111111",
  "provider_id": "00000000-0000-0000-0000-000000000001",
  "canonical_id": "openai::gpt-4o-mini",
  "sharing_mode": "shared",
  "approval": { "state": "revoked", "decided_by": "22222222-2222-2222-2222-222222222222", "decided_at": "2026-10-27T09:00:00Z" },
  "info": {
    "gts_type": "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~",
    "provider_model_id": "gpt-4o-mini",
    "base_url": null,
    "secret_ref": "platform-openai-key",
    "display_name": "GPT-4o mini (demo)",
    "description": null,
    "family": "gpt-4o",
    "vendor": "OpenAI",
    "managed": false,
    "architecture": null,
    "size_bytes": null,
    "format": null,
    "region": null,
    "hosted_by": null,
    "last_release_at": null,
    "reasoning_level": null,
    "version": null,
    "sort_order": 2,
    "icon": null,
    "multiplier_display": null,
    "performance": {},
    "additional_info": {},
    "supported_api": ["completion", "batch"],
    "capabilities": {
      "vision": { "enabled": true, "supported_mime_types": ["image/png"] },
      "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
      "tools": {
        "enabled": true, "parallel_tool_calls": true, "max_tool_calls": true,
        "tool_choice_required": true, "tool_choice_none": true, "tool_choice_named": true
      },
      "output": { "json_mode": true, "json_schema": true, "strict_schema": true, "verbosity": true },
      "sampling": {
        "temperature": true, "top_p": true, "seed": true,
        "presence_penalty": true, "frequency_penalty": true, "logprobs": true
      },
      "streaming": true,
      "file_input": { "enabled": false, "supported_mime_types": [] },
      "image_generation": { "enabled": false, "supported_mime_types": [] },
      "audio_input": { "enabled": false, "supported_mime_types": [] },
      "audio_output": { "enabled": false, "supported_mime_types": [] },
      "code_interpreter": false,
      "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
    },
    "disabled_capabilities": {
      "vision": { "disabled": false, "disabled_mime_types": [] },
      "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
      "streaming": false,
      "file_input": { "disabled": false, "disabled_mime_types": [] },
      "image_generation": { "disabled": false, "disabled_mime_types": [] },
      "audio_input": { "disabled": false, "disabled_mime_types": [] },
      "audio_output": { "disabled": false, "disabled_mime_types": [] },
      "code_interpreter": false,
      "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
    },
    "context_window": { "max_input_tokens": 128000, "max_output_tokens": 16384, "output_vector_size": null },
    "default_parameters": {},
    "allow_parameter_override": true,
    "allow_extra_params": [],
    "provider_settings": {
      "oagw_alias": "openai-prod",
      "cost": { "input_per_1k_micro": 150, "output_per_1k_micro": 600 }
    }
  },
  "created_at": "2026-10-26T12:00:00Z",
  "created_by": "22222222-2222-2222-2222-222222222222",
  "updated_at": "2026-10-27T09:00:00Z",
  "updated_by": "22222222-2222-2222-2222-222222222222"
}"#;

/// One `Anthropic` model with a different provider type and settings shape.
/// No plugin is registered for it, so the gateway reports that explicitly.
const ANTHROPIC_FIXTURE: &str = r#"{
  "id": "00000000-0000-0000-0000-000000000020",
  "tenant_id": "11111111-1111-1111-1111-111111111111",
  "provider_id": "00000000-0000-0000-0000-000000000002",
  "canonical_id": "anthropic::claude-sonnet-4",
  "sharing_mode": "shared",
  "approval": { "state": "approved", "decided_by": "22222222-2222-2222-2222-222222222222", "decided_at": "2026-10-26T12:00:00Z" },
  "info": {
    "gts_type": "gts.cf.genai.model.provider.v1~cf.genai._.anthropic.v1~",
    "provider_model_id": "claude-sonnet-4-20250514",
    "base_url": null,
    "secret_ref": "platform-anthropic-key",
    "display_name": "Claude Sonnet 4 (demo)",
    "description": null,
    "family": "claude",
    "vendor": "Anthropic",
    "managed": false,
    "architecture": null,
    "size_bytes": null,
    "format": null,
    "region": null,
    "hosted_by": null,
    "last_release_at": null,
    "reasoning_level": null,
    "version": null,
    "sort_order": 3,
    "icon": null,
    "multiplier_display": null,
    "performance": {},
    "additional_info": {},
    "supported_api": ["completion"],
    "capabilities": {
      "vision": { "enabled": true, "supported_mime_types": ["image/png", "image/jpeg", "image/webp"] },
      "reasoning": { "effort": true, "toggle": false, "resume": false, "budget": true },
      "tools": {
        "enabled": true, "parallel_tool_calls": true, "max_tool_calls": false,
        "tool_choice_required": true, "tool_choice_none": true, "tool_choice_named": true
      },
      "output": { "json_mode": false, "json_schema": true, "strict_schema": true, "verbosity": false },
      "sampling": { "temperature": true, "top_p": true },
      "streaming": true,
      "file_input": { "enabled": true, "supported_mime_types": ["application/pdf"] },
      "image_generation": { "enabled": false, "supported_mime_types": [] },
      "audio_input": { "enabled": false, "supported_mime_types": [] },
      "audio_output": { "enabled": false, "supported_mime_types": [] },
      "code_interpreter": false,
      "web_search": { "enabled": true, "allowed_domains": false, "excluded_domains": false }
    },
    "disabled_capabilities": {
      "vision": { "disabled": false, "disabled_mime_types": [] },
      "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
      "streaming": false,
      "file_input": { "disabled": false, "disabled_mime_types": [] },
      "image_generation": { "disabled": false, "disabled_mime_types": [] },
      "audio_input": { "disabled": false, "disabled_mime_types": [] },
      "audio_output": { "disabled": false, "disabled_mime_types": [] },
      "code_interpreter": false,
      "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
    },
    "context_window": { "max_input_tokens": 200000, "max_output_tokens": 8192, "output_vector_size": null },
    "default_parameters": {},
    "allow_parameter_override": true,
    "allow_extra_params": [],
    "provider_settings": {
      "oagw_alias": "anthropic-prod",
      "anthropic_version": "2023-06-01",
      "cost": { "input_per_1k_micro": 3000, "output_per_1k_micro": 15000 }
    }
  },
  "created_at": "2026-10-26T12:00:00Z",
  "created_by": "22222222-2222-2222-2222-222222222222",
  "updated_at": "2026-10-26T12:00:00Z",
  "updated_by": "22222222-2222-2222-2222-222222222222"
}"#;
