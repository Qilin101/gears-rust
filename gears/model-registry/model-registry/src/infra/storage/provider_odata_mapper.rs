//! Binds the SDK's `providers` `OData` filter allowlist to real `SeaORM` columns.
//!
//! The filterable wire surface itself — [`ProviderFilterField`] — is public
//! contract and lives in [`model_registry_sdk::odata`]. What stays here is the
//! column binding, which names storage types the SDK does not expose, plus the
//! cursor-value extraction that keyset pagination needs. The model-side binding
//! lives in [`super::model_odata_mapper`].
//!
//! Every field maps to exactly one real column: the toolkit `OData` layer
//! (`toolkit-db` / `sea_orm_filter`) supports neither JSONB-path filtering nor
//! joins — so nothing inside the `metadata` blob is filterable. Non-allowlisted
//! fields never reach this module — they are rejected by
//! [`toolkit_odata::filter::FilterField::from_name`] (returns `None`), which the
//! `OData` parser surfaces as an unknown-field validation error.

use model_registry_sdk::odata::ProviderFilterField;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};

use super::entity::provider;

// ===========================================================================
// Provider filter fields
// ===========================================================================

/// Maps [`ProviderFilterField`] to `providers` columns and extracts cursor values.
pub struct ProviderODataMapper;

impl FieldToColumn<ProviderFilterField> for ProviderODataMapper {
    type Column = provider::Column;

    fn map_field(field: ProviderFilterField) -> provider::Column {
        match field {
            ProviderFilterField::Slug => provider::Column::Slug,
            ProviderFilterField::Name => provider::Column::Name,
            ProviderFilterField::Status => provider::Column::Status,
            ProviderFilterField::GtsType => provider::Column::GtsType,
            ProviderFilterField::Managed => provider::Column::Managed,
            ProviderFilterField::DiscoveryEnabled => provider::Column::DiscoveryEnabled,
        }
    }
}

impl ODataFieldMapping<ProviderFilterField> for ProviderODataMapper {
    type Entity = provider::Entity;

    fn extract_cursor_value(
        m: &<Self::Entity as sea_orm::EntityTrait>::Model,
        field: ProviderFilterField,
    ) -> sea_orm::Value {
        match field {
            ProviderFilterField::Slug => sea_orm::Value::String(Some(m.slug.clone())),
            ProviderFilterField::Name => sea_orm::Value::String(Some(m.name.clone())),
            ProviderFilterField::Status => sea_orm::Value::String(Some(m.status.clone())),
            ProviderFilterField::GtsType => sea_orm::Value::String(Some(m.gts_type.clone())),
            ProviderFilterField::Managed => sea_orm::Value::Bool(Some(m.managed)),
            ProviderFilterField::DiscoveryEnabled => {
                sea_orm::Value::Bool(Some(m.discovery_enabled))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    // =======================================================================
    // Provider column binding
    // =======================================================================

    #[test]
    fn provider_field_map_field_returns_correct_column() {
        use sea_orm::Iden;
        fn assert_col(f: ProviderFilterField, expected: &str) {
            let actual = ProviderODataMapper::map_field(f);
            assert_eq!(
                actual.to_string(),
                expected,
                "field {f:?} should map to column {expected}"
            );
        }

        assert_col(ProviderFilterField::Slug, "slug");
        assert_col(ProviderFilterField::Name, "name");
        assert_col(ProviderFilterField::Status, "status");
        assert_col(ProviderFilterField::GtsType, "gts_type");
        assert_col(ProviderFilterField::Managed, "managed");
        assert_col(ProviderFilterField::DiscoveryEnabled, "discovery_enabled");
    }

    #[test]
    fn provider_extract_cursor_value_round_trip() {
        let now = Utc::now();
        let m = provider::Model {
            id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            slug: "openai".to_owned(),
            name: "OpenAI".to_owned(),
            gts_type: "gts.cf.genai._.openai.v1~".to_owned(),
            status: "active".to_owned(),
            managed: true,
            metadata: None,
            discovery_enabled: false,
            discovery_interval_seconds: None,
            created_at: now,
            updated_at: now,
        };

        assert_eq!(
            ProviderODataMapper::extract_cursor_value(&m, ProviderFilterField::Slug),
            sea_orm::Value::String(Some("openai".to_owned()))
        );
        assert_eq!(
            ProviderODataMapper::extract_cursor_value(&m, ProviderFilterField::Managed),
            sea_orm::Value::Bool(Some(true))
        );
        assert_eq!(
            ProviderODataMapper::extract_cursor_value(&m, ProviderFilterField::DiscoveryEnabled),
            sea_orm::Value::Bool(Some(false))
        );
    }
}
