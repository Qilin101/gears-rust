<!-- Created: 2026-11-16 by Constructor Tech -->
# Anthropic Plugin Feature Map

Completion API features and their support in the Anthropic plugin, which
maps Open Responses requests to the Anthropic Messages API
(`POST /v1/messages`, API version `2026-10-21`).

Status terms:

- **Supported** — the plugin maps the feature to the Anthropic API.
- **Not supported by Anthropic** — the Anthropic API has no equivalent.
- **Not supported by LLM Gateway** — the LLM Gateway SDK has no way to
  express or enforce it.
- **Not supported by the plugin** — Anthropic and LLM Gateway both allow it;
  the plugin does not implement it.

A request parameter that is not supported by Anthropic is an unsupported
parameter: it is rejected with `unsupported_parameter`, or with
`drop_unsupported_params` it is dropped and listed in `ignored_parameters`.

## Feature map

| # | Area | Feature | Status | Mapping |
|---|---|---|---|---|
| 1 | Output blocks | Text | Supported | `text` block → `message` item with an `output_text` part. Consecutive text blocks share one message item. |
| 2 | Output blocks | Reasoning | Supported | `thinking` → `reasoning` item with the text in `content` (summarized thinking included) and the signature in `encrypted_content`; omitted thinking text is an empty `reasoning_text` part, so the item replays as a `thinking` block. `redacted_thinking` → `reasoning` item with `encrypted_content` only. |
| 3 | Output blocks | Tool calls | Supported | `tool_use` → `function_call` item; `arguments` is `input` as JSON text. |
| 4 | Output blocks | Web search results | Supported | `server_tool_use` (web search) and `web_search_tool_result` → one `cf_gears:web_search_call` item with `queries` and `results`; without a result block the item is `incomplete`. `web_search_result_location` citations → `url_citation` annotations spanning the whole text part. |
| 5 | Output blocks | Other blocks | Supported | Any other block → `anthropic:<type>` item, kept verbatim. |
| 6 | Streaming | Text | Supported | `text_delta` → `response.output_text.delta`. |
| 7 | Streaming | Reasoning | Supported | `thinking_delta` → `response.reasoning.delta`. Thinking text is requested with `reasoning.summary` (see 45). |
| 8 | Streaming | Tool call arguments | Supported | `input_json_delta` → `response.function_call_arguments.delta`; `function_call_arguments.done` carries the deltas joined. A call without input sends `{}` as its only delta. |
| 9 | Streaming | Web search and other blocks | Supported | Emitted whole when the block stops. A web search item is added as `in_progress` when the call block stops and is done when its result block arrives or the stream ends. |
| 10 | Streaming | Terminal event | Supported | Exactly one of `response.completed`, `response.incomplete`, `response.failed`. The final snapshot equals the non-streaming response for the same blocks. Empty deltas are not sent. |
| 11 | Streaming | Failed stream | Supported | Before `response.failed`, every open item is done; a block cut off mid-stream is kept in the output as `incomplete`. |
| 12 | Status | Stop reasons | Supported | `end_turn`, `stop_sequence`, `tool_use` → `completed`; `max_tokens`, `model_context_window_exceeded`, `pause_turn` → `incomplete` (`max_output_tokens`); `refusal` → `incomplete` (`content_filter`), or a `content_filtered` error when nothing was produced. The last item of an incomplete response is `incomplete`. The original stop reason is in `provider_details`. |
| 13 | Status | Refusal details | Not supported by LLM Gateway | `stop_details` `category` and `explanation` have no response field; a refusal without output carries the explanation as the error's provider message. |
| 14 | Response | Echoed request fields | Supported | `max_output_tokens`, `tool_choice`, and `parallel_tool_calls` report the values sent, settings fallbacks included; `service_tier` reports Anthropic's `usage.service_tier` (`standard` → `default`). Other fields echo the request. |
| 15 | Input | Text | Supported | `input_text` and string content → `text` block. |
| 16 | Input | Images by URL or base64 | Supported | `input_image.image_url` (http(s) URL or base64 data URI) → `image` block. |
| 17 | Input | Image `detail` | Not supported by Anthropic | `low` and `high` are unsupported parameters; `auto` is accepted. |
| 18 | Input | Files by URL or base64 | Supported | `input_file.file_url` (http(s) URL or base64 data URI) and `input_file.file_data` (base64 data URI only) → `document` block, `filename` → `title`. A data URI keeps its media type; Anthropic takes base64 and URL documents as PDF only. `file_data` in any other format is rejected with `validation_error`. Accepted file types are declared in the model capabilities. |
| 19 | Input | Images and files by `file_id` | Not supported by the plugin | Rejected with `capability_not_supported`. |
| 20 | Input | Audio and video | Not supported by Anthropic | Rejected with `capability_not_supported`. |
| 21 | Input | Conversation history | Supported | Assistant messages, `function_call`, `function_call_output`, and `reasoning` items with a signature map to assistant and user blocks. `function_call` arguments must be a JSON object. Assistant `output_text` and `refusal` parts replay as text; other output parts are rejected with `validation_error`. Each `message` item is its own message; the other items join the adjacent message of the same role. |
| 22 | Input | `anthropic:<type>` items | Supported | Forwarded verbatim with the prefix stripped, as blocks of the assistant turn. |
| 23 | Input | Other item types | Not supported by the plugin | An unknown type or another provider's prefix is rejected with `validation_error`, `param` `input[<i>]`. |
| 24 | Input | `cf_gears:web_search_call` items | Not supported by LLM Gateway | Dropped from the input: replaying a search needs each result's `encrypted_content`, which `WebSearchResult` lacks. |
| 25 | Input | `item_reference` | Not supported by Anthropic | Rejected with `capability_not_supported`. |
| 26 | System instructions | Instructions | Supported | Settings `system`, then `instructions`, then system and developer messages → `system`. System and developer messages accept only text; other content is rejected with `validation_error`. |
| 27 | User-defined tools | Function tools with a schema | Supported | `name`, `description`, `parameters` → `input_schema`, `strict`. The schema is forwarded verbatim; without `parameters` it is `{"type": "object"}`. |
| 28 | Anthropic built-in tools | `anthropic:<type>` tools the consumer runs | Supported | `bash_*`, `text_editor_*`, `memory_*`, `computer_*`: forwarded verbatim with the prefix stripped. Their calls come back as `function_call` items. |
| 29 | Anthropic built-in tools | `anthropic:<type>` tools Anthropic runs | Not supported by the plugin | Code execution, web fetch, tool search, and other server tools keep state or cost at Anthropic: rejected with `capability_not_supported`. Web search goes through `cf_gears:web_search`. |
| 30 | Anthropic built-in tools | Allowing built-in tools per model | Not supported by LLM Gateway | Model Registry has no per-model allow-list for provider tools, and `ProviderPluginCapabilities` has no tool-namespace field. |
| 31 | Other tools | `cf_gears:image_generation` | Not supported by Anthropic | Rejected with `capability_not_supported`. |
| 32 | Other tools | Other tool types | Not supported by the plugin | An unknown type or another provider's prefix is rejected with `validation_error`, `param` `tools[<i>]`. |
| 33 | `cf_gears:web_search` | Web search tool | Supported | → `web_search_20250305` with `allowed_domains`, `blocked_domains`, `max_uses`, `user_location`. Anthropic does not accept `allowed_domains` and `blocked_domains` together. |
| 34 | `cf_gears:web_search` | Other web search options | Not supported by Anthropic | Unsupported parameter, named `tools[<i>].<option>`. |
| 35 | `cf_gears:web_search` | Resuming a paused search turn | Not supported by LLM Gateway | `pause_turn` maps to `incomplete` (see 12), and web search items are dropped from the input (see 24). |
| 36 | Tool choice | `auto`, `required`, `none`, named function | Supported | → `auto`, `any`, `none`, `tool`. Sent only when the request has tools; falls back to the settings `tool_choice`. `required` or a named function without tools, or a name that is not a function tool, is rejected with `validation_error`. With parallel calls allowed, a named choice can still yield further calls. |
| 37 | Tool choice | Forced choice with thinking | Not supported by Anthropic | `any` or `tool` while thinking is on is an unsupported parameter (`tool_choice`); dropped, it becomes `auto`. |
| 38 | Tool choice | Forcing a non-function tool | Not supported by LLM Gateway | `NamedToolChoice` has only the function variant. |
| 39 | Tool choice | Parallel tool calls | Supported | `parallel_tool_calls: false` → `disable_parallel_tool_use: true`. |
| 40 | Tool choice | `max_tool_calls` | Not supported by Anthropic | Unsupported parameter. |
| 41 | Reasoning | Effort `low` to `xhigh` | Supported | → `thinking: adaptive` and `output_config.effort`. |
| 42 | Reasoning | Effort `none` | Supported | → `thinking: disabled`. Combined with `budget_tokens` it is rejected with `validation_error`. |
| 43 | Reasoning | Effort `max` | Not supported by LLM Gateway | `ReasoningEffort` ends at `xhigh`. `max` is available as the settings default `output_config.effort`. |
| 44 | Reasoning | Budget | Supported | `budget_tokens` → `thinking: enabled` with that budget, plus `output_config.effort` when an effort is set. Newer models, such as Claude Haiku 5.5, reject `thinking: enabled`. |
| 45 | Reasoning | Summary | Supported | `summary` → `display: summarized`. Without effort or budget it applies to the settings default thinking, or to adaptive thinking when there is no default; a default of `disabled` stays disabled. |
| 46 | Reasoning | Settings defaults | Supported | Without `reasoning` in the request, the settings `thinking` and `output_config.effort` apply. |
| 47 | Max output tokens | `max_output_tokens` | Supported | → `max_tokens`, falling back to the settings `max_tokens`, then the model's output limit. A request with none of the three is rejected with `validation_error`. |
| 48 | Structured output | `text` | Supported | No `output_config.format` is sent. |
| 49 | Structured output | `json_schema` | Supported | → `output_config.format`, with the schema adapted to Anthropic's subset (see Schema adaptation). Falls back to the settings `output_config.format`, which is forwarded verbatim. |
| 50 | Structured output | `json_object` | Not supported by Anthropic | Rejected with `capability_not_supported`. |
| 51 | General settings | `temperature`, `top_p` | Not supported by Anthropic | Unsupported parameters. |
| 52 | General settings | `top_k` | Not supported by Anthropic | No request field and no settings field. |
| 53 | General settings | `seed`, `presence_penalty`, `frequency_penalty` | Not supported by Anthropic | Unsupported parameters. |
| 54 | General settings | `top_logprobs`, `include: message.output_text.logprobs` | Not supported by Anthropic | Unsupported parameters. `include: reasoning.encrypted_content` is accepted; signatures are always returned. |
| 55 | General settings | `text.verbosity` | Not supported by Anthropic | Unsupported parameter. |
| 56 | General settings | `service_tier` | Supported | `auto` → `auto`, `default` → `standard_only`; falls back to the settings. |
| 57 | General settings | `safety_identifier` | Supported | → `metadata.user_id`. |
| 58 | General settings | Model settings | Supported | Settings `inference_geo` and `container` are forwarded as-is. |
| 59 | Truncation | `truncation: disabled` | Supported | Nothing is sent. |
| 60 | Truncation | `truncation: auto` | Not supported by Anthropic | Unsupported parameter. |
| 61 | Cache settings | Prompt caching | Supported | A `prompt_cache_key` → top-level `cache_control: ephemeral`. Cache-read tokens → `usage.input_tokens_details.cached_tokens`. |
| 62 | Cache settings | Cache TTL and breakpoint placement | Not supported by LLM Gateway | `prompt_cache_key` is the only cache field. |
| 63 | Cache settings | Cache-creation token count | Not supported by LLM Gateway | Available only in `usage.provider_raw`. |
| 64 | Stop words | Stop words per request | Not supported by LLM Gateway | `CreateResponseBody` has no `stop` field, and the response has no field for the matched sequence. |
| 65 | Stop words | Stop words per model | Supported | Settings `stop_sequences` → `stop_sequences`. |
| 66 | Extra fields | Forwarding `extra_fields` | Supported | Each entry is added to the top level of the request body. An entry for a field the plugin builds (`model`, `messages`, `max_tokens`, `stream`, `system`, `tools`, `tool_choice`, `thinking`, `output_config`, ...) is rejected with `validation_error`, `param` `extra_fields.<key>`. |

## Schema adaptation

`text.format` JSON schemas are adapted like `transform_schema` of the
Anthropic Python SDK:

- objects get `additionalProperties: false`;
- `oneOf` becomes `anyOf`;
- string `format` is kept for `date-time`, `time`, `date`, `duration`,
  `email`, `hostname`, `uri`, `ipv4`, `ipv6`, `uuid`;
- `minItems` is kept for 0 and 1;
- every other unsupported keyword moves into `description`;
- a schema node that is not an object, or has none of `type`, `anyOf`,
  `oneOf`, `allOf`, `enum`, `const`, `$ref`, is rejected with
  `unsupported_schema`.

## Errors

An Anthropic error reply maps by its `error.type`, or by the HTTP status when
the type is unknown or the body is not an Anthropic error:

| Gateway error | Anthropic `error.type` | HTTP status, when the type is unknown |
|---|---|---|
| `context_length_exceeded` | `invalid_request_error` about the prompt length or context window | — |
| `invalid_request` | other `invalid_request_error`, `not_found_error`, `request_too_large` | 400, 404, 413, 422 |
| `authentication_failed` | `authentication_error`, `permission_error` | 401, 403 |
| `rate_limited` | `rate_limit_error` | 429 |
| `provider_unavailable` | `overloaded_error`, `api_error`, `timeout_error` | 408, 5xx |
| `provider_error` | `billing_error` | any other |

- The error carries `provider_code` (`error.type`), `provider_message`
  (`error.message`), and `provider_request_id` (the body `request_id`, else
  the `request-id` header).
- `retry_after` comes from `retry-after-ms`, rounded up to whole seconds, else
  from `retry-after`.
- An SSE `error` event fails the stream with the same mapping.
- A 2xx reply that is not a valid Message, or a non-SSE reply to a streaming
  request, is `provider_error`.
- A stream that breaks off or ends before `message_stop` fails with
  `provider_unavailable`.
- OAGW failures: a missing route or upstream, or a denied destination, is
  `unapproved_destination`; OAGW rate limiting is `rate_limited`; a timeout or
  unavailable upstream is `provider_unavailable`; failed upstream
  authentication is `authentication_failed`.
