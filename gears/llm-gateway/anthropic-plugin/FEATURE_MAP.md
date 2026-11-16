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
| 2 | Output blocks | Reasoning | Supported | `thinking` → `reasoning` item with the text in `content` and the signature in `encrypted_content`. `redacted_thinking` → `reasoning` item with `encrypted_content` only. |
| 3 | Output blocks | Tool calls | Supported | `tool_use` → `function_call` item. |
| 4 | Output blocks | Web search results | Supported | `server_tool_use` (web search) and `web_search_tool_result` → one `cf_gears:web_search_call` item with `queries` and `results`. `web_search_result_location` citations → `url_citation` annotations spanning the whole text part. |
| 5 | Output blocks | Other blocks | Supported | Any other block → `anthropic:<type>` item, kept verbatim. |
| 6 | Streaming | Text | Supported | `text_delta` → `response.output_text.delta`. |
| 7 | Streaming | Reasoning | Supported | `thinking_delta` → `response.reasoning.delta`. Thinking text is requested with `reasoning.summary` (see 33). |
| 8 | Streaming | Tool call arguments | Supported | `input_json_delta` → `response.function_call_arguments.delta`. |
| 9 | Streaming | Web search and other blocks | Supported | Emitted whole when the block stops. A web search item is added as `in_progress` when the call block stops and is done when its result block arrives or the stream ends. |
| 10 | Streaming | Terminal event | Supported | Exactly one of `response.completed`, `response.incomplete`, `response.failed`. The final snapshot equals the non-streaming response for the same blocks. |
| 11 | Input | Text | Supported | `input_text` and string content → `text` block. |
| 12 | Input | Images by URL or base64 | Supported | `input_image.image_url` (http(s) URL or base64 data URI) → `image` block. |
| 13 | Input | Files by URL or base64 | Supported | `input_file.file_url` and `input_file.file_data` (data URI, or bare base64 taken as PDF) → `document` block with the given media type. Accepted file types are declared in the model capabilities. |
| 14 | Input | Images and files by `file_id` | Not supported by the plugin | Rejected with `capability_not_supported`. |
| 15 | Input | Audio and video | Not supported by Anthropic | Rejected with `capability_not_supported`. |
| 16 | Input | Conversation history | Supported | Assistant messages, `function_call`, `function_call_output`, and `reasoning` items with a signature map to assistant and user blocks. `anthropic:<type>` items are forwarded verbatim. |
| 17 | Input | `cf_gears:web_search_call` items | Not supported by the plugin | Dropped from the input. |
| 18 | Input | `item_reference` | Not supported by Anthropic | Rejected with `capability_not_supported`. |
| 19 | User-defined tools | Function tools with a schema | Supported | `name`, `description`, `parameters` → `input_schema`, `strict`. The schema is forwarded verbatim. |
| 20 | Anthropic built-in tools | `anthropic:<type>` tools | Supported | Forwarded verbatim with the prefix stripped. |
| 21 | Anthropic built-in tools | Allowing built-in tools per model | Not supported by LLM Gateway | Model Registry has no per-model allow-list for provider tools, and `ProviderPluginCapabilities` has no tool-namespace field. |
| 22 | `cf_gears:web_search` | Web search tool | Supported | → `web_search_20250305` with `allowed_domains`, `blocked_domains`, `max_uses`, `user_location`. |
| 23 | `cf_gears:web_search` | Other web search options | Not supported by Anthropic | Dropped without an entry in `ignored_parameters`. |
| 24 | `cf_gears:web_search` | Resuming a paused search turn | Not supported by the plugin | `pause_turn` maps to status `completed`, and web search items are dropped from the input. |
| 25 | Tool choice | `auto`, `required`, `none`, named function | Supported | → `auto`, `any`, `none`, `tool`. Sent only when the request has tools; falls back to the settings `tool_choice`. |
| 26 | Tool choice | Forcing a non-function tool | Not supported by LLM Gateway | `NamedToolChoice` has only the function variant. |
| 27 | Tool choice | Parallel tool calls | Supported | `parallel_tool_calls: false` → `disable_parallel_tool_use: true`. |
| 28 | Tool choice | `max_tool_calls` | Not supported by Anthropic | Unsupported parameter. |
| 29 | Reasoning | Effort `low` to `xhigh` | Supported | → `thinking: adaptive` and `output_config.effort`. |
| 30 | Reasoning | Effort `none` | Supported | → `thinking: disabled`. |
| 31 | Reasoning | Effort `max` | Not supported by LLM Gateway | `ReasoningEffort` ends at `xhigh`. `max` is available as the settings default `output_config.effort`. |
| 32 | Reasoning | Budget | Supported | `budget_tokens` → `thinking: enabled` with that budget. |
| 33 | Reasoning | Summary | Supported | `summary` → `display: summarized`. Without effort or budget it applies to the settings default thinking, or to adaptive thinking when there is no default; a default of `disabled` stays disabled. |
| 34 | Reasoning | Settings defaults | Supported | Without `reasoning` in the request, the settings `thinking` and `output_config.effort` apply. |
| 35 | Max output tokens | `max_output_tokens` | Supported | → `max_tokens`, falling back to the settings `max_tokens`, then the model's output limit. A request with none of the three is rejected with `validation_error`. |
| 36 | System instructions | Instructions | Supported | Settings `system`, then `instructions`, then system and developer messages → `system`. System and developer messages accept only text. |
| 37 | Structured output | `text` | Supported | No `output_config.format` is sent. |
| 38 | Structured output | `json_schema` | Supported | → `output_config.format`, with the schema adapted to Anthropic's subset (see Schema adaptation). Falls back to the settings `output_config.format`, which is forwarded verbatim. |
| 39 | Structured output | `json_object` | Not supported by Anthropic | Rejected with `capability_not_supported`. |
| 40 | General settings | `temperature`, `top_p` | Not supported by Anthropic | Unsupported parameters. |
| 41 | General settings | `top_k` | Not supported by Anthropic | No request field and no settings field. |
| 42 | General settings | `seed`, `presence_penalty`, `frequency_penalty` | Not supported by Anthropic | Unsupported parameters. |
| 43 | General settings | `top_logprobs`, `include: message.output_text.logprobs` | Not supported by Anthropic | Unsupported parameters. |
| 44 | General settings | `text.verbosity` | Not supported by Anthropic | Dropped without an entry in `ignored_parameters`. |
| 45 | General settings | `service_tier` | Supported | `auto` → `auto`, `default` → `standard_only`; falls back to the settings. |
| 46 | General settings | `safety_identifier` | Supported | → `metadata.user_id`. |
| 47 | Truncation | `truncation: disabled` | Supported | Nothing is sent. |
| 48 | Truncation | `truncation: auto` | Not supported by Anthropic | Unsupported parameter. |
| 49 | Cache settings | Prompt caching | Supported | A `prompt_cache_key` → top-level `cache_control: ephemeral`. Cache-read tokens → `usage.input_tokens_details.cached_tokens`. |
| 50 | Cache settings | Cache TTL and breakpoint placement | Not supported by LLM Gateway | `prompt_cache_key` is the only cache field. |
| 51 | Cache settings | Cache-creation token count | Not supported by LLM Gateway | Available only in `usage.provider_raw`. |
| 52 | Stop words | Stop words per request | Not supported by LLM Gateway | `CreateResponseBody` has no `stop` field, and the response has no field for the matched sequence. |
| 53 | Stop words | Stop words per model | Supported | Settings `stop_sequences` → `stop_sequences`. |
| 54 | Extra fields | Forwarding `extra_fields` | Supported | Each entry is merged into the top level of the request body and overrides a typed field of the same name. |
| 55 | Effort | Effort with reasoning | Supported | `reasoning.effort` → `output_config.effort`, together with `thinking: adaptive` (see 29). |

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
  `validation_error`.