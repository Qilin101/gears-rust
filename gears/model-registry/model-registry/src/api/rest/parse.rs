//! Request body → SDK request conversion, with the REST-only checks of
//! DESIGN §3.3: enum parsing, nested JSON parsing, and the `IMMUTABLE_FIELD`
//! rejection on update bodies.

use std::collections::{HashMap, HashSet};

use model_registry_sdk::{
    ApprovalState, CreateModelRequestV1, CreateProviderRequestV1, SharingMode, SupportedApi,
    UpdateModelRequestV1, UpdateProviderRequestV1, field,
};
use toolkit_canonical_errors::CanonicalError;

use super::dto::{
    CreateModelRequestDto, CreateProviderRequestDto, UpdateModelRequestDto,
    UpdateProviderRequestDto,
};
use crate::domain::error::{DomainError, Violation};

/// Fields no update body may carry (`cpt-cf-model-registry-constraint-immutable-identity`,
/// `canonical_id`, and the audit and decision fields).
pub const PROVIDER_IMMUTABLE_FIELDS: &[&str] = &[
    "id",
    "tenant_id",
    "slug",
    "gts_type",
    "created_at",
    "created_by",
    "updated_at",
    "updated_by",
];

pub const MODEL_IMMUTABLE_FIELDS: &[&str] = &[
    "id",
    "tenant_id",
    "provider_id",
    "provider_model_id",
    "canonical_id",
    "gts_type",
    "approval",
    "approval_decided_by",
    "approval_decided_at",
    "created_at",
    "created_by",
    "updated_at",
    "updated_by",
];

fn invalid(field: &str, description: impl Into<String>, reason: field::Reason) -> CanonicalError {
    DomainError::invalid(Violation::new(field, description, reason)).into()
}

/// Rejects an update body that names an immutable field, before it is parsed.
pub fn reject_immutable(
    body: &serde_json::Value,
    immutable: &[&str],
) -> Result<(), CanonicalError> {
    let Some(map) = body.as_object() else {
        return Err(invalid(
            "body",
            "must be a JSON object",
            field::Reason::InvalidFormat,
        ));
    };
    let violations: Vec<Violation> = immutable
        .iter()
        .filter(|f| map.contains_key(**f))
        .map(|f| Violation::new(*f, "field is immutable", field::Reason::ImmutableField))
        .collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(DomainError::Invalid(violations).into())
    }
}

/// Parses a body into a DTO; a shape error is `INVALID_FORMAT` on `body`.
pub fn body<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, CanonicalError> {
    serde_json::from_value(value)
        .map_err(|e| invalid("body", e.to_string(), field::Reason::InvalidFormat))
}

fn sharing_mode(raw: &str) -> Result<SharingMode, CanonicalError> {
    SharingMode::from_wire(raw).ok_or_else(|| {
        invalid(
            "sharing_mode",
            format!("unknown value `{raw}`, expected one of: tenant, shared"),
            field::Reason::InvalidFormat,
        )
    })
}

fn approval_state(raw: &str) -> Result<ApprovalState, CanonicalError> {
    ApprovalState::from_wire(raw).ok_or_else(|| {
        invalid(
            "approval_state",
            format!("unknown value `{raw}`, expected one of: pending, approved, rejected, revoked"),
            field::Reason::InvalidFormat,
        )
    })
}

fn supported_api(raw: &[String]) -> Result<HashSet<SupportedApi>, CanonicalError> {
    raw.iter()
        .map(|s| {
            SupportedApi::from_wire(s).ok_or_else(|| {
                invalid(
                    "supported_api",
                    format!("unknown value `{s}`, expected one of: completion, embedding, batch"),
                    field::Reason::InvalidFormat,
                )
            })
        })
        .collect()
}

fn nested<T: serde::de::DeserializeOwned>(
    field_name: &str,
    value: serde_json::Value,
) -> Result<T, CanonicalError> {
    serde_json::from_value(value)
        .map_err(|e| invalid(field_name, e.to_string(), field::Reason::InvalidFormat))
}

fn nested_or_default<T: serde::de::DeserializeOwned + Default>(
    field_name: &str,
    value: Option<serde_json::Value>,
) -> Result<T, CanonicalError> {
    value.map_or_else(|| Ok(T::default()), |v| nested(field_name, v))
}

pub fn create_provider(
    dto: CreateProviderRequestDto,
) -> Result<CreateProviderRequestV1, CanonicalError> {
    let mut b =
        CreateProviderRequestV1::builder(dto.slug, dto.name, gts::GtsTypeId::new(&dto.gts_type));
    if let Some(enabled) = dto.enabled {
        b = b.enabled(enabled);
    }
    if let Some(mode) = dto.sharing_mode.as_deref() {
        b = b.sharing_mode(sharing_mode(mode)?);
    }
    if let Some(managed) = dto.managed {
        b = b.managed(managed);
    }
    if let Some(metadata) = dto.metadata {
        b = b.metadata(metadata);
    }
    Ok(b.build())
}

pub fn update_provider(
    dto: UpdateProviderRequestDto,
) -> Result<UpdateProviderRequestV1, CanonicalError> {
    Ok(UpdateProviderRequestV1 {
        name: dto.name,
        enabled: dto.enabled,
        sharing_mode: dto.sharing_mode.as_deref().map(sharing_mode).transpose()?,
        managed: dto.managed,
        metadata: dto.metadata,
    })
}

pub fn create_model(dto: CreateModelRequestDto) -> Result<CreateModelRequestV1, CanonicalError> {
    Ok(CreateModelRequestV1 {
        provider_id: dto.provider_id,
        sharing_mode: dto.sharing_mode.as_deref().map(sharing_mode).transpose()?,
        approval_state: dto
            .approval_state
            .as_deref()
            .map(approval_state)
            .transpose()?,
        provider_model_id: dto.provider_model_id,
        base_url: dto.base_url,
        secret_ref: dto.secret_ref,
        display_name: dto.display_name,
        description: dto.description,
        family: dto.family,
        vendor: dto.vendor,
        managed: dto.managed.unwrap_or(false),
        architecture: dto.architecture,
        size_bytes: dto.size_bytes,
        format: dto.format,
        region: dto.region,
        hosted_by: dto.hosted_by,
        last_release_at: dto.last_release_at,
        reasoning_level: dto.reasoning_level,
        version: dto.version,
        sort_order: dto.sort_order.unwrap_or(0),
        icon: dto.icon,
        multiplier_display: dto.multiplier_display,
        performance: nested_or_default("performance", dto.performance)?,
        additional_info: nested_or_default::<HashMap<String, serde_json::Value>>(
            "additional_info",
            dto.additional_info,
        )?,
        supported_api: dto
            .supported_api
            .as_deref()
            .map(supported_api)
            .transpose()?
            .unwrap_or_default(),
        capabilities: nested_or_default("capabilities", dto.capabilities)?,
        disabled_capabilities: nested_or_default(
            "disabled_capabilities",
            dto.disabled_capabilities,
        )?,
        context_window: nested("context_window", dto.context_window)?,
        default_parameters: nested_or_default("default_parameters", dto.default_parameters)?,
        allow_parameter_override: dto.allow_parameter_override.unwrap_or(false),
        allow_extra_params: dto.allow_extra_params.unwrap_or_default(),
        provider_settings: dto
            .provider_settings
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
    })
}

pub fn update_model(dto: UpdateModelRequestDto) -> Result<UpdateModelRequestV1, CanonicalError> {
    Ok(UpdateModelRequestV1 {
        sharing_mode: dto.sharing_mode.as_deref().map(sharing_mode).transpose()?,
        approval_state: dto
            .approval_state
            .as_deref()
            .map(approval_state)
            .transpose()?,
        base_url: dto.base_url,
        secret_ref: dto.secret_ref,
        display_name: dto.display_name,
        description: dto.description,
        family: dto.family,
        vendor: dto.vendor,
        managed: dto.managed,
        architecture: dto.architecture,
        size_bytes: dto.size_bytes,
        format: dto.format,
        region: dto.region,
        hosted_by: dto.hosted_by,
        last_release_at: dto.last_release_at,
        reasoning_level: dto.reasoning_level,
        version: dto.version,
        sort_order: dto.sort_order,
        icon: dto.icon,
        multiplier_display: dto.multiplier_display,
        performance: dto
            .performance
            .map(|v| nested("performance", v))
            .transpose()?,
        additional_info: dto
            .additional_info
            .map(|v| nested("additional_info", v))
            .transpose()?,
        supported_api: dto
            .supported_api
            .as_deref()
            .map(supported_api)
            .transpose()?,
        capabilities: dto
            .capabilities
            .map(|v| nested("capabilities", v))
            .transpose()?,
        disabled_capabilities: dto
            .disabled_capabilities
            .map(|v| nested("disabled_capabilities", v))
            .transpose()?,
        context_window: dto
            .context_window
            .map(|v| nested("context_window", v))
            .transpose()?,
        default_parameters: dto
            .default_parameters
            .map(|v| nested("default_parameters", v))
            .transpose()?,
        allow_parameter_override: dto.allow_parameter_override,
        allow_extra_params: dto.allow_extra_params,
        provider_settings: dto.provider_settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use toolkit_canonical_errors::InvalidArgument;

    fn violations(err: &CanonicalError) -> Vec<(String, String)> {
        match err {
            CanonicalError::InvalidArgument {
                ctx: InvalidArgument::FieldViolations { field_violations },
                ..
            } => field_violations
                .iter()
                .map(|v| (v.field.clone(), v.reason.clone()))
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn immutable_fields_are_rejected_with_the_sdk_reason() {
        let err = reject_immutable(
            &json!({ "name": "x", "slug": "y", "created_at": "z" }),
            PROVIDER_IMMUTABLE_FIELDS,
        )
        .unwrap_err();
        assert_eq!(
            violations(&err),
            vec![
                ("slug".to_owned(), field::IMMUTABLE_FIELD.to_owned()),
                ("created_at".to_owned(), field::IMMUTABLE_FIELD.to_owned())
            ]
        );
        reject_immutable(&json!({ "name": "x" }), PROVIDER_IMMUTABLE_FIELDS).unwrap();
        assert!(reject_immutable(&json!([]), MODEL_IMMUTABLE_FIELDS).is_err());
    }

    #[test]
    fn update_model_body_distinguishes_absent_null_and_value() {
        let dto: UpdateModelRequestDto =
            body(json!({ "base_url": null, "description": "d", "approval_state": "revoked" }))
                .unwrap();
        let req = update_model(dto).unwrap();
        assert_eq!(req.base_url, Some(None));
        assert_eq!(req.description, Some(Some("d".into())));
        assert_eq!(req.secret_ref, None);
        assert_eq!(req.approval_state, Some(ApprovalState::Revoked));
    }

    #[test]
    fn enums_and_nested_objects_are_validated() {
        let dto: UpdateModelRequestDto = body(json!({ "sharing_mode": "subtree" })).unwrap();
        let err = update_model(dto).unwrap_err();
        assert_eq!(
            violations(&err)[0],
            ("sharing_mode".to_owned(), field::INVALID_FORMAT.to_owned())
        );

        let dto: CreateModelRequestDto = body(json!({
            "provider_id": uuid::Uuid::nil(),
            "provider_model_id": "gpt",
            "display_name": "GPT",
            "context_window": { "max_input_tokens": "many" },
            "supported_api": ["completion"]
        }))
        .unwrap();
        let err = create_model(dto).unwrap_err();
        assert_eq!(violations(&err)[0].0, "context_window");

        let dto: CreateModelRequestDto = body(json!({
            "provider_id": uuid::Uuid::nil(),
            "provider_model_id": "gpt",
            "display_name": "GPT",
            "context_window": { "max_input_tokens": 8 },
            "supported_api": ["completion", "batch"],
            "performance": { "tokens_per_second": 90 }
        }))
        .unwrap();
        let req = create_model(dto).unwrap();
        assert_eq!(req.supported_api.len(), 2);
        assert_eq!(req.performance.tokens_per_second, Some(90));
        assert!(!req.capabilities.streaming);
        assert_eq!(req.provider_settings, json!({}));
        assert!(!req.managed);
    }

    #[test]
    fn provider_bodies_parse() {
        let dto: CreateProviderRequestDto = body(json!({
            "slug": "openai", "name": "OpenAI", "gts_type": "gts.x~", "sharing_mode": "shared"
        }))
        .unwrap();
        let req = create_provider(dto).unwrap();
        assert_eq!(req.sharing_mode(), SharingMode::Shared);
        assert!(req.enabled());

        let dto: UpdateProviderRequestDto =
            body(json!({ "metadata": null, "enabled": false })).unwrap();
        let req = update_provider(dto).unwrap();
        assert_eq!(req.metadata, Some(None));
        assert_eq!(req.enabled, Some(false));
    }
}
