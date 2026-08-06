//! Mock [`ModelRegistryClientV1`] holding a set of pre-built models keyed by
//! `canonical_id`.
//!
//! Currently ships two models -- `openai::gpt-4o` and `anthropic::claude-sonnet-4`
//! -- with different provider types so the demo can exercise both the success
//! path and the "no provider plugin registered" path.
//!
//! `get_tenant_model` and `list_tenant_models` are implemented; the remaining
//! trait methods are stubbed. `list_tenant_models` evaluates the caller's
//! `$filter` AST in memory against the fixtures, resolving each identifier
//! through [`ModelFilterField::from_name`] — the same allowlist lookup the real
//! gear performs before mapping a field to a `models` column.

use std::collections::HashMap;

use async_trait::async_trait;
use model_registry_sdk::odata::ModelFilterField;
use model_registry_sdk::{
    CreateModelRequestV1, CreateProviderRequestV1, ModelRegistryClientV1, ModelRegistryError,
    ModelV1, ProviderV1, UpdateModelRequestV1, UpdateProviderRequestV1,
};
use toolkit_odata::ast::{CompareOperator, Expr, Value};
use toolkit_odata::filter::FilterField as _;
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Page size applied when the caller sets no `limit`, matching the gear's own
/// default.
const DEFAULT_PAGE_SIZE: u64 = 20;

/// Provider identity of the `OpenAI` model fixture.
pub const OPENAI_PROVIDER_TYPE: &str = "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~";

/// Provider identity of the `Anthropic` model fixture.
pub const ANTHROPIC_PROVIDER_TYPE: &str = "gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~";

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
        let openai: ModelV1 = serde_json::from_str(OPENAI_FIXTURE)?;
        let anthropic: ModelV1 = serde_json::from_str(ANTHROPIC_FIXTURE)?;
        let mut by_id = HashMap::new();
        by_id.insert(openai.canonical_id.clone(), openai);
        by_id.insert(anthropic.canonical_id.clone(), anthropic);
        Ok(Self { by_id })
    }
}

#[async_trait]
impl ModelRegistryClientV1 for MockModelRegistry {
    async fn get_tenant_model(
        &self,
        _ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, ModelRegistryError> {
        self.by_id
            .get(canonical_id)
            .cloned()
            .ok_or_else(|| ModelRegistryError::ModelNotFound {
                canonical_id: canonical_id.to_owned(),
            })
    }

    async fn list_tenant_models(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, ModelRegistryError> {
        // Sorted by `canonical_id` so demo output is stable — the same default
        // tiebreaker the real repository paginates on.
        let mut candidates: Vec<&ModelV1> = self.by_id.values().collect();
        candidates.sort_by(|a, b| a.canonical_id.cmp(&b.canonical_id));

        let mut items = Vec::new();
        for model in candidates {
            let keep = match query.filter() {
                Some(expr) => eval(expr, model)?,
                None => true,
            };
            if keep {
                items.push(model.clone());
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

    async fn create_model(
        &self,
        _ctx: &SecurityContext,
        _req: CreateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError> {
        Err(unsupported())
    }

    async fn update_model(
        &self,
        _ctx: &SecurityContext,
        _canonical_id: &str,
        _req: UpdateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError> {
        Err(unsupported())
    }

    async fn delete_model(
        &self,
        _ctx: &SecurityContext,
        _canonical_id: &str,
    ) -> Result<(), ModelRegistryError> {
        Err(unsupported())
    }

    async fn get_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ProviderV1, ModelRegistryError> {
        Err(unsupported())
    }

    async fn list_providers(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, ModelRegistryError> {
        Err(unsupported())
    }

    async fn create_provider(
        &self,
        _ctx: &SecurityContext,
        _req: CreateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError> {
        Err(unsupported())
    }

    async fn update_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _req: UpdateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError> {
        Err(unsupported())
    }

    async fn delete_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), ModelRegistryError> {
        Err(unsupported())
    }
}

fn unsupported() -> ModelRegistryError {
    ModelRegistryError::Validation {
        message: "not implemented in demo".to_owned(),
    }
}

// ===========================================================================
// In-memory `$filter` evaluation
// ===========================================================================

/// The scalar one filter field resolves to for a given model.
///
/// Mirrors the real gear's column projection: the 15 fields the repository
/// binds to `models` columns, read straight off [`ModelV1`] instead.
enum Scalar {
    Text(Option<String>),
    Flag(bool),
}

/// Project a model onto one filterable field.
fn project(model: &ModelV1, field: ModelFilterField) -> Scalar {
    use ModelFilterField as F;

    let caps = &model.info.capabilities;
    match field {
        F::CanonicalId => Scalar::Text(Some(model.canonical_id.clone())),
        F::LifecycleStatus => Scalar::Text(Some(model.lifecycle_status.as_str().to_owned())),
        F::ApprovalStatus => Scalar::Text(Some(model.approval_status.as_str().to_owned())),
        F::GtsType => Scalar::Text(Some(model.info.gts_type.to_string())),
        F::SupportedApi => {
            // Sorted comma-joined shadow of the API set, as the gear stores it.
            let mut apis: Vec<&str> = model
                .info
                .supported_api
                .iter()
                .map(|a| a.as_str())
                .collect();
            apis.sort_unstable();
            Scalar::Text(Some(apis.join(",")))
        }
        F::ProviderModelId => Scalar::Text(Some(model.info.provider_model_id.clone())),
        F::Vendor => Scalar::Text(model.info.vendor.clone()),
        F::Family => Scalar::Text(model.info.family.clone()),
        F::Managed => Scalar::Flag(model.info.managed),
        F::Architecture => Scalar::Text(model.info.architecture.clone()),
        F::Format => Scalar::Text(model.info.format.clone()),
        F::Vision => Scalar::Flag(caps.vision.enabled),
        F::FunctionCalling => Scalar::Flag(caps.function_calling),
        F::Streaming => Scalar::Flag(caps.streaming),
        F::ReasoningEffort => Scalar::Flag(caps.reasoning.effort),
    }
}

/// Evaluate one `$filter` node against a model.
///
/// Covers the subset the demo needs: `and` / `or` / `not` over
/// `<field> eq|ne <literal>`. Anything else is an explicit error rather than a
/// silent mismatch.
fn eval(expr: &Expr, model: &ModelV1) -> Result<bool, ModelRegistryError> {
    match expr {
        Expr::And(lhs, rhs) => Ok(eval(lhs, model)? && eval(rhs, model)?),
        Expr::Or(lhs, rhs) => Ok(eval(lhs, model)? || eval(rhs, model)?),
        Expr::Not(inner) => Ok(!eval(inner, model)?),
        Expr::Compare(lhs, op, rhs) => compare(lhs, *op, rhs, model),
        _ => Err(ModelRegistryError::Validation {
            message: "demo mock supports only and/or/not over `field eq|ne literal`".to_owned(),
        }),
    }
}

/// Evaluate a single `<field> <op> <literal>` comparison.
fn compare(
    lhs: &Expr,
    op: CompareOperator,
    rhs: &Expr,
    model: &ModelV1,
) -> Result<bool, ModelRegistryError> {
    let (Expr::Identifier(name), Expr::Value(literal)) = (lhs, rhs) else {
        return Err(ModelRegistryError::Validation {
            message: "demo mock expects `<field> <op> <literal>`".to_owned(),
        });
    };

    // The SDK's allowlist is the gate: an unknown name never reaches the data.
    let field =
        ModelFilterField::from_name(name).ok_or_else(|| ModelRegistryError::Validation {
            message: format!("`{name}` is not a filterable field on models"),
        })?;

    let equal = match (project(model, field), literal) {
        (Scalar::Text(actual), Value::String(want)) => actual.as_deref() == Some(want.as_str()),
        (Scalar::Flag(actual), Value::Bool(want)) => actual == *want,
        _ => {
            return Err(ModelRegistryError::Validation {
                message: format!("literal type does not match the `{name}` field kind"),
            });
        }
    };

    match op {
        CompareOperator::Eq => Ok(equal),
        CompareOperator::Ne => Ok(!equal),
        _ => Err(ModelRegistryError::Validation {
            message: "demo mock supports only `eq` and `ne`".to_owned(),
        }),
    }
}

/// One `OpenAI` model as it would arrive from the registry's JSONB row.
const OPENAI_FIXTURE: &str = r#"{
  "id": "00000000-0000-0000-0000-000000000000",
  "canonical_id": "openai::gpt-4o",
  "lifecycle_status": "production",
  "approval_status": "approved",
  "info": {
    "gts_type": "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~",
    "display_name": "GPT-4o (demo)",
    "managed": false,
    "performance": {},
    "additional_info": {},
    "supported_api": ["completion"],
    "provider_model_id": "gpt-4o",
    "capabilities": {
      "vision": { "enabled": true, "supported_mime_types": ["image/png", "image/jpeg"] },
      "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
      "function_calling": true,
      "response_schema": true,
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
      "function_calling": false,
      "response_schema": false,
      "streaming": false,
      "file_input": { "disabled": false, "disabled_mime_types": [] },
      "image_generation": { "disabled": false, "disabled_mime_types": [] },
      "audio_input": { "disabled": false, "disabled_mime_types": [] },
      "audio_output": { "disabled": false, "disabled_mime_types": [] },
      "code_interpreter": false,
      "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
    },
    "context_window": { "max_input_tokens": 128000, "max_output_tokens": 16384 },
    "default_parameters": {},
    "allow_parameter_override": true,
    "allow_extra_params": [],
    "provider_settings": {
      "oagw_alias": "openai-prod",
      "endpoint_kind": "responses",
      "organization": null,
      "project": null,
      "temperature": 0.7,
      "top_p": null,
      "presence_penalty": null,
      "frequency_penalty": null,
      "top_logprobs": null,
      "service_tier": null,
      "prompt_cache_retention": null,
      "reasoning_effort": null,
      "reasoning_summary": null,
      "verbosity": null,
      "parallel_tool_calls": null,
      "store": null,
      "response_format": null,
      "max_tokens": 4096,
      "max_completion_tokens": null,
      "n": null,
      "stop": null,
      "seed": null,
      "logprobs": null,
      "max_output_tokens": null,
      "max_tool_calls": null,
      "truncation": null,
      "encoding_format": null,
      "dimensions": null,
      "cost": {
        "input_per_1k_micro": null,
        "cached_input_per_1k_micro": null,
        "output_per_1k_micro": null,
        "long_context_input_per_1k_micro": null,
        "long_context_cached_input_per_1k_micro": null,
        "long_context_output_per_1k_micro": null,
        "long_context_threshold_tokens": null,
        "web_search_per_1k_calls_micro": null,
        "file_search_per_1k_calls_micro": null
      }
    }
  }
}"#;

/// One `Anthropic` model fixture with a different provider type and settings
/// shape. No plugin is expected to serve it, so resolving it should surface
/// an explicit "no provider plugin" error from the gateway.
const ANTHROPIC_FIXTURE: &str = r#"{
  "id": "00000000-0000-0000-0000-000000000001",
  "canonical_id": "anthropic::claude-sonnet-4",
  "lifecycle_status": "production",
  "approval_status": "approved",
  "info": {
    "gts_type": "gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~",
    "display_name": "Claude Sonnet 4 (demo)",
    "managed": false,
    "performance": {},
    "additional_info": {},
    "supported_api": ["completion"],
    "provider_model_id": "claude-sonnet-4-20250514",
    "capabilities": {
      "vision": { "enabled": true, "supported_mime_types": ["image/png", "image/jpeg", "image/webp"] },
      "reasoning": { "effort": true, "toggle": false, "resume": false, "budget": true },
      "function_calling": true,
      "response_schema": true,
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
      "function_calling": false,
      "response_schema": false,
      "streaming": false,
      "file_input": { "disabled": false, "disabled_mime_types": [] },
      "image_generation": { "disabled": false, "disabled_mime_types": [] },
      "audio_input": { "disabled": false, "disabled_mime_types": [] },
      "audio_output": { "disabled": false, "disabled_mime_types": [] },
      "code_interpreter": false,
      "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
    },
    "context_window": { "max_input_tokens": 200000, "max_output_tokens": 8192 },
    "default_parameters": {},
    "allow_parameter_override": true,
    "allow_extra_params": [],
    "provider_settings": {
      "oagw_alias": "anthropic-prod",
      "anthropic_version": "2023-06-01",
      "anthropic_beta": [],
      "temperature": 0.7,
      "top_p": null,
      "top_k": null,
      "max_tokens": 8192,
      "stop_sequences": null,
      "system": null,
      "inference_geo": null,
      "service_tier": null,
      "container": null,
      "thinking": null,
      "tool_choice": null,
      "output_config": null,
      "cost": {
        "input_per_1k_micro": null,
        "output_per_1k_micro": null,
        "cache_creation_5m_per_1k_micro": null,
        "cache_creation_1h_per_1k_micro": null,
        "cache_read_per_1k_micro": null,
        "web_search_per_1k_calls_micro": null
      }
    }
  }
}"#;
