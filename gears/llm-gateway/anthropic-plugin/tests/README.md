# Conformance tests

Fixture cases that drive the plugin through its public API against a fake OAGW. Each case checks two
transformations with one plugin call per mode:

1. **Input** — the gateway request becomes the expected Anthropic request.
2. **Output** — a reply recorded from the real Anthropic API becomes the expected gateway response
   (sync) or event list (stream).

```bash
cargo test -p cf-gears-llm-gateway-anthropic-plugin --test conformance
CASE=reasoning/ cargo test -p cf-gears-llm-gateway-anthropic-plugin --test conformance   # a subset
```

The tests need no network and no API key.

## Case files

A case is a directory under `cases/<area>/` with a `request.json`.

| File | Written by | Content |
|---|---|---|
| `request.json` | hand | Gateway `CreateResponseBody` |
| `config.json` | hand | Optional: `settings` and `model` overrides, `modes`, `synthetic`, `record` |
| `anthropic_request.json` | `UPDATE_GOLDEN=1` | Expected body sent to Anthropic, without `stream` |
| `reply.json` | `record.py` | Sync reply: `{status, headers, body}` |
| `stream_reply.json`, `stream_reply.sse` | `record.py` | Stream reply: `{status, headers}` and the raw event stream; a non-SSE reply keeps its `body` in the JSON file |
| `recording.json` | `record.py` | When the replies were recorded and the request that was sent |
| `response.json` / `error.json` | `UPDATE_GOLDEN=1` | Expected sync result |
| `events.json` / `stream_error.json` | `UPDATE_GOLDEN=1` | Expected stream result |

A case without `anthropic_request.json` is rejected before anything is sent and has only `error.json`.

`config.json` options:

- `settings` — provider settings merged over the defaults in `support/mod.rs`.
- `model` — `ModelInfoV1` fields merged over the defaults, e.g. `{"provider_model_id": "..."}`.
- `modes` — `["sync"]` or `["stream"]`; both when absent.
- `synthetic` — the reply files are hand-written because the real API cannot be made to produce them
  (429, 529, refusal, a cut stream). The recorder skips these cases.
- `record` — recorder options: `warmup` sends the request twice and keeps the second reply,
  `expect_error` marks a 4xx reply as intended, `api_key` overrides the key.

## Adding a case

1. Write `request.json`, and `config.json` if the case needs non-default settings.
2. `UPDATE_GOLDEN=1 cargo test -p cf-gears-llm-gateway-anthropic-plugin --test conformance requests_match`,
   then review the generated `anthropic_request.json` (or `error.json`).
3. Record the replies: `ANTHROPIC_API_KEY=... python tests/record.py --case <name>`.
   A reply the API rejects means the expected request is wrong.
4. Run with `UPDATE_GOLDEN=1` again and review `response.json` and `events.json`.

`UPDATE_GOLDEN=1` never touches hand-written or recorded files. Generated files are expectations:
review every diff before committing it.

## Re-recording

`recordings_are_current` fails when `anthropic_request.json` no longer matches the request a reply was
recorded for. `record.py` re-records such cases; `--force` re-records everything, `--dry-run` lists
what would be sent. Only `content-type`, `request-id`, `retry-after` and `retry-after-ms` reply
headers are stored.

The recorder is the only part that needs Python. Install its packages into an environment of your
choice with `pip install -r tests/requirements.txt`.

## Other checks

- `stream_invariants` — sequence numbers, one terminal event, every added item is done, deltas add up
  to the final snapshot.
- `stream_responses_match` also replays each stream in 7-byte chunks and expects the same events.
- `every_case_is_well_formed` — no missing or stray files; lists the cases still to be recorded.
