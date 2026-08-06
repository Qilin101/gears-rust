// Created: 2026-08-06 by Constructor Tech
use super::*;
use toolkit_odata::filter::{FieldKind, FilterField};

/// Returns all filterable field names as expected by `OData` API consumers.
fn expected_model_field_names() -> Vec<&'static str> {
    vec![
        "canonical_id",
        "lifecycle_status",
        "approval_status",
        "gts_type",
        "supported_api",
        "provider_model_id",
        "vendor",
        "family",
        "managed",
        "architecture",
        "format",
        "vision",
        "function_calling",
        "streaming",
        "reasoning_effort",
    ]
}

#[test]
fn model_field_names_match_expected() {
    let expected = expected_model_field_names();
    let actual: Vec<&str> = ModelFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .collect();
    assert_eq!(
        actual, expected,
        "model filter field names must match the API contract"
    );
}

#[test]
fn model_field_kinds_are_correct() {
    for field in ModelFilterField::FIELDS {
        match field {
            ModelFilterField::Managed
            | ModelFilterField::Vision
            | ModelFilterField::FunctionCalling
            | ModelFilterField::Streaming
            | ModelFilterField::ReasoningEffort => {
                assert_eq!(
                    field.kind(),
                    FieldKind::Bool,
                    "field {field:?} should be Bool"
                );
            }
            _ => {
                assert_eq!(
                    field.kind(),
                    FieldKind::String,
                    "field {field:?} should be String"
                );
            }
        }
    }
}

#[test]
fn model_from_name_resolves_exact_match() {
    assert_eq!(
        ModelFilterField::from_name("lifecycle_status"),
        Some(ModelFilterField::LifecycleStatus)
    );
    assert_eq!(
        ModelFilterField::from_name("canonical_id"),
        Some(ModelFilterField::CanonicalId)
    );
    assert_eq!(
        ModelFilterField::from_name("approval_status"),
        Some(ModelFilterField::ApprovalStatus)
    );
    assert_eq!(
        ModelFilterField::from_name("vision"),
        Some(ModelFilterField::Vision)
    );
    assert_eq!(
        ModelFilterField::from_name("reasoning_effort"),
        Some(ModelFilterField::ReasoningEffort)
    );
}

#[test]
fn model_from_name_is_case_insensitive() {
    assert_eq!(
        ModelFilterField::from_name("LIFECYCLE_STATUS"),
        Some(ModelFilterField::LifecycleStatus)
    );
    assert_eq!(
        ModelFilterField::from_name("Gts_Type"),
        Some(ModelFilterField::GtsType)
    );
}

#[test]
fn model_rejects_non_allowlisted_fields() {
    // These fields should NOT be filterable per DESIGN §3.3
    assert_eq!(ModelFilterField::from_name("provider_settings"), None);
    assert_eq!(ModelFilterField::from_name("default_parameters"), None);
    assert_eq!(ModelFilterField::from_name("additional_info"), None);
    assert_eq!(ModelFilterField::from_name("context_window"), None);
    assert_eq!(ModelFilterField::from_name("cost"), None);
    assert_eq!(ModelFilterField::from_name("nonexistent_field"), None);
}

#[test]
fn model_rejects_jsonb_path_style_fields() {
    // `OData` layer maps to real columns, not JSONB paths
    assert_eq!(ModelFilterField::from_name("info.gts_type"), None);
    assert_eq!(ModelFilterField::from_name("info.supported_api"), None);
    assert_eq!(
        ModelFilterField::from_name("info.capabilities.vision"),
        None
    );
    assert_eq!(ModelFilterField::from_name("info.vendor"), None);
}
