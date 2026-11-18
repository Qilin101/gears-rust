// Created: 2026-11-16 by Constructor Tech
//! JSON Schema adaptation for Anthropic structured output
//! (`output_config.format`).
//!
//! Follows `transform_schema` of the Anthropic Python SDK: keywords Anthropic
//! rejects are moved into `description`, and objects are closed.

use llm_gateway_sdk::LlmGatewayError;
use serde_json::{Map, Value, json};

const PARAM: &str = "text.format.schema";

/// String `format` values Anthropic accepts.
const STRING_FORMATS: [&str; 10] = [
    "date-time",
    "time",
    "date",
    "duration",
    "email",
    "hostname",
    "uri",
    "ipv4",
    "ipv6",
    "uuid",
];

/// The schema in the subset Anthropic accepts for structured output.
///
/// # Errors
///
/// `validation_error` when a schema node is not an object or has none of
/// `type`, `anyOf`, `oneOf`, `allOf`, `enum`, `const`, `$ref`.
pub fn adapt(schema: &Value) -> Result<Value, LlmGatewayError> {
    let Value::Object(source) = schema else {
        return Err(invalid("schema must be an object"));
    };
    let mut rest = source.clone();
    let mut out = Map::new();

    if let Some(defs) = rest.remove("$defs") {
        out.insert("$defs".to_owned(), adapt_map(&defs)?);
    }
    if let Some(reference) = rest.remove("$ref") {
        out.insert("$ref".to_owned(), reference);
        return Ok(Value::Object(out));
    }

    let mut kind = rest.remove("type");
    let any_of = rest.remove("anyOf");
    let one_of = rest.remove("oneOf");
    let all_of = rest.remove("allOf");

    let primitive_enum = rest
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| !values.is_empty() && values.iter().all(is_primitive));
    if primitive_enum {
        move_key(&mut rest, &mut out, "enum");
    }
    if rest.get("const").is_some_and(is_primitive) {
        move_key(&mut rest, &mut out, "const");
    }
    let literals = out.contains_key("enum") || out.contains_key("const");
    if literals {
        kind = None;
    }

    if let Some(Value::Array(variants)) = any_of.or(one_of) {
        out.insert("anyOf".to_owned(), adapt_all(&variants)?);
    } else if let Some(Value::Array(variants)) = all_of {
        out.insert("allOf".to_owned(), adapt_all(&variants)?);
    } else if !literals {
        let kind = kind
            .clone()
            .ok_or_else(|| invalid("schema must have type, anyOf, oneOf, allOf, enum, or const"))?;
        out.insert("type".to_owned(), kind);
    }

    move_key(&mut rest, &mut out, "description");
    move_key(&mut rest, &mut out, "title");

    // `type` may be a list, e.g. `["string", "null"]`.
    let is = |name: &str| match &kind {
        Some(Value::String(kind)) => kind == name,
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == name),
        _ => false,
    };
    if is("object") {
        let properties = rest.remove("properties").unwrap_or_else(|| json!({}));
        out.insert("properties".to_owned(), adapt_map(&properties)?);
        rest.remove("additionalProperties");
        out.insert("additionalProperties".to_owned(), json!(false));
        move_key(&mut rest, &mut out, "required");
    }
    if is("string") {
        let supported = rest
            .get("format")
            .and_then(Value::as_str)
            .is_some_and(|format| STRING_FORMATS.contains(&format));
        if supported {
            move_key(&mut rest, &mut out, "format");
        }
    }
    if is("array") {
        if let Some(items) = rest.remove("items") {
            out.insert("items".to_owned(), adapt(&items)?);
        }
        if matches!(rest.get("minItems").and_then(Value::as_u64), Some(0 | 1)) {
            move_key(&mut rest, &mut out, "minItems");
        }
    }

    if !rest.is_empty() {
        let unsupported = rest
            .iter()
            .map(|(key, value)| match value {
                Value::String(text) => format!("{key}: {text}"),
                other => format!("{key}: {other}"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let description = match out.get("description").and_then(Value::as_str) {
            Some(description) => format!("{description}\n\n{{{unsupported}}}"),
            None => format!("{{{unsupported}}}"),
        };
        out.insert("description".to_owned(), json!(description));
    }
    Ok(Value::Object(out))
}

/// Adapt every schema of a name -> schema map (`properties`, `$defs`).
fn adapt_map(schemas: &Value) -> Result<Value, LlmGatewayError> {
    let Value::Object(schemas) = schemas else {
        return Err(invalid("properties and $defs must be objects"));
    };
    schemas
        .iter()
        .map(|(name, schema)| Ok((name.clone(), adapt(schema)?)))
        .collect::<Result<Map<_, _>, _>>()
        .map(Value::Object)
}

fn adapt_all(schemas: &[Value]) -> Result<Value, LlmGatewayError> {
    schemas
        .iter()
        .map(adapt)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn move_key(from: &mut Map<String, Value>, to: &mut Map<String, Value>, key: &str) {
    if let Some(value) = from.remove(key) {
        to.insert(key.to_owned(), value);
    }
}

fn is_primitive(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

fn invalid(message: &str) -> LlmGatewayError {
    LlmGatewayError::validation_param(message, PARAM)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "schema_tests.rs"]
mod schema_tests;
