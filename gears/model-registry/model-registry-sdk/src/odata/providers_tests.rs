// Created: 2026-08-06 by Constructor Tech
use super::*;
use toolkit_odata::filter::{FieldKind, FilterField};

#[test]
fn provider_field_names_match_expected() {
    let expected: Vec<&str> = vec![
        "slug",
        "name",
        "status",
        "gts_type",
        "managed",
        "discovery_enabled",
    ];
    let actual: Vec<&str> = ProviderFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .collect();
    assert_eq!(
        actual, expected,
        "provider filter field names must match the API contract"
    );
}

#[test]
fn provider_field_kinds_are_correct() {
    for field in ProviderFilterField::FIELDS {
        match field {
            ProviderFilterField::Managed | ProviderFilterField::DiscoveryEnabled => {
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
fn provider_from_name_resolves() {
    assert_eq!(
        ProviderFilterField::from_name("slug"),
        Some(ProviderFilterField::Slug)
    );
    assert_eq!(
        ProviderFilterField::from_name("discovery_enabled"),
        Some(ProviderFilterField::DiscoveryEnabled)
    );
}

#[test]
fn provider_rejects_non_allowlisted_fields() {
    assert_eq!(ProviderFilterField::from_name("metadata"), None);
    assert_eq!(
        ProviderFilterField::from_name("discovery_interval_seconds"),
        None
    );
    assert_eq!(ProviderFilterField::from_name("unknown_field"), None);
}
