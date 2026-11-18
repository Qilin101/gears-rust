// Created: 2026-11-18 by Constructor Tech
//! Fake OAGW, case loading, and golden-file comparison for the conformance suite.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// Request id of every call; output item ids derive from it.
pub const REQUEST_ID: &str = "req_test";
pub const API_KEY: &str = "sk-test";
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";

// ---------------------------------------------------------------------------
// Fake OAGW
// ---------------------------------------------------------------------------

/// A canned provider reply.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Body chunks; more than one chunk is served as a stream.
    pub chunks: Vec<Vec<u8>>,
    pub stream: bool,
}

impl Reply {
    /// The reply stored as `{status, headers, body}`; `sse` replaces `body`
    /// for an event-stream reply.
    pub fn from_file(meta: &Value, sse: Option<Vec<u8>>) -> Self {
        let headers = meta["headers"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
            .collect();
        let stream = sse.is_some();
        let body = sse.unwrap_or_else(|| match &meta["body"] {
            Value::String(text) => text.clone().into_bytes(),
            other => serde_json::to_vec(other).unwrap(),
        });
        Self {
            status: u16::try_from(meta["status"].as_u64().unwrap()).unwrap(),
            headers,
            chunks: vec![body],
            stream,
        }
    }

    /// The same reply delivered in `size`-byte chunks.
    pub fn rechunked(&self, size: usize) -> Self {
        let body = self.chunks.concat();
        Self {
            chunks: body.chunks(size).map(<[u8]>::to_vec).collect(),
            ..self.clone()
        }
    }

    fn response(&self) -> http::Response<Body> {
        let mut builder = http::Response::builder().status(self.status);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let body = if self.stream {
            let chunks: Vec<Result<Bytes, Box<dyn std::error::Error + Send + Sync>>> = self
                .chunks
                .iter()
                .map(|c| Ok(Bytes::from(c.clone())))
                .collect();
            Body::Stream(Box::pin(futures::stream::iter(chunks)))
        } else {
            Body::Bytes(self.chunks.concat().into())
        };
        builder.body(body).unwrap()
    }
}

/// A recorded outbound request.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: http::Method,
    pub uri: String,
    pub headers: http::HeaderMap,
    pub body: Value,
}

/// Fake OAGW: records the proxied request and answers with a canned reply.
pub struct FakeGateway {
    reply: Reply,
    recorded: Mutex<Vec<Recorded>>,
}

impl FakeGateway {
    pub fn new(reply: Reply) -> Arc<Self> {
        Arc::new(Self {
            reply,
            recorded: Mutex::new(Vec::new()),
        })
    }

    /// The only request sent, if any.
    pub fn sent(&self) -> Option<Recorded> {
        let recorded = self.recorded.lock().unwrap();
        assert!(recorded.len() <= 1, "more than one provider request");
        recorded.first().cloned()
    }
}

fn unused<T>() -> Result<T, CanonicalError> {
    Err(CanonicalError::internal("not used by the plugin").create())
}

#[async_trait]
impl ServiceGatewayClientV1 for FakeGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unused()
    }
    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unused()
    }
    async fn list_upstreams(
        &self,
        _: SecurityContext,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, CanonicalError> {
        unused()
    }
    async fn update_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unused()
    }
    async fn delete_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), CanonicalError> {
        unused()
    }
    async fn create_route(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unused()
    }
    async fn get_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unused()
    }
    async fn list_routes(
        &self,
        _: SecurityContext,
        _: Option<uuid::Uuid>,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, CanonicalError> {
        unused()
    }
    async fn update_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unused()
    }
    async fn delete_route(&self, _: SecurityContext, _: uuid::Uuid) -> Result<(), CanonicalError> {
        unused()
    }
    async fn resolve_proxy_target(
        &self,
        _: SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), CanonicalError> {
        unused()
    }
    async fn proxy_request(
        &self,
        _: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, CanonicalError> {
        let (parts, body) = req.into_parts();
        let bytes = body.into_bytes().await.unwrap();
        self.recorded.lock().unwrap().push(Recorded {
            method: parts.method,
            uri: parts.uri.to_string(),
            headers: parts.headers,
            body: serde_json::from_slice(&bytes).unwrap(),
        });
        Ok(self.reply.response())
    }
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Sync,
    Stream,
}

/// `config.json`: the plugin-side context of a case; every field is optional.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseConfig {
    /// Provider settings merged over the defaults.
    #[serde(default)]
    pub settings: Value,
    /// `ModelInfoV1` fields merged over the defaults.
    #[serde(default)]
    pub model: Value,
    #[serde(default)]
    pub modes: Option<Vec<Mode>>,
    /// Reply files are hand-written; the recorder skips the case.
    #[serde(default)]
    pub synthetic: bool,
    /// Recorder options; unused by the tests.
    #[serde(default, rename = "record")]
    _record: Value,
}

#[derive(Debug, Clone)]
pub struct Case {
    /// Path relative to `tests/cases`, e.g. `input/text_string`.
    pub name: String,
    pub dir: PathBuf,
    pub config: CaseConfig,
}

impl Case {
    pub fn path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    pub fn has(&self, file: &str) -> bool {
        self.path(file).is_file()
    }

    pub fn json(&self, file: &str) -> Option<Value> {
        let text = std::fs::read_to_string(self.path(file)).ok()?;
        Some(
            serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}/{file} is not valid JSON: {e}", self.name)),
        )
    }

    pub fn runs(&self, mode: Mode) -> bool {
        self.config
            .modes
            .as_ref()
            .is_none_or(|modes| modes.contains(&mode))
    }

    pub fn settings(&self) -> Value {
        let mut settings = json!({
            "oagw_alias": "anthropic",
            "anthropic_version": "",
            "anthropic_beta": [],
            "max_tokens": null,
            "stop_sequences": null,
            "system": null,
            "inference_geo": null,
            "service_tier": null,
            "container": null,
            "thinking": null,
            "tool_choice": null,
            "output_config": null
        });
        merge(&mut settings, &self.config.settings);
        settings
    }

    pub fn model_info(&self) -> model_registry_sdk::ModelInfoV1 {
        let mut info = json!({
            "gts_type": "gts.cf.genai.model.provider.v1~cf.genai._.anthropic.v1~",
            "provider_model_id": DEFAULT_MODEL,
            "base_url": null,
            "secret_ref": null,
            "display_name": "Claude",
            "description": null,
            "family": null,
            "vendor": null,
            "managed": false,
            "architecture": null,
            "size_bytes": null,
            "format": null,
            "region": null,
            "hosted_by": null,
            "last_release_at": null,
            "reasoning_level": null,
            "model_version": null,
            "sort_order": 0,
            "icon": null,
            "multiplier_display": null,
            "cost": {},
            "performance": {},
            "additional_info": {},
            "supported_api": ["completion"],
            "capabilities": {
                "vision": { "enabled": true, "supported_mime_types": ["image/png"] },
                "reasoning": { "effort": true, "toggle": true, "resume": true, "budget": false },
                "tools": {
                    "enabled": true, "parallel_tool_calls": true, "max_tool_calls": false,
                    "tool_choice_required": true, "tool_choice_none": true, "tool_choice_named": true
                },
                "output": { "json_mode": false, "json_schema": true, "strict_schema": true, "verbosity": false },
                "sampling": {
                    "temperature": false, "top_p": false, "seed": false,
                    "presence_penalty": false, "frequency_penalty": false, "logprobs": false
                },
                "streaming": true,
                "file_input": { "enabled": true, "supported_mime_types": ["application/pdf"] },
                "image_generation": { "enabled": false, "supported_mime_types": [] },
                "audio_input": { "enabled": false, "supported_mime_types": [] },
                "audio_output": { "enabled": false, "supported_mime_types": [] },
                "code_interpreter": false,
                "web_search": { "enabled": true, "allowed_domains": true, "excluded_domains": true }
            },
            "disabled_capabilities": {},
            "context_window": { "max_input_tokens": 200_000, "max_output_tokens": 8192, "output_vector_size": null },
            "default_parameters": {},
            "allow_parameter_override": true,
            "allow_extra_params": [],
            "provider_settings": self.settings(),
        });
        merge(&mut info, &self.config.model);
        serde_json::from_value(info)
            .unwrap_or_else(|e| panic!("{}: invalid model override: {e}", self.name))
    }
}

/// Merge `patch` into `base`: objects merge by key, anything else replaces.
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (_, Value::Null) => {}
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                match base.get_mut(key) {
                    Some(slot) if slot.is_object() && value.is_object() => merge(slot, value),
                    _ => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, patch) => *base = patch.clone(),
    }
}

pub fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases")
}

/// Every directory under `tests/cases` holding a `request.json`, sorted by name.
/// `CASE=<substring>` keeps only the matching ones.
pub fn cases() -> Vec<Case> {
    let mut found = all_cases();
    if let Ok(filter) = std::env::var("CASE") {
        found.retain(|case| case.name.contains(&filter));
    }
    found
}

/// Every case, whatever `CASE` says.
pub fn all_cases() -> Vec<Case> {
    let root = cases_dir();
    let mut found = Vec::new();
    collect(&root, &root, &mut found);
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn collect(root: &Path, dir: &Path, found: &mut Vec<Case>) {
    let name = dir
        .strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let config = dir.join("config.json");
    if dir.join("request.json").is_file() {
        let config = match std::fs::read_to_string(&config) {
            Ok(text) => serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{name}/config.json is invalid: {e}")),
            Err(_) => CaseConfig::default(),
        };
        found.push(Case {
            name,
            dir: dir.to_owned(),
            config,
        });
        return;
    }
    assert!(
        !config.is_file(),
        "{name}/config.json has no request.json next to it"
    );
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(root, &path, found);
        }
    }
}

// ---------------------------------------------------------------------------
// Golden files
// ---------------------------------------------------------------------------

pub fn updating() -> bool {
    std::env::var("UPDATE_GOLDEN").is_ok_and(|v| v == "1")
}

/// Compare `actual` with the golden `file` of `case`, or rewrite the file
/// under `UPDATE_GOLDEN=1`. A mismatch is appended to `failures`.
pub fn check(case: &Case, file: &str, actual: &Value, failures: &mut Vec<String>) {
    let expected = case.json(file);
    if expected.as_ref() == Some(actual) {
        return;
    }
    if updating() {
        let mut text = serde_json::to_string_pretty(actual).unwrap();
        text.push('\n');
        std::fs::write(case.path(file), text).unwrap();
        return;
    }
    failures.push(match expected {
        Some(expected) => format!(
            "{}/{file}: {}",
            case.name,
            difference(&expected, actual, "").unwrap_or_default()
        ),
        None => format!(
            "{}/{file} is missing; run with UPDATE_GOLDEN=1 and review the result",
            case.name
        ),
    });
}

/// The first place two JSON values differ, as a JSON pointer with both sides.
pub fn difference(expected: &Value, actual: &Value, at: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            for (key, value) in e {
                let at = format!("{at}/{key}");
                match a.get(key) {
                    Some(other) => {
                        if let Some(found) = difference(value, other, &at) {
                            return Some(found);
                        }
                    }
                    None => return Some(format!("{at}: expected {value}, got nothing")),
                }
            }
            a.iter()
                .find(|(key, _)| !e.contains_key(*key))
                .map(|(key, value)| format!("{at}/{key}: unexpected {value}"))
        }
        (Value::Array(e), Value::Array(a)) => {
            for (index, (value, other)) in e.iter().zip(a).enumerate() {
                if let Some(found) = difference(value, other, &format!("{at}/{index}")) {
                    return Some(found);
                }
            }
            (e.len() != a.len())
                .then(|| format!("{at}: expected {} elements, got {}", e.len(), a.len()))
        }
        (e, a) => (e != a).then(|| format!("{at}: expected {e}, got {a}")),
    }
}

/// Zero the wall-clock fields, the only values that differ between runs.
pub fn normalize(mut value: Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(object) => {
                for (key, value) in object.iter_mut() {
                    if matches!(key.as_str(), "created_at" | "completed_at") && value.is_number() {
                        *value = json!(0);
                    } else {
                        walk(value);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut value);
    value
}
