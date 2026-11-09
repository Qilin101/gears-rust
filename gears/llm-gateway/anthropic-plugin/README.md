<!-- Created: 2026-11-09 by Constructor Tech -->
# LLM Gateway Anthropic Plugin

Provider plugin for the [LLM Gateway](../README.md) that serves models whose
provider type is `gts.cf.genai.model.provider.v1~cf.genai._.anthropic.v1~`.

`AnthropicPlugin` implements `LlmGatewayProviderPluginClientV1`:

- `create_response` and `create_response_stream` translate the Open Responses
  request to `POST /v1/messages`, call Anthropic through OAGW, and translate
  the message or SSE stream back.
- `create_embedding` returns `capability_not_supported`.

Cost in AI credits is not computed yet (`usage.cost` is `0`); pricing comes from
the generic `ModelInfoV1.cost`. Gear
registration (GTS plugin instance, scoped ClientHub client) is not wired yet.

## Provider settings

The crate defines the provider type `AnthropicProviderSettingsV1`, derived
from Model Registry's `ProviderTypeV1`; it is the shape of a model's
`provider_settings`:

- `oagw_alias` — OAGW upstream alias; requests go to `/{oagw_alias}/v1/messages`.
- `anthropic_version` — `anthropic-version` header (defaults to `2026-10-21`).
- `anthropic_beta` — joined into the `anthropic-beta` header.
- Inference defaults (`temperature`, `top_p`, `top_k`, `max_tokens`,
  `stop_sequences`, `system`, `thinking`, `tool_choice`, `output_config`, …)
  apply when the request leaves the field unset.

## OAGW upstream

The provider API key from `ProviderCallCtx` is sent as
`Authorization: Bearer <key>`; without a key no auth header is sent and the
upstream must authenticate the call itself. OAGW strips caller headers by
default, so the Anthropic upstream must allow passthrough of `authorization`,
`anthropic-version`, `anthropic-beta`, and `accept`.

## Mapping notes

- Provider-specific tools, input items, and output items use the `anthropic:`
  prefix (`anthropic:web_fetch_20250910`, `anthropic:server_tool_use`) and are
  forwarded verbatim with the prefix stripped.
- `cf_gears:web_search` maps to `web_search_20250305`; results come back as
  `cf_gears:web_search_call` items and `url_citation` annotations.
- Thinking blocks become `reasoning` items (`content` + `encrypted_content` =
  signature); redacted thinking has `encrypted_content` only. Both round-trip
  when sent back as input.
