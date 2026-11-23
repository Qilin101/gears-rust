// Created: 2026-11-18 by Constructor Tech
//! Stateless guard: rejects native Anthropic values that create or reference
//! state kept at Anthropic, and provider tools billed per call, and keeps only
//! allowlisted `anthropic-beta` flags.
//!
//! Applied to pass-through bodies and to `anthropic:<type>` items and tools of
//! the unified API. A match fails with `capability_not_supported` carrying the
//! JSON path of the field as `param`. The beta allowlist applies to the
//! pass-through header and to the model's `anthropic_beta` setting.

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

/// `anthropic-beta` flags that are forwarded: features that keep no state at
/// Anthropic, call no other model or service, and are priced at the model's
/// rates. Any other flag is dropped.
const ALLOWED_BETAS: [&str; 18] = [
    "compact-2026-01-12",
    "compact-2026-09-04",
    "computer-use-2025-01-24",
    "computer-use-2025-11-24",
    "context-management-2025-06-27",
    "effort-2025-11-24",
    "fine-grained-tool-streaming-2025-05-14",
    "inline-tools-2026-09-15",
    "interleaved-thinking-2025-05-14",
    "mid-conversation-output-config-2026-07-01",
    "mid-conversation-system-clear-at-2026-08-21",
    "mid-conversation-tool-changes-2026-07-01",
    "output-128k-2025-02-19",
    "structured-outputs-2025-11-13",
    "task-budgets-2026-03-13",
    "thinking-binding-controls-2026-08-01",
    "thinking-display-updates-2026-08-18",
    "token-efficient-tools-2025-02-19",
];

/// The `anthropic-beta` header for `values`, each a flag or a comma-separated
/// list of flags: the allowlisted flags joined by commas, or `None` when none
/// is left.
pub fn beta_header<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let flags: Vec<&str> = values
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|flag| ALLOWED_BETAS.contains(flag))
        .collect();
    (!flags.is_empty()).then(|| flags.join(","))
}

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
