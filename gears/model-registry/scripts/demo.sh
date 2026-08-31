#!/usr/bin/env bash
# Walks all 12 model-registry P1 endpoints against a running server.
#
# Two zones: /model-registry/v1 is the eval-facing read surface, keyed by
# canonical_id; /model-registry/v1/admin is the management surface, keyed by
# UUID. `::` percent-encoding therefore appears on exactly one call — the eval
# read at step 7.
#
# Requires: curl, jq
# Server must be started with --features model-registry,static-authn,static-authz,static-tenants,static-credstore
# and config/quickstart.yaml (auth_disabled: true, prefix_path: /cf).
#
# Override the base URL by exporting BASE_URL before running.

set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8087/cf/model-registry/v1}"
ADMIN_URL="${BASE_URL}/admin"

PROVIDER_SLUG="demo-openai"
PROVIDER_MODEL_ID="gpt-4o-mini"
PROVIDER_NAME="Demo OpenAI"

step() {
    printf '\n\033[1;36m== %s ==\033[0m\n' "$1"
}

# ── 1. POST /admin/providers ────────────────────────────────────────────────
step "POST /admin/providers — create provider"

PROVIDER_PAYLOAD=$(cat <<JSON
{
  "slug": "${PROVIDER_SLUG}",
  "name": "${PROVIDER_NAME}",
  "gts_type": "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~",
  "managed": false,
  "metadata": { "region": "us-east-1" },
  "discovery_enabled": false
}
JSON
)

PROVIDER_JSON=$(curl -fsS -X POST "${ADMIN_URL}/providers" \
    -H 'Content-Type: application/json' \
    -d "${PROVIDER_PAYLOAD}")
echo "${PROVIDER_JSON}" | jq .

PROVIDER_ID=$(echo "${PROVIDER_JSON}" | jq -r .id)

# ── 2. GET /admin/providers — list with OData filter ───────────────────────
step "GET /admin/providers — list with OData filter"

curl -fsS --get "${ADMIN_URL}/providers" \
    --data-urlencode "\$filter=slug eq '${PROVIDER_SLUG}'" \
    --data-urlencode "\$limit=10" | jq .

# ── 3. GET /admin/providers/{id} ────────────────────────────────────────────
step "GET /admin/providers/{id} — fetch by id"

curl -fsS "${ADMIN_URL}/providers/${PROVIDER_ID}" | jq .

# ── 4. PATCH /admin/providers/{id} ──────────────────────────────────────────
step "PATCH /admin/providers/{id} — change name + clear metadata"

curl -fsS -X PATCH "${ADMIN_URL}/providers/${PROVIDER_ID}" \
    -H 'Content-Type: application/json' \
    -d '{ "name": "Demo OpenAI (renamed)", "metadata": null }' | jq .

# ── 5. POST /admin/models ───────────────────────────────────────────────────
step "POST /admin/models — create model (provider addressed by id)"

# Full ModelInfoV1 envelope — mirrors the SDK schema and the integration test
# fixture at gears/model-registry/model-registry/tests/integration.rs.
MODEL_INFO=$(cat <<JSON
{
  "gts_type": "gts.cf.genai.model.info.v1~",
  "display_name": "Demo GPT-4o mini",
  "description": null,
  "family": "gpt-4o",
  "vendor": "OpenAI",
  "managed": false,
  "architecture": "transformer",
  "size_bytes": null,
  "format": "api-only",
  "region": "us-east-1",
  "hosted_by": "OpenAI",
  "last_release_at": null,
  "reasoning_level": null,
  "version": "2024-07-18",
  "sort_order": 0,
  "icon": null,
  "multiplier_display": "1x",
  "performance": { "response_latency_ms": null, "tokens_per_second": null },
  "additional_info": {},
  "supported_api": ["completion"],
  "provider_model_id": "${PROVIDER_MODEL_ID}",
  "capabilities": {
    "vision": { "enabled": true, "supported_mime_types": ["image/jpeg", "image/png"] },
    "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
    "function_calling": true,
    "response_schema": true,
    "streaming": true,
    "file_input": { "enabled": false, "supported_mime_types": [] },
    "image_generation": { "enabled": false, "supported_mime_types": [] },
    "audio_input": { "enabled": false, "supported_mime_types": [] },
    "audio_output": { "enabled": false, "supported_mime_types": [] },
    "code_interpreter": false,
    "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
  },
  "disabled_capabilities": {
    "vision": { "disabled": false, "disabled_mime_types": [] },
    "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
    "function_calling": false,
    "response_schema": false,
    "streaming": false,
    "file_input": { "disabled": false, "disabled_mime_types": [] },
    "image_generation": { "disabled": false, "disabled_mime_types": [] },
    "audio_input": { "disabled": false, "disabled_mime_types": [] },
    "audio_output": { "disabled": false, "disabled_mime_types": [] },
    "code_interpreter": false,
    "web_search": { "disabled": false, "allowed_domains": false, "excluded_domains": false }
  },
  "context_window": { "max_input_tokens": 128000, "max_output_tokens": 16384 },
  "default_parameters": {
    "temperature": 0.7,
    "top_p": null,
    "max_output_tokens": null,
    "max_tool_calls": null,
    "presence_penalty": null,
    "frequency_penalty": null,
    "top_logprobs": null,
    "truncation": null,
    "service_tier": null,
    "parallel_tool_calls": null,
    "text": null,
    "reasoning": null,
    "tool_choice": null,
    "store": null
  },
  "allow_parameter_override": false,
  "allow_extra_params": [],
  "provider_settings": {}
}
JSON
)

MODEL_PAYLOAD=$(jq -n \
    --arg pid "${PROVIDER_ID}" \
    --argjson info "${MODEL_INFO}" \
    '{ provider_id: $pid, lifecycle_status: "production", approval_status: "pending", info: $info }')

MODEL_JSON=$(curl -fsS -X POST "${ADMIN_URL}/models" \
    -H 'Content-Type: application/json' \
    -d "${MODEL_PAYLOAD}")
echo "${MODEL_JSON}" | jq .

# The server derives canonical_id from the provider's slug; the id is the handle
# every management call below uses.
MODEL_ID=$(echo "${MODEL_JSON}" | jq -r .id)
CANONICAL_ID=$(echo "${MODEL_JSON}" | jq -r .canonical_id)
CANONICAL_ID_ENC=$(jq -rn --arg v "${CANONICAL_ID}" '$v|@uri')

# ── 6. GET /models — list with OData filter on canonical_id ─────────────────
step "GET /models — list with OData filter"

curl -fsS --get "${BASE_URL}/models" \
    --data-urlencode "\$filter=canonical_id eq '${CANONICAL_ID}'" \
    --data-urlencode "\$limit=10" | jq .

# ── 7. GET /models/{canonical_id} — the one slug-keyed path ─────────────────
# Eval-facing and gated: the model is still `pending` here, so this returns 403
# `model_not_approved`. Step 9 reads the same row by id with no gates.
step "GET /models/{canonical_id} — eval read (:: url-encoded; expect 403 pending)"

EVAL_HTTP=$(curl -sS -o /tmp/mr_eval_resp.json -w '%{http_code}' \
    "${BASE_URL}/models/${CANONICAL_ID_ENC}" || true)
echo "HTTP ${EVAL_HTTP}"
if [ -s /tmp/mr_eval_resp.json ]; then jq . /tmp/mr_eval_resp.json; fi

# ── 8. GET /admin/models — management listing ───────────────────────────────
step "GET /admin/models — management listing with visibility flags"

curl -fsS --get "${ADMIN_URL}/models" \
    --data-urlencode "\$filter=canonical_id eq '${CANONICAL_ID}'" \
    --data-urlencode "\$limit=10" | jq .

# ── 9. GET /admin/models/{id} — management read ─────────────────────────────
# Ungated: the model created above is still `pending`, so the eval read at
# step 7 refuses it while this one returns it.
step "GET /admin/models/{id} — fetch by id (no eval gates)"

curl -fsS "${ADMIN_URL}/models/${MODEL_ID}" | jq .

# ── 10. PATCH /admin/models/{id} — change approval + lifecycle ──────────────
step "PATCH /admin/models/{id} — approve + activate"

curl -fsS -X PATCH "${ADMIN_URL}/models/${MODEL_ID}" \
    -H 'Content-Type: application/json' \
    -d '{ "approval_status": "approved", "lifecycle_status": "production" }' | jq .

# ── 11. DELETE /admin/models/{id} — soft delete ─────────────────────────────
step "DELETE /admin/models/{id} — soft delete (expect 204)"

curl -fsS -X DELETE -o /dev/null -w 'HTTP %{http_code}\n' \
    "${ADMIN_URL}/models/${MODEL_ID}"

# ── 12. DELETE /admin/providers/{id} ────────────────────────────────────────
# Soft-delete on the model retains the FK row, so the provider delete may
# either succeed (if you hard-delete the model first) or return 400
# `failed_precondition` because the (soft-deleted) model still references it.
# Both are valid outcomes — the demo just demonstrates the call lands on the
# route.
step "DELETE /admin/providers/{id} — expect 204 or 400 (model still references it)"

DEL_HTTP=$(curl -sS -o /tmp/mr_delete_resp.json -w '%{http_code}' \
    -X DELETE "${ADMIN_URL}/providers/${PROVIDER_ID}" || true)
echo "HTTP ${DEL_HTTP}"
if [ -s /tmp/mr_delete_resp.json ]; then jq . /tmp/mr_delete_resp.json; fi

if [ "${DEL_HTTP}" != "204" ] && [ "${DEL_HTTP}" != "400" ]; then
    echo "expected 204 or 400, got ${DEL_HTTP}" >&2
    exit 1
fi

printf '\n\033[1;32mAll 12 endpoints hit successfully.\033[0m\n'
