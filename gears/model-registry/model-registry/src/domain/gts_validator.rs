//! GTS checks on writes (DESIGN §4 GTS Checks): provider type checks on
//! provider create and type migration, `provider_settings` validation on
//! model create, settings update, and for every model of a provider on type
//! migration. Not called on the resolution path.

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

/// Provider type checks 1 and 2: the id parses as a GTS type id, and it is
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

/// Compiles the `provider_settings` validator of a resolved provider type
/// schema (JSON Schema Draft-07). `None` when the schema does not constrain
/// `provider_settings`, which then accepts any JSON object.
pub fn compile_settings_validator(
    resolved_schema: &serde_json::Value,
) -> Result<Option<jsonschema::Validator>, DomainError> {
    let subschemas = settings_subschemas(resolved_schema);
    if subschemas.is_empty() {
        return Ok(None);
    }
    // The subschemas reference nested types through `#/definitions/...` and
    // `#/$defs/...` of the resolved document, so those sections travel with
    // them.
    let mut combined = serde_json::json!({ "allOf": subschemas });
    for section in ["definitions", "$defs"] {
        if let Some(defs) = resolved_schema.get(section) {
            combined[section] = defs.clone();
        }
    }
    // The draft is pinned: a GTS chain id in `$schema` is not a registered
    // meta-schema, so autodetection would fail the compile.
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .build(&combined)
        .map(Some)
        .map_err(|e| {
            DomainError::internal(format!(
                "provider type schema is not a valid JSON Schema: {e}"
            ))
        })
}

/// Checks one `provider_settings` document against a compiled validator. Each
/// violation is reported on `field_name`: `provider_settings` plus the JSON
/// pointer on model writes, or `gts_type` with the model's canonical id in the
/// description on a type migration (`context`).
#[must_use]
pub fn check_settings(
    validator: Option<&jsonschema::Validator>,
    settings: &serde_json::Value,
    field_name: &str,
    context: Option<&str>,
) -> Vec<Violation> {
    let describe = |pointer: &str, message: &str| match context {
        Some(ctx) => format!("{ctx}: provider_settings{pointer}: {message}"),
        None => message.to_owned(),
    };
    let field_of = |pointer: &str| {
        if context.is_some() {
            field_name.to_owned()
        } else {
            format!("{field_name}{pointer}")
        }
    };
    if !settings.is_object() {
        return vec![Violation::new(
            field_of(""),
            describe("", "must be a JSON object"),
            field::Reason::SchemaMismatch,
        )];
    }
    let Some(validator) = validator else {
        return Vec::new();
    };
    validator
        .iter_errors(settings)
        .map(|error| {
            let pointer = error.instance_path().to_string();
            Violation::new(
                field_of(&pointer),
                describe(&pointer, &error.to_string()),
                field::Reason::SchemaMismatch,
            )
        })
        .collect()
}

/// Validates `provider_settings` of one model write against the resolved
/// provider type schema.
pub fn validate_settings(
    resolved_schema: &serde_json::Value,
    settings: &serde_json::Value,
) -> Result<(), DomainError> {
    let validator = compile_settings_validator(resolved_schema)?;
    let violations = check_settings(validator.as_ref(), settings, "provider_settings", None);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(DomainError::invalid_all(violations))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use toolkit_gts::gts_id;

    fn reasons(err: &DomainError) -> Vec<(String, field::Reason)> {
        match err {
            DomainError::Invalid { violations: v, .. } => {
                v.iter().map(|v| (v.field.clone(), v.reason)).collect()
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn provider_type_checks() {
        check_provider_type(&gts::GtsTypeId::new(gts_id!(
            "cf.genai.model.provider.v1~cf.genai._.openai.v1~"
        )))
        .unwrap();

        let err = check_provider_type(&gts::GtsTypeId::new("not a gts id")).unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![("gts_type".to_owned(), field::Reason::InvalidGtsType)]
        );

        let err = check_provider_type(&gts::GtsTypeId::new(gts_id!(
            "cf.genai.model.provider.v1~cf.genai._.openai.v1"
        )))
        .unwrap_err();
        assert_eq!(reasons(&err)[0].1, field::Reason::InvalidGtsType);

        for bad in [
            gts_id!("cf.genai.model.provider.v1~"),
            gts_id!("cf.toolkit.authz.permission.v1~cf.genai._.openai.v1~"),
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
                "region": { "type": "object", "properties": { "zone": { "type": "number" } } }
            }
        }));
        validate_settings(
            &schema,
            &json!({ "oagw_alias": "p", "region": { "zone": 1.5 } }),
        )
        .unwrap();

        let err = validate_settings(&schema, &json!({ "region": { "zone": "x" } })).unwrap_err();
        let r = reasons(&err);
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(
            r.iter()
                .all(|(_, reason)| *reason == field::Reason::SchemaMismatch)
        );
        assert!(
            r.iter().any(|(f, _)| f == "provider_settings/region/zone"),
            "{r:?}"
        );
        assert!(r.iter().any(|(f, _)| f == "provider_settings"), "{r:?}");
    }

    /// A derived type with a nested struct references it through the root
    /// `definitions`; the subschema extraction must keep those resolvable.
    #[test]
    fn nested_definitions_stay_resolvable() {
        let mut schema = derived_schema(&json!({
            "type": "object",
            "required": ["endpoint"],
            "properties": { "endpoint": { "$ref": "#/definitions/Endpoint" } }
        }));
        schema["definitions"] = json!({
            "Endpoint": { "type": "object", "properties": { "port": { "type": "integer" } }, "additionalProperties": false }
        });
        validate_settings(&schema, &json!({ "endpoint": { "port": 3 } })).unwrap();
        let err =
            validate_settings(&schema, &json!({ "endpoint": { "port": "https" } })).unwrap_err();
        assert_eq!(reasons(&err)[0].0, "provider_settings/endpoint/port");
    }

    #[test]
    fn open_schema_accepts_any_object_and_rejects_non_objects() {
        let open = derived_schema(&json!({ "type": "object" }));
        validate_settings(&open, &json!({ "anything": [1, 2] })).unwrap();
        let err = validate_settings(&json!({ "type": "object" }), &json!("str")).unwrap_err();
        assert_eq!(reasons(&err)[0].1, field::Reason::SchemaMismatch);
        validate_settings(&json!({ "type": "object" }), &json!({})).unwrap();
        assert!(
            compile_settings_validator(&json!({ "type": "object" }))
                .unwrap()
                .is_none()
        );
    }

    /// A type migration reports every model's violations on `gts_type`,
    /// naming the model and the pointer.
    #[test]
    fn migration_violations_land_on_gts_type_with_context() {
        let schema = derived_schema(&json!({
            "type": "object",
            "required": ["oagw_alias"],
            "properties": { "oagw_alias": { "type": "string" } }
        }));
        let validator = compile_settings_validator(&schema).unwrap();
        assert!(validator.is_some());
        let ok = check_settings(
            validator.as_ref(),
            &json!({ "oagw_alias": "p" }),
            "gts_type",
            Some("openai::gpt-4o"),
        );
        assert!(ok.is_empty());
        let bad = check_settings(
            validator.as_ref(),
            &json!({ "oagw_alias": 1 }),
            "gts_type",
            Some("openai::gpt-4o"),
        );
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].field, "gts_type");
        assert_eq!(bad[0].reason, field::Reason::SchemaMismatch);
        assert!(
            bad[0]
                .description
                .starts_with("openai::gpt-4o: provider_settings/oagw_alias:"),
            "{}",
            bad[0].description
        );
    }

    #[tokio::test]
    async fn unregistered_type_is_reported_on_the_named_field() {
        let source = crate::testing::StaticSchemaSource::new();
        let ctx = crate::testing::security_context(uuid::Uuid::new_v4());
        let err = provider_type_schema(
            &source,
            &ctx,
            &gts::GtsTypeId::new(gts_id!("cf.genai.model.provider.v1~cf.genai._.x.v1~")),
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
