<!-- Created: 2026-11-09 by Constructor Tech -->
# LLM Gateway Anthropic Plugin

Provider plugin for the [LLM Gateway](../README.md) that serves models whose
provider type is `gts.cf.genai.model.provider.v1~cf.genai._.anthropic.v1~`.

`AnthropicPlugin` implements `LlmGatewayProviderPluginClientV1`:

- `create_response` and `create_response_stream` translate the Open Responses
  request to `POST /v1/messages`, call Anthropic through OAGW, and translate
  the message or SSE stream back.
- `create_embedding` returns `capability_not_supported`.
- `forward_passthrough` forwards native Anthropic requests
  ([Pass-through](#pass-through)).

The plugin reports usage counts (input, output, cache read and write, reasoning
tokens, web search calls) and leaves `usage.cost` at `0`; the Gateway core
prices the call from `ModelInfoV1.cost`. Gear registration (GTS plugin
instance, scoped ClientHub client) is not wired yet.

## Provider settings

The crate defines the provider type `AnthropicProviderSettingsV1`, derived
from Model Registry's `ProviderTypeV1`; it is the shape of a model's
`provider_settings`:

- `anthropic_version` — `anthropic-version` header (defaults to `2026-10-21`).
- `anthropic_beta` — joined into the `anthropic-beta` header; flags outside
  the [beta allowlist](#beta-allowlist) are dropped.
- Inference defaults (`max_tokens`, `stop_sequences`, `system`, `thinking`, `tool_choice`, `output_config`, …)
  apply when the request leaves the field unset.

## OAGW upstream

The destination is the model's `base_url`, or `https://api.anthropic.com` when
it is absent. Its host names the OAGW upstream alias, with the port appended
unless it is 80 or 443; a path prefix is kept. Requests go to
`/{alias}{prefix}/v1/messages`.

The plugin sends no credential: the OAGW upstream must authenticate the call
itself. A model with a `secret_ref` fails with `credentials_unavailable` and
nothing is sent, until OAGW can be told which credential to use.

OAGW strips caller headers by default, so the upstream must allow passthrough
of `anthropic-version`, `anthropic-beta`, and `accept`.

## Pass-through

The plugin declares two pass-through endpoints: `POST /v1/messages`
(metered) and `POST /v1/messages/count_tokens` (not metered: its response
carries no usage).

- The body is sent as received, with `model` replaced by the model's
  `provider_model_id`; the response and the `message_start` event get the
  Gateway model identifier back. Provider settings defaults are not applied.
- `anthropic-version` and `anthropic-beta` come from the request; without an
  `anthropic-version` the model's setting or the default is sent. Flags
  outside the [beta allowlist](#beta-allowlist) are dropped. The model's
  `anthropic_beta` setting is not added.
- A provider error response is relayed with its status and body.
- Usage is read from the response, or from `message_start` and
  `message_delta` in a stream.

### Stateless guard

A request that refers to state kept at Anthropic, or uses a provider tool
billed per call, is rejected with `capability_not_supported` and the JSON
path of the field as `param`; nothing is sent:

- a `file_id`, `container`, or `vector_store_ids` key at any depth;
- a block whose `source.type` is `file`, and `container_upload` blocks;
- `code_execution_*` and `web_search_*` tools.

Subtrees the consumer defines are not scanned: `input_schema`, `metadata`,
`output_config.format.schema`, and the `input` of `tool_use` blocks.

The guard applies to both pass-through endpoints.

### Beta allowlist

`anthropic-beta` flags are forwarded only when allowlisted, on the
pass-through header and from the model's `anthropic_beta` setting; any other
flag is dropped. Allowlisted flags enable features that keep no state at
Anthropic, call no other model or service, and are priced at the model's
rates:

- `compact-2026-01-12`, `compact-2026-09-04`
- `computer-use-2025-01-24`, `computer-use-2025-11-24`
- `context-management-2025-06-27`
- `effort-2025-11-24`
- `fine-grained-tool-streaming-2025-05-14`
- `inline-tools-2026-09-15`
- `interleaved-thinking-2025-05-14`
- `mid-conversation-output-config-2026-07-01`,
  `mid-conversation-system-clear-at-2026-08-21`,
  `mid-conversation-tool-changes-2026-07-01`
- `output-128k-2025-02-19`
- `structured-outputs-2025-11-13`
- `task-budgets-2026-03-13`
- `thinking-binding-controls-2026-08-01`
- `thinking-display-updates-2026-08-18`
- `token-efficient-tools-2025-02-19`

## Mapping notes

- Provider-specific tools, input items, and output items use the `anthropic:`
  prefix (`anthropic:bash_20250124`, `anthropic:server_tool_use`) and are
  forwarded verbatim with the prefix stripped. Input items and tools pass the
  [stateless guard](#stateless-guard) first.
- `cf_gears:web_search` maps to `web_search_20250305`; results come back as
  `cf_gears:web_search_call` items and `url_citation` annotations.
- Thinking blocks become `reasoning` items (`content` + `encrypted_content` =
  signature); redacted thinking has `encrypted_content` only. Both round-trip
  when sent back as input.
