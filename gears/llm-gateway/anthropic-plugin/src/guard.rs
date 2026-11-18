// Created: 2026-11-18 by Constructor Tech
//! Stateless guard: rejects native Anthropic values that create or reference
//! state kept at Anthropic, and provider tools billed per call.
//!
//! Applied to pass-through bodies and to `anthropic:<type>` items and tools of
//! the unified API. A match fails with `capability_not_supported` carrying the
//! JSON path of the field as `param`.

use llm_gateway_sdk::LlmGatewayError;
use serde_json::Value;

/// Keys that reference stored provider objects, rejected at any depth, with
/// the capability each one names.
const STORED_OBJECT_KEYS: [(&str, &str); 3] = [
    ("file_id", "files"),
    ("container", "container"),
    ("vector_store_ids", "vector_stores"),
];

/// Type prefixes of rejected provider tools, with the capability each one
/// names: code execution keeps a container, web search is billed per call.
const DENIED_TOOL_PREFIXES: [(&str, &str); 2] = [
    ("code_execution_", "code_execution"),
    ("web_search_", "web_search"),
];

/// Keys whose content the consumer defines; not scanned.
const SKIPPED_KEYS: [&str; 2] = ["input_schema", "metadata"];

/// Parent of the structured output schema, which is not scanned.
const OUTPUT_FORMAT_PATH: &str = "output_config.format";

/// Check a native Messages request body.
pub fn check_request(body: &Value) -> Result<(), LlmGatewayError> {
    let tools = body.get("tools").and_then(Value::as_array);
    for (index, tool) in tools.into_iter().flatten().enumerate() {
        let kind = tool.get("type").and_then(Value::as_str).unwrap_or_default();
        let denied_tool = DENIED_TOOL_PREFIXES
            .iter()
            .find(|(prefix, _)| kind.starts_with(prefix));
        if let Some((_, capability)) = denied_tool {
            return Err(denied(capability, &format!("tools[{index}]")));
        }
    }
    scan(body, "")
}

/// Check one native block or tool located at `path` of the request.
pub fn check_value(value: &Value, path: &str) -> Result<(), LlmGatewayError> {
    scan(value, path)
}

fn scan(value: &Value, path: &str) -> Result<(), LlmGatewayError> {
    match value {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(index, item)| scan(item, &format!("{path}[{index}]"))),
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str);
            if kind == Some("container_upload") {
                return Err(denied("files", path));
            }
            for (key, child) in object {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let stored = STORED_OBJECT_KEYS.iter().find(|(name, _)| name == key);
                if let Some((_, capability)) = stored {
                    return Err(denied(capability, &child_path));
                }
                if key == "source" && child.get("type").and_then(Value::as_str) == Some("file") {
                    return Err(denied("files", &child_path));
                }
                let skipped = SKIPPED_KEYS.contains(&key.as_str())
                    || (key == "input" && kind == Some("tool_use"))
                    || (key == "schema" && path == OUTPUT_FORMAT_PATH);
                if !skipped {
                    scan(child, &child_path)?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn denied(capability: &str, path: &str) -> LlmGatewayError {
    LlmGatewayError::capability_not_supported_param(capability, path)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "guard_tests.rs"]
mod guard_tests;
