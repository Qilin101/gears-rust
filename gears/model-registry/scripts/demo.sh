#!/usr/bin/env bash
# Walks the twelve model-registry endpoints against a running server.
#
# Requires: curl, jq
# Server must be started with --features model-registry,static-authn,static-authz,static-tenants,static-credstore
# and config/quickstart.yaml (auth_disabled: true, prefix_path: /cf).
#
# Override the server prefix by exporting BASE_URL before running.

set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8087/cf}"
MR="${BASE_URL}/model-registry/v1"
TR="${BASE_URL}/types-registry/v1"

PROVIDER_TYPE="gts.cf.genai.model.provider.v1~cf.genai._.demo.v1~"
PROVIDER_SLUG="demo-openai"
PROVIDER_MODEL_ID="gpt-4o-mini"

step() {
    printf '\n\033[1;36m== %s ==\033[0m\n' "$1"
}

# ── 0. Register a demo provider type (what an LLM Gateway plugin does) ──────
step "POST /types-registry/v1/entities — register the demo provider type"

TYPE_SCHEMA=$(cat <<JSON
{
  "\$id": "gts://${PROVIDER_TYPE}",
  "\$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "description": "Demo provider type: provider_settings carries an OAGW alias and a cost table",
  "allOf": [
    { "\$ref": "gts://gts.cf.genai.model.provider.v1~" },
    {
      "type": "object",
      "properties": {
        "provider_settings": {
          "type": "object",
          "required": ["oagw_alias"],
          "properties": {
            "oagw_alias": { "type": "string" },
            "cost": {
              "type": "object",
              "properties": { "input_per_1k_micro": { "type": "integer" }, "output_per_1k_micro": { "type": "integer" } }
            }
          }
        }
      }
    }
  ]
}
JSON
)
curl -fsS -X POST "${TR}/entities" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --argjson s "${TYPE_SCHEMA}" '{ entities: [$s] }')" | jq .

# ── 1. POST /providers ──────────────────────────────────────────────────────
step "POST /providers — create a shared provider"

PROVIDER_JSON=$(curl -fsS -X POST "${MR}/providers" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg slug "${PROVIDER_SLUG}" --arg t "${PROVIDER_TYPE}" \
        '{ slug: $slug, name: "Demo OpenAI", gts_type: $t, sharing_mode: "shared", metadata: { region: "us-east-1" } }')")
echo "${PROVIDER_JSON}" | jq .
PROVIDER_ID=$(echo "${PROVIDER_JSON}" | jq -r .id)

# ── 2. GET /providers ───────────────────────────────────────────────────────
step "GET /providers — list with OData filter and \$select"

curl -fsS --get "${MR}/providers" \
    --data-urlencode "\$filter=slug eq '${PROVIDER_SLUG}'" \
    --data-urlencode "\$select=id,slug,enabled,inherited,shadowed,available_for_eval" \
    --data-urlencode "limit=10" | jq .

# ── 3. GET /providers/{id} ──────────────────────────────────────────────────
step "GET /providers/{id}"

curl -fsS "${MR}/providers/${PROVIDER_ID}" | jq .

# ── 4. PATCH /providers/{id} ────────────────────────────────────────────────
step "PATCH /providers/{id} — rename and clear metadata"

curl -fsS -X PATCH "${MR}/providers/${PROVIDER_ID}" \
    -H 'Content-Type: application/json' \
    -d '{ "name": "Demo OpenAI (renamed)", "metadata": null }' | jq .

# ── 5. POST /models ─────────────────────────────────────────────────────────
step "POST /models — settings that violate the provider type schema (expect 400 SCHEMA_MISMATCH)"

BAD_HTTP=$(curl -sS -o /tmp/mr_bad_resp.json -w '%{http_code}' -X POST "${MR}/models" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg pid "${PROVIDER_ID}" --arg pmid "${PROVIDER_MODEL_ID}" \
        '{ provider_id: $pid, provider_model_id: $pmid, display_name: "Demo", context_window: { max_input_tokens: 128000 }, provider_settings: { cost: { input_per_1k_micro: "free" } } }')" || true)
echo "HTTP ${BAD_HTTP}"
jq . /tmp/mr_bad_resp.json
[ "${BAD_HTTP}" = "400" ] || { echo "expected 400" >&2; exit 1; }

step "POST /models — create the model"

MODEL_JSON=$(curl -fsS -X POST "${MR}/models" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg pid "${PROVIDER_ID}" --arg pmid "${PROVIDER_MODEL_ID}" '{
        provider_id: $pid,
        provider_model_id: $pmid,
        display_name: "Demo GPT-4o mini",
        sharing_mode: "shared",
        vendor: "OpenAI",
        family: "gpt-4o",
        base_url: "https://api.openai.com/v1",
        secret_ref: "demo-openai-key",
        supported_api: ["completion"],
        capabilities: {
          vision: { enabled: true, supported_mime_types: ["image/jpeg", "image/png"] },
          reasoning: { effort: false, toggle: false, resume: false, budget: false },
          function_calling: true, response_schema: true, streaming: true,
          file_input: { enabled: false, supported_mime_types: [] },
          image_generation: { enabled: false, supported_mime_types: [] },
          audio_input: { enabled: false, supported_mime_types: [] },
          audio_output: { enabled: false, supported_mime_types: [] },
          code_interpreter: false,
          web_search: { enabled: false, allowed_domains: false, excluded_domains: false }
        },
        context_window: { max_input_tokens: 128000, max_output_tokens: 16384 },
        default_parameters: { temperature: 0.7 },
        provider_settings: { oagw_alias: "demo-openai", cost: { input_per_1k_micro: 150, output_per_1k_micro: 600 } }
    }')")
echo "${MODEL_JSON}" | jq .
MODEL_ID=$(echo "${MODEL_JSON}" | jq -r .id)
CANONICAL_ID=$(echo "${MODEL_JSON}" | jq -r .canonical_id)
CANONICAL_ID_ENC=$(jq -rn --arg v "${CANONICAL_ID}" '$v|@uri')

# ── 6. GET /models ──────────────────────────────────────────────────────────
step "GET /models — admin list with flags"

curl -fsS --get "${MR}/models" \
    --data-urlencode "\$filter=vendor eq 'OpenAI' and streaming eq true" \
    --data-urlencode "\$orderby=canonical_id desc" \
    --data-urlencode "limit=10" | jq .

# ── 7. GET /models/{id} ─────────────────────────────────────────────────────
step "GET /models/{id}"

curl -fsS "${MR}/models/${MODEL_ID}" | jq .

# ── 8. GET /available-models/{canonical_id} ─────────────────────────────────
step "GET /available-models/{canonical_id} — resolve (:: url-encoded)"

curl -fsS "${MR}/available-models/${CANONICAL_ID_ENC}" | jq .

# ── 9. GET /available-models ────────────────────────────────────────────────
step "GET /available-models — exactly the resolvable models"

curl -fsS --get "${MR}/available-models" \
    --data-urlencode "\$select=canonical_id,info.gts_type,info.base_url,info.provider_settings" | jq .

# ── 10. PATCH /models/{id} ──────────────────────────────────────────────────
step "PATCH /models/{id} — revoke approval"

curl -fsS -X PATCH "${MR}/models/${MODEL_ID}" \
    -H 'Content-Type: application/json' \
    -d '{ "approval_state": "revoked" }' | jq .

step "GET /available-models/{canonical_id} — after revoke (expect 403 MODEL_NOT_APPROVED)"

EVAL_HTTP=$(curl -sS -o /tmp/mr_eval_resp.json -w '%{http_code}' \
    "${MR}/available-models/${CANONICAL_ID_ENC}" || true)
echo "HTTP ${EVAL_HTTP}"
jq . /tmp/mr_eval_resp.json
[ "${EVAL_HTTP}" = "403" ] || { echo "expected 403" >&2; exit 1; }

# ── 11. DELETE /models/{id} ─────────────────────────────────────────────────
step "DELETE /models/{id} (expect 204)"

curl -fsS -X DELETE -o /dev/null -w 'HTTP %{http_code}\n' "${MR}/models/${MODEL_ID}"

# ── 12. DELETE /providers/{id} ──────────────────────────────────────────────
step "DELETE /providers/{id} (expect 204)"

curl -fsS -X DELETE -o /dev/null -w 'HTTP %{http_code}\n' "${MR}/providers/${PROVIDER_ID}"

printf '\n\033[1;32mAll twelve endpoints hit successfully.\033[0m\n'
