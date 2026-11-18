// Created: 2026-11-18 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Conformance suite: fixture cases under `tests/cases`, driven through the
//! public plugin API against a fake OAGW. See `tests/README.md`.

mod support;

use std::collections::{BTreeMap, BTreeSet};

use anthropic_plugin::AnthropicPlugin;
use futures::StreamExt;
use llm_gateway_sdk::{CreateResponseBody, LlmGatewayProviderPluginClientV1, ProviderCallCtx};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;

use support::{API_KEY, Case, FakeGateway, Mode, REQUEST_ID, Recorded, Reply, check, normalize};

const CONFIG: &str = "config.json";
const REQUEST: &str = "request.json";
const ANTHROPIC_REQUEST: &str = "anthropic_request.json";
const REPLY: &str = "reply.json";
const STREAM_REPLY: &str = "stream_reply.json";
const STREAM_REPLY_SSE: &str = "stream_reply.sse";
const RECORDING: &str = "recording.json";
const RESPONSE: &str = "response.json";
const EVENTS: &str = "events.json";
const ERROR: &str = "error.json";
const STREAM_ERROR: &str = "stream_error.json";

const DEFAULT_ANTHROPIC_VERSION: &str = "2026-10-21";

/// What one plugin call sent and returned.
struct Run {
    sent: Option<Recorded>,
    /// The response or event list, or the error as a `ResponseError`.
    result: Result<Value, Value>,
}

async fn run(case: &Case, mode: Mode, reply: Reply) -> Run {
    let gateway = FakeGateway::new(reply);
    let plugin = AnthropicPlugin::new(gateway.clone());
    let ctx = SecurityContext::anonymous();
    let call = ProviderCallCtx::new(case.model_info(), REQUEST_ID, CancellationToken::new())
        .with_api_key(API_KEY.to_owned().into());
    let request = case
        .json(REQUEST)
        .unwrap_or_else(|| panic!("{}/{REQUEST} is missing", case.name));
    let body: CreateResponseBody = serde_json::from_value(request)
        .unwrap_or_else(|e| panic!("{}/{REQUEST} is not a valid request: {e}", case.name));

    let result = match mode {
        Mode::Sync => plugin
            .create_response(&ctx, &call, body)
            .await
            .map(|response| serde_json::to_value(response).unwrap()),
        Mode::Stream => match plugin.create_response_stream(&ctx, &call, body).await {
            Ok(events) => Ok(serde_json::to_value(events.collect::<Vec<_>>().await).unwrap()),
            Err(e) => Err(e),
        },
    };
    Run {
        sent: gateway.sent(),
        result: result
            .map(normalize)
            .map_err(|e| serde_json::to_value(e.to_response_error()).unwrap()),
    }
}

/// A reply for runs that only look at the outbound request.
fn stub_reply() -> Reply {
    Reply {
        status: 500,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        chunks: vec![b"{}".to_vec()],
        stream: false,
    }
}

fn stream_reply(case: &Case) -> Option<Reply> {
    let meta = case.json(STREAM_REPLY)?;
    Some(Reply::from_file(
        &meta,
        std::fs::read(case.path(STREAM_REPLY_SSE)).ok(),
    ))
}

fn report(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Input: gateway request -> Anthropic request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn requests_match() {
    let mut failures = Vec::new();
    for case in support::cases() {
        for mode in [Mode::Sync, Mode::Stream] {
            if !case.runs(mode) {
                continue;
            }
            let run = run(&case, mode, stub_reply()).await;
            match run.sent {
                Some(sent) => check_sent(&case, mode, sent, &mut failures),
                None if case.has(ANTHROPIC_REQUEST) => failures.push(format!(
                    "{} ({mode:?}): no provider request was sent: {}",
                    case.name,
                    run.result.unwrap_err()
                )),
                None => check(&case, ERROR, &run.result.unwrap_err(), &mut failures),
            }
        }
    }
    report(&failures);
}

fn check_sent(case: &Case, mode: Mode, sent: Recorded, failures: &mut Vec<String>) {
    let settings = case.settings();
    let mut body = sent.body;
    let stream = body.as_object_mut().and_then(|o| o.remove("stream"));
    let mut expect = |what: &str, actual: Option<&str>, expected: Option<&str>| {
        if actual != expected {
            failures.push(format!(
                "{} ({mode:?}): {what}: expected {expected:?}, got {actual:?}",
                case.name
            ));
        }
    };
    let header = |name: &str| sent.headers.get(name).and_then(|v| v.to_str().ok());
    let version = match settings["anthropic_version"].as_str() {
        Some(version) if !version.is_empty() => version,
        _ => DEFAULT_ANTHROPIC_VERSION,
    };
    let beta = settings["anthropic_beta"]
        .as_array()
        .filter(|flags| !flags.is_empty())
        .map(|flags| {
            let flags: Vec<&str> = flags.iter().filter_map(Value::as_str).collect();
            flags.join(",")
        });
    let (accept, stream_flag) = match mode {
        Mode::Sync => ("application/json", None),
        Mode::Stream => ("text/event-stream", Some(json!(true))),
    };

    expect("method", Some(sent.method.as_str()), Some("POST"));
    expect("uri", Some(&sent.uri), Some("/anthropic/v1/messages"));
    expect("accept", header("accept"), Some(accept));
    expect(
        "content-type",
        header("content-type"),
        Some("application/json"),
    );
    expect(
        "anthropic-version",
        header("anthropic-version"),
        Some(version),
    );
    expect("anthropic-beta", header("anthropic-beta"), beta.as_deref());
    expect(
        "authorization",
        header("authorization"),
        Some(&format!("Bearer {API_KEY}")),
    );
    if stream != stream_flag {
        failures.push(format!(
            "{} ({mode:?}): stream: expected {stream_flag:?}, got {stream:?}",
            case.name
        ));
    }
    check(case, ANTHROPIC_REQUEST, &body, failures);
}

// ---------------------------------------------------------------------------
// Output: Anthropic reply -> gateway response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_responses_match() {
    let mut failures = Vec::new();
    for case in support::cases() {
        let Some(meta) = case.json(REPLY).filter(|_| case.runs(Mode::Sync)) else {
            continue;
        };
        let run = run(&case, Mode::Sync, Reply::from_file(&meta, None)).await;
        match run.result {
            Ok(response) => check(&case, RESPONSE, &response, &mut failures),
            Err(error) => check(&case, ERROR, &error, &mut failures),
        }
    }
    report(&failures);
}

#[tokio::test]
async fn stream_responses_match() {
    let mut failures = Vec::new();
    for case in support::cases() {
        let Some(reply) = stream_reply(&case).filter(|_| case.runs(Mode::Stream)) else {
            continue;
        };
        let whole = run(&case, Mode::Stream, reply.clone()).await.result;
        let chunked = run(&case, Mode::Stream, reply.rechunked(7)).await.result;
        if whole != chunked {
            failures.push(format!(
                "{}: the result depends on how the reply is chunked",
                case.name
            ));
        }
        match whole {
            Ok(events) => check(&case, EVENTS, &events, &mut failures),
            Err(error) => check(&case, STREAM_ERROR, &error, &mut failures),
        }
    }
    report(&failures);
}

#[tokio::test]
async fn stream_invariants() {
    let mut failures = Vec::new();
    for case in support::cases() {
        let Some(reply) = stream_reply(&case).filter(|_| case.runs(Mode::Stream)) else {
            continue;
        };
        let Ok(events) = run(&case, Mode::Stream, reply).await.result else {
            continue;
        };
        if let Err(problem) = invariants(events.as_array().unwrap()) {
            failures.push(format!("{}: {problem}", case.name));
        }
    }
    report(&failures);
}

const TERMINAL: [&str; 3] = [
    "response.completed",
    "response.incomplete",
    "response.failed",
];

/// Properties every translated stream has, whatever the reply was.
fn invariants(events: &[Value]) -> Result<(), String> {
    let kind = |event: &Value| event["type"].as_str().unwrap_or_default().to_owned();
    let index = |event: &Value, field: &str| event[field].as_u64().unwrap_or_default();

    for (position, event) in events.iter().enumerate() {
        if event["sequence_number"].as_u64() != Some(position as u64) {
            return Err(format!(
                "event {position} ({}) has sequence_number {}",
                kind(event),
                event["sequence_number"]
            ));
        }
    }
    let terminals = events
        .iter()
        .filter(|e| TERMINAL.contains(&kind(e).as_str()))
        .count();
    let last = events.last().ok_or("no events")?;
    if terminals != 1 || !TERMINAL.contains(&kind(last).as_str()) {
        return Err(format!(
            "expected one terminal event at the end, got {terminals}, last is {}",
            kind(last)
        ));
    }
    if kind(&events[0]) != "response.created" {
        return Err(format!("first event is {}", kind(&events[0])));
    }

    let mut open = BTreeSet::new();
    let mut done = BTreeMap::new();
    let mut text: BTreeMap<(u64, u64), String> = BTreeMap::new();
    let mut reasoning: BTreeMap<(u64, u64), String> = BTreeMap::new();
    let mut arguments: BTreeMap<u64, String> = BTreeMap::new();
    for event in events {
        let output = index(event, "output_index");
        let part = (output, index(event, "content_index"));
        match kind(event).as_str() {
            "response.output_item.added" => {
                if !open.insert(output) {
                    return Err(format!("output item {output} was added twice"));
                }
            }
            "response.output_item.done" => {
                if !open.remove(&output) {
                    return Err(format!("output item {output} is done but was not added"));
                }
                done.insert(output, event["item"].clone());
            }
            "response.output_text.delta" => {
                *text.entry(part).or_default() += event["text"].as_str().unwrap_or_default();
            }
            "response.reasoning.delta" => {
                *reasoning.entry(part).or_default() += event["delta"].as_str().unwrap_or_default();
            }
            "response.function_call_arguments.delta" => {
                *arguments.entry(output).or_default() +=
                    event["arguments"].as_str().unwrap_or_default();
            }
            _ => {}
        }
    }
    if kind(last) == "response.failed" {
        return Ok(());
    }
    if let Some(output) = open.first() {
        return Err(format!("output item {output} was added but never done"));
    }

    let output = &last["response"]["output"];
    let item = |output_index: u64| &output[usize::try_from(output_index).unwrap()];
    let part = |(output_index, content_index): (u64, u64)| {
        &item(output_index)["content"][usize::try_from(content_index).unwrap()]["text"]
    };
    if done.len() != output.as_array().map_or(0, Vec::len) {
        return Err(format!(
            "{} items were done, the final response has {}",
            done.len(),
            output.as_array().map_or(0, Vec::len)
        ));
    }
    for (output_index, done_item) in &done {
        if done_item != item(*output_index) {
            return Err(format!(
                "output item {output_index} differs between its done event and the final response"
            ));
        }
    }
    for (at, deltas) in text.iter().chain(&reasoning) {
        if part(*at).as_str() != Some(deltas) {
            return Err(format!(
                "deltas of part {at:?} do not add up to the final text"
            ));
        }
    }
    // The final arguments are re-serialized, so they match the deltas as JSON.
    let json = |text: &str| serde_json::from_str::<Value>(text).unwrap_or_else(|_| json!({}));
    for (output_index, deltas) in &arguments {
        let done = item(*output_index)["arguments"]
            .as_str()
            .unwrap_or_default();
        if json(done) != json(deltas) {
            return Err(format!(
                "argument deltas of item {output_index} do not add up to the final arguments"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The fixture set itself
// ---------------------------------------------------------------------------

#[test]
fn recordings_are_current() {
    let failures: Vec<String> = support::cases()
        .iter()
        .filter_map(|case| {
            let recording = case.json(RECORDING)?;
            (Some(&recording["request"]) != case.json(ANTHROPIC_REQUEST).as_ref()).then(|| {
                format!(
                    "{}: {ANTHROPIC_REQUEST} changed since the reply was recorded; \
                     re-record with tests/record.py --force --case {}",
                    case.name, case.name
                )
            })
        })
        .collect();
    report(&failures);
}

#[test]
fn every_case_is_well_formed() {
    let failures: Vec<String> = support::cases()
        .iter()
        .flat_map(|case| {
            problems(case)
                .into_iter()
                .map(move |problem| format!("{}: {problem}", case.name))
        })
        .collect();
    report(&failures);
}

fn problems(case: &Case) -> Vec<String> {
    const KNOWN: [&str; 11] = [
        CONFIG,
        REQUEST,
        ANTHROPIC_REQUEST,
        REPLY,
        STREAM_REPLY,
        STREAM_REPLY_SSE,
        RECORDING,
        RESPONSE,
        EVENTS,
        ERROR,
        STREAM_ERROR,
    ];
    let mut problems = Vec::new();
    let mut require = |ok: bool, problem: &str| {
        if !ok {
            problems.push(problem.to_owned());
        }
    };

    for entry in std::fs::read_dir(&case.dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        require(
            KNOWN.contains(&name.as_str()),
            &format!("unexpected file {name}"),
        );
    }
    require(case.has(REQUEST), "request.json is missing");

    if !case.has(ANTHROPIC_REQUEST) {
        require(
            case.has(ERROR),
            "neither anthropic_request.json nor error.json",
        );
        for file in [
            REPLY,
            STREAM_REPLY,
            STREAM_REPLY_SSE,
            RECORDING,
            RESPONSE,
            EVENTS,
            STREAM_ERROR,
        ] {
            require(
                !case.has(file),
                &format!("{file} in a case rejected before sending"),
            );
        }
        return problems;
    }

    let recorded = |reply: &str| {
        if case.config.synthetic {
            format!("{reply} is missing")
        } else {
            format!("{reply} is missing: record it with tests/record.py")
        }
    };
    if case.runs(Mode::Sync) {
        require(case.has(REPLY), &recorded(REPLY));
        require(
            !case.has(REPLY) || case.has(RESPONSE) != case.has(ERROR),
            "expected exactly one of response.json and error.json",
        );
    } else {
        require(
            ![REPLY, RESPONSE, ERROR].iter().any(|f| case.has(f)),
            "sync files in a case without the sync mode",
        );
    }
    if case.runs(Mode::Stream) {
        require(case.has(STREAM_REPLY), &recorded(STREAM_REPLY));
        require(
            !case.has(STREAM_REPLY) || case.has(EVENTS) != case.has(STREAM_ERROR),
            "expected exactly one of events.json and stream_error.json",
        );
    } else {
        require(
            ![STREAM_REPLY, STREAM_REPLY_SSE, EVENTS, STREAM_ERROR]
                .iter()
                .any(|f| case.has(f)),
            "stream files in a case without the stream mode",
        );
    }
    let has_reply = case.has(REPLY) || case.has(STREAM_REPLY);
    require(
        !has_reply || case.has(RECORDING) != case.config.synthetic,
        "recording.json must exist exactly for recorded (non-synthetic) replies",
    );
    problems
}
