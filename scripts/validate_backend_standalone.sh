#!/usr/bin/env bash
set -euo pipefail

BIND_ADDR="${PONDERER_BACKEND_BIND:-127.0.0.1:8878}"
TOKEN="${PONDERER_BACKEND_TOKEN:-standalone-smoke-token}"
BACKEND_URL="http://${BIND_ADDR}"
LOG_FILE="${PONDERER_BACKEND_SMOKE_LOG:-/tmp/ponderer_backend_smoke.log}"

echo "Running standalone backend smoke test"
echo "  bind:   ${BIND_ADDR}"
echo "  token:  [set]"
echo "  log:    ${LOG_FILE}"

: >"${LOG_FILE}"
PONDERER_BACKEND_BIND="${BIND_ADDR}" \
PONDERER_BACKEND_TOKEN="${TOKEN}" \
  cargo run -q --manifest-path ponderer_backend/Cargo.toml --bin ponderer_backend >"${LOG_FILE}" 2>&1 &
PID=$!

cleanup() {
  kill "${PID}" >/dev/null 2>&1 || true
  wait "${PID}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

sleep 3

unauth_code=$(curl -s -o /tmp/ponderer_smoke_unauth.out -w "%{http_code}" "${BACKEND_URL}/v1/health")
if [[ "${unauth_code}" != "401" ]]; then
  echo "Expected unauthorized /v1/health to return 401, got ${unauth_code}" >&2
  exit 1
fi

echo "[ok] unauthorized auth boundary"

health_json=$(curl -sS -H "Authorization: Bearer ${TOKEN}" "${BACKEND_URL}/v1/health")
if [[ "${health_json}" != *'"status":"ok"'* ]]; then
  echo "Expected health status ok, got: ${health_json}" >&2
  exit 1
fi
echo "[ok] authorized health"

conversations_json=$(curl -sS -H "Authorization: Bearer ${TOKEN}" "${BACKEND_URL}/v1/conversations?limit=5")
if [[ "${conversations_json}" != *'"id"'* ]]; then
  echo "Expected conversations response to contain at least one id, got: ${conversations_json}" >&2
  exit 1
fi
echo "[ok] list conversations"

create_json=$(curl -sS -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
  -d '{"title":"Standalone Smoke"}' "${BACKEND_URL}/v1/conversations")
conversation_id=$(printf '%s' "${create_json}" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
if [[ -z "${conversation_id}" ]]; then
  echo "Failed to parse conversation id from: ${create_json}" >&2
  exit 1
fi
echo "[ok] create conversation ${conversation_id}"

send_json=$(curl -sS -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
  -d '{"content":"hello from standalone smoke"}' "${BACKEND_URL}/v1/conversations/${conversation_id}/messages")
if [[ "${send_json}" != *'"status":"queued"'* ]]; then
  echo "Expected queued status from send message, got: ${send_json}" >&2
  exit 1
fi
echo "[ok] enqueue operator message"

messages_json=$(curl -sS -H "Authorization: Bearer ${TOKEN}" \
  "${BACKEND_URL}/v1/conversations/${conversation_id}/messages?limit=10")
if [[ "${messages_json}" != *'hello from standalone smoke'* ]]; then
  echo "Expected sent message in history, got: ${messages_json}" >&2
  exit 1
fi
echo "[ok] message persisted"

status_json=$(curl -sS -H "Authorization: Bearer ${TOKEN}" "${BACKEND_URL}/v1/agent/status")
if [[ "${status_json}" != *'"visual_state"'* ]]; then
  echo "Expected visual_state in agent status, got: ${status_json}" >&2
  exit 1
fi
echo "[ok] agent status"

plugins_json=$(curl -sS -H "Authorization: Bearer ${TOKEN}" "${BACKEND_URL}/v1/plugins")
if [[ "${plugins_json}" != *'builtin.core'* ]]; then
  echo "Expected builtin.core in plugins response, got: ${plugins_json}" >&2
  exit 1
fi
echo "[ok] plugin manifests"

echo "Standalone backend smoke checks passed"
echo "Recent backend log tail:"
tail -n 20 "${LOG_FILE}" || true
