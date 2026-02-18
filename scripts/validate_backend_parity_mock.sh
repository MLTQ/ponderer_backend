#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d -t ponderer-parity-XXXXXX)"
MOCK_PORT="${MOCK_OPENAI_PORT:-19090}"
BACKEND_BIND="${PONDERER_BACKEND_BIND:-127.0.0.1:8879}"
BACKEND_URL="http://${BACKEND_BIND}"
WS_URL="ws://${BACKEND_BIND}/v1/ws/events"
BACKEND_LOG="${WORK_DIR}/backend.log"
MOCK_LOG="${WORK_DIR}/mock_llm.log"
WS_EVENTS_LOG="${WORK_DIR}/ws_events.jsonl"

cleanup() {
  kill "${WS_PID:-}" >/dev/null 2>&1 || true
  kill "${BACKEND_PID:-}" >/dev/null 2>&1 || true
  kill "${MOCK_PID:-}" >/dev/null 2>&1 || true
  wait "${WS_PID:-}" >/dev/null 2>&1 || true
  wait "${BACKEND_PID:-}" >/dev/null 2>&1 || true
  wait "${MOCK_PID:-}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

cat >"${WORK_DIR}/ponderer_config.toml" <<CFG
graphchan_api_url = ""
llm_api_url = "http://127.0.0.1:${MOCK_PORT}"
llm_model = "mock-model"
username = "Ponderer"
poll_interval_secs = 1
max_tool_iterations = 6
disable_tool_iteration_limit = false
max_chat_autonomous_turns = 1
disable_chat_turn_limit = false
max_background_subtask_turns = 2
disable_background_subtask_turn_limit = false
enable_ambient_loop = false
enable_image_generation = false
enable_self_reflection = false
enable_screen_capture_in_loop = false
enable_camera_capture_tool = false
max_posts_per_hour = 10
database_path = "parity_validation.db"
CFG
# Ensure this config is always selected over executable-directory configs.
touch -t 209912312359 "${WORK_DIR}/ponderer_config.toml"

echo "Working directory: ${WORK_DIR}"
echo "Starting mock OpenAI server on :${MOCK_PORT}"
python3 "${REPO_ROOT}/scripts/mock_openai_server.py" --host 127.0.0.1 --port "${MOCK_PORT}" >"${MOCK_LOG}" 2>&1 &
MOCK_PID=$!
sleep 1

echo "Starting standalone backend on ${BACKEND_BIND}"
(
  cd "${WORK_DIR}"
  RUST_LOG=info,ponderer_backend=debug \
  PONDERER_BACKEND_BIND="${BACKEND_BIND}" \
  PONDERER_BACKEND_AUTH_MODE=disabled \
  cargo run -q --manifest-path "${REPO_ROOT}/ponderer_backend/Cargo.toml" --bin ponderer_backend
) >"${BACKEND_LOG}" 2>&1 &
BACKEND_PID=$!
sleep 3

echo "Capturing websocket events"
node -e '
const fs = require("fs");
const url = process.argv[1];
const out = process.argv[2];
const durationMs = Number(process.argv[3]);
const events = [];
const ws = new WebSocket(url);
let done = false;
const finish = (code) => {
  if (done) return;
  done = true;
  try { fs.writeFileSync(out, events.join("\n")); } catch {}
  try { ws.close(); } catch {}
  process.exit(code);
};
ws.onmessage = (evt) => {
  events.push(String(evt.data));
};
ws.onerror = () => finish(2);
ws.onclose = () => finish(0);
setTimeout(() => finish(0), durationMs);
' "${WS_URL}" "${WS_EVENTS_LOG}" "14000" >"${WORK_DIR}/ws.log" 2>&1 &
WS_PID=$!
sleep 1

echo "Creating conversation + sending operator message"
CREATE_JSON="$(curl -sS -X POST -H 'Content-Type: application/json' \
  -d '{"title":"Parity Mock Conversation"}' "${BACKEND_URL}/v1/conversations")"
CONVERSATION_ID="$(printf '%s' "${CREATE_JSON}" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
if [[ -z "${CONVERSATION_ID}" ]]; then
  echo "Failed parsing conversation id from: ${CREATE_JSON}" >&2
  exit 1
fi

curl -sS -X POST -H 'Content-Type: application/json' \
  -d '{"content":"Please keep working in the background and finish."}' \
  "${BACKEND_URL}/v1/conversations/${CONVERSATION_ID}/messages" >/dev/null

echo "Polling turn history for background-subtask handoff"
FOUND=0
for _ in $(seq 1 50); do
  TURNS_JSON="$(curl -sS "${BACKEND_URL}/v1/conversations/${CONVERSATION_ID}/turns?limit=20")"
  if python3 - "$TURNS_JSON" <<'PY'
import json
import sys
turns = json.loads(sys.argv[1])
has_fg_continue = any(t.get("iteration") == 1 and t.get("decision") == "continue" for t in turns)
has_bg_turn = any((t.get("iteration") or 0) >= 100 for t in turns)
has_bg_done = any((t.get("iteration") or 0) >= 100 and t.get("decision") == "yield" and t.get("status") == "done" for t in turns)
if has_fg_continue and has_bg_turn and has_bg_done:
    sys.exit(0)
sys.exit(1)
PY
  then
    FOUND=1
    break
  fi
  sleep 0.4
done

if [[ "${FOUND}" != "1" ]]; then
  echo "Background subtask parity checks did not converge in time" >&2
  echo "Turns payload: ${TURNS_JSON}" >&2
  exit 1
fi

MESSAGES_JSON="$(curl -sS "${BACKEND_URL}/v1/conversations/${CONVERSATION_ID}/messages?limit=20")"
if ! python3 - "$MESSAGES_JSON" <<'PY'
import json
import sys
messages = json.loads(sys.argv[1])
if not any(m.get("role") == "agent" for m in messages):
    sys.exit(1)
if not any("Background task complete" in (m.get("content") or "") or "background" in (m.get("content") or "").lower() for m in messages):
    sys.exit(1)
sys.exit(0)
PY
then
  echo "Expected agent/background completion messages not found" >&2
  echo "Messages payload: ${MESSAGES_JSON}" >&2
  exit 1
fi

wait "${WS_PID}" || true

if [[ ! -s "${WS_EVENTS_LOG}" ]]; then
  echo "No websocket events captured" >&2
  exit 1
fi

if ! python3 - "${WS_EVENTS_LOG}" <<'PY'
import json
import sys
path = sys.argv[1]
event_types = set()
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line=line.strip()
        if not line:
            continue
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue
        event_type = payload.get("event_type")
        if event_type:
            event_types.add(event_type)
required = {"chat_streaming", "tool_call_progress", "action_taken"}
missing = sorted(required - event_types)
if missing:
    print("Missing event types:", ", ".join(missing))
    sys.exit(1)
sys.exit(0)
PY
then
  echo "Websocket event stream missing expected event types" >&2
  exit 1
fi

if ! grep -q "Loaded config from" "${BACKEND_LOG}"; then
  echo "Backend did not report loading temp config file" >&2
  exit 1
fi

if [[ ! -s "${MOCK_LOG}" ]]; then
  echo "Mock LLM log is empty; backend likely did not call mock model" >&2
  exit 1
fi

echo "[ok] standalone background handoff and websocket parity"
echo "Work dir: ${WORK_DIR}"
echo "Backend log tail:"
tail -n 25 "${BACKEND_LOG}" || true
echo "Mock LLM log tail:"
tail -n 10 "${MOCK_LOG}" || true
