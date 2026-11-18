// Created: 2026-11-16 by Constructor Tech
use serde_json::json;

use super::*;

#[test]
fn closes_objects_and_moves_unsupported_keywords_to_description() {
    let adapted = adapt(&json!({
        "type": "object",
        "title": "Order",
        "additionalProperties": true,
        "required": ["count"],
        "properties": {
            "count": { "type": "integer", "description": "How many", "minimum": 1, "maximum": 10 },
            "email": { "type": "string", "format": "email" },
            "code": { "type": "string", "format": "byte", "pattern": "^a" },
            "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
            "ids": { "type": "array", "items": { "type": "integer" }, "minItems": 2 }
        }
    }))
    .unwrap();
    assert_eq!(
        adapted,
        json!({
            "type": "object",
            "title": "Order",
            "additionalProperties": false,
            "required": ["count"],
            "properties": {
                "count": { "type": "integer", "description": "How many\n\n{maximum: 10, minimum: 1}" },
                "email": { "type": "string", "format": "email" },
                "code": { "type": "string", "description": "{format: byte, pattern: ^a}" },
                "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "ids": { "type": "array", "items": { "type": "integer" }, "description": "{minItems: 2}" }
            }
        })
    );
}

#[test]
fn rewrites_one_of_and_keeps_refs_and_literals() {
    let adapted = adapt(&json!({
        "$defs": { "Leaf": { "type": "object", "properties": { "v": { "type": "number" } } } },
        "type": "object",
        "properties": {
            "leaf": { "$ref": "#/$defs/Leaf", "description": "dropped" },
            "choice": { "oneOf": [{ "type": "string" }, { "type": "null" }] },
            "level": { "type": "string", "enum": ["low", "high"] },
            "fixed": { "const": 1 },
            "nullable": { "type": ["object", "null"], "properties": {} }
        }
    }))
    .unwrap();
    assert_eq!(
        adapted,
        json!({
            "$defs": { "Leaf": { "type": "object", "additionalProperties": false,
                "properties": { "v": { "type": "number" } } } },
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "leaf": { "$ref": "#/$defs/Leaf" },
                "choice": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                "level": { "enum": ["low", "high"] },
                "fixed": { "const": 1 },
                "nullable": { "type": ["object", "null"], "properties": {}, "additionalProperties": false }
            }
        })
    );
}

#[test]
fn untyped_and_non_object_schemas_are_unsupported() {
    for schema in [
        json!({ "properties": {} }),
        json!(true),
        json!({ "type": "array", "items": true }),
    ] {
        let err = adapt(&schema).unwrap_err();
        assert_eq!(err.code(), "unsupported_schema", "{schema}");
    }
}
