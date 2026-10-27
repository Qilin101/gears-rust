//! GTS checks on writes (DESIGN §4 GTS Checks): provider type checks on
//! provider create, `provider_settings` validation on model create and
//! settings update. Not called on the resolution path.

use async_trait::async_trait;
use model_registry_sdk::field;
use model_registry_sdk::gts::PROVIDER_BASE_TYPE;
use toolkit_security::SecurityContext;

use super::error::{DomainError, Violation};

/// Where resolved provider type schemas come from (Types Registry in
/// production, a map in tests).
#[async_trait]
pub trait SchemaSource: Send + Sync {
    /// The resolved schema (parents merged) of `type_id`, or `None` when the
    /// registry has no active schema for it.
    async fn resolved_schema(
        &self,
        ctx: &SecurityContext,
        type_id: &gts::GtsTypeId,
    ) -> Result<Option<serde_json::Value>, DomainError>;
}

/// Provider create checks 1 and 2: the id parses as a GTS type id, and it is
/// derived from the provider base type.
pub fn check_provider_type(gts_type: &gts::GtsTypeId) -> Result<(), DomainError> {
    let raw = gts_type.as_ref();
    if let Err(e) = gts::GtsTypeId::try_new(raw) {
        return Err(DomainError::invalid(Violation::new(
            "gts_type",
            format!("not a GTS type id: {e}"),
            field::Reason::InvalidGtsType,
        )));
    }
    let derived = raw
        .strip_prefix(PROVIDER_BASE_TYPE)
        .is_some_and(|rest| !rest.is_empty() && rest.ends_with('~'));
    if !derived {
        return Err(DomainError::invalid(Violation::new(
            "gts_type",
            format!("must be derived from the provider base type {PROVIDER_BASE_TYPE}"),
            field::Reason::NotAProviderType,
        )));
    }
    Ok(())
}

/// Fetches the resolved schema of a provider type for a write; `field` names
/// the request field reported when the type is not registered.
pub async fn provider_type_schema(
    source: &dyn SchemaSource,
    ctx: &SecurityContext,
    gts_type: &gts::GtsTypeId,
    field_name: &'static str,
) -> Result<serde_json::Value, DomainError> {
    source.resolved_schema(ctx, gts_type).await?.ok_or_else(|| {
        DomainError::invalid(Violation::new(
            field_name,
            format!("provider type {gts_type} is not registered"),
            field::Reason::GtsTypeNotRegistered,
        ))
    })
}

/// Every `properties.provider_settings` subschema of the resolved schema:
/// at the root and inside each `allOf` branch (the parent chain is inlined
/// there).
fn settings_subschemas(schema: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    if let Some(sub) = schema.pointer("/properties/provider_settings") {
        out.push(sub.clone());
    }
    if let Some(branches) = schema.get("allOf").and_then(|v| v.as_array()) {
        for branch in branches {
            out.extend(settings_subschemas(branch));
        }
    }
    out
}

/// Validates `provider_settings` against the resolved provider type schema
/// (JSON Schema Draft-07). A schema that does not constrain
/// `provider_settings` accepts any JSON object.
pub fn validate_settings(
    resolved_schema: &serde_json::Value,
    settings: &serde_json::Value,
) -> Result<(), DomainError> {
    if !settings.is_object() {
        return Err(DomainError::invalid(Violation::new(
            "provider_settings",
            "must be a JSON object",
            field::Reason::SchemaMismatch,
        )));
    }
    let subschemas = settings_subschemas(resolved_schema);
    if subschemas.is_empty() {
        return Ok(());
    }
    let combined = serde_json::json!({ "allOf": subschemas });
    // The draft is pinned: a GTS chain id in `$schema` is not a registered
    // meta-schema, so autodetection would fail the compile.
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .build(&combined)
        .map_err(|e| {
            DomainError::internal(format!(
                "provider type schema is not a valid JSON Schema: {e}"
            ))
        })?;
    let violations: Vec<Violation> = validator
        .iter_errors(settings)
        .map(|error| {
            Violation::new(
                format!("provider_settings{}", error.instance_path()),
                error.to_string(),
                field::Reason::SchemaMismatch,
            )
        })
        .collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(DomainError::Invalid(violations))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reasons(err: &DomainError) -> Vec<(String, field::Reason)> {
        match err {
            DomainError::Invalid(v) => v.iter().map(|v| (v.field.clone(), v.reason)).collect(),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn provider_type_checks() {
        check_provider_type(&gts::GtsTypeId::new(
            "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~",
        ))
        .unwrap();

        let err = check_provider_type(&gts::GtsTypeId::new("not a gts id")).unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![("gts_type".to_owned(), field::Reason::InvalidGtsType)]
        );

        let err = check_provider_type(&gts::GtsTypeId::new(
            "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1",
        ))
        .unwrap_err();
        assert_eq!(reasons(&err)[0].1, field::Reason::InvalidGtsType);

        for bad in [
            "gts.cf.genai.model.provider.v1~",
            "gts.cf.toolkit.authz.permission.v1~cf.genai._.openai.v1~",
        ] {
            let err = check_provider_type(&gts::GtsTypeId::new(bad)).unwrap_err();
            assert_eq!(reasons(&err)[0].1, field::Reason::NotAProviderType, "{bad}");
        }
    }

    fn derived_schema(settings: &serde_json::Value) -> serde_json::Value {
        json!({
            "$id": "gts://gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~",
            "type": "object",
            "allOf": [
                {
                    "type": "object",
                    "properties": {
                        "gts_type": { "type": "string" },
                        "provider_settings": { "type": "object" }
                    }
                },
                { "type": "object", "properties": { "provider_settings": settings } }
            ]
        })
    }

    #[test]
    fn settings_validation_reports_pointer_per_violation() {
        let schema = derived_schema(&json!({
            "type": "object",
            "required": ["oagw_alias"],
            "properties": {
                "oagw_alias": { "type": "string" },
                "cost": { "type": "object", "properties": { "input": { "type": "number" } } }
            }
        }));
        validate_settings(
            &schema,
            &json!({ "oagw_alias": "p", "cost": { "input": 1.5 } }),
        )
        .unwrap();

        let err = validate_settings(&schema, &json!({ "cost": { "input": "x" } })).unwrap_err();
        let r = reasons(&err);
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(
            r.iter()
                .all(|(_, reason)| *reason == field::Reason::SchemaMismatch)
        );
        assert!(
            r.iter().any(|(f, _)| f == "provider_settings/cost/input"),
            "{r:?}"
        );
        assert!(r.iter().any(|(f, _)| f == "provider_settings"), "{r:?}");
    }

    #[test]
    fn open_schema_accepts_any_object_and_rejects_non_objects() {
        let open = derived_schema(&json!({ "type": "object" }));
        validate_settings(&open, &json!({ "anything": [1, 2] })).unwrap();
        let err = validate_settings(&json!({ "type": "object" }), &json!("str")).unwrap_err();
        assert_eq!(reasons(&err)[0].1, field::Reason::SchemaMismatch);
        validate_settings(&json!({ "type": "object" }), &json!({})).unwrap();
    }

    #[tokio::test]
    async fn unregistered_type_is_reported_on_the_named_field() {
        let source = crate::testing::StaticSchemaSource::new();
        let ctx = crate::testing::security_context(uuid::Uuid::new_v4());
        let err = provider_type_schema(
            &source,
            &ctx,
            &gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.x.v1~"),
            "provider_settings",
        )
        .await
        .unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![(
                "provider_settings".to_owned(),
                field::Reason::GtsTypeNotRegistered
            )]
        );
    }
}
