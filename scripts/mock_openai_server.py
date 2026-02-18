#!/usr/bin/env python3
"""Minimal OpenAI-compatible mock server for backend parity tests."""

from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Dict, List


STATE = {
    "continuation_calls": 0,
}


def extract_text_content(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: List[str] = []
        for item in content:
            if isinstance(item, dict):
                text = item.get("text")
                if isinstance(text, str):
                    parts.append(text)
        return "\n".join(parts)
    return ""


class Handler(BaseHTTPRequestHandler):
    server_version = "MockOpenAIServer/0.1"

    def log_message(self, format: str, *args: Any) -> None:
        return

    def _send_json(self, status: int, payload: Dict[str, Any]) -> None:
        raw = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_POST(self) -> None:  # noqa: N802
        if self.path != "/v1/chat/completions":
            self._send_json(404, {"error": f"unsupported path: {self.path}"})
            return

        try:
            length = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(length)
            request = json.loads(body.decode("utf-8") or "{}")
        except Exception as exc:  # pragma: no cover - defensive
            self._send_json(400, {"error": f"invalid json body: {exc}"})
            return

        if request.get("stream") is True:
            # Force backend fallback path from streaming -> non-streaming.
            self._send_json(400, {"error": "stream not supported by mock"})
            return

        messages = request.get("messages") or []
        last_msg = messages[-1] if messages else {}
        last_role = last_msg.get("role", "")
        last_text = extract_text_content(last_msg.get("content"))
        print(
            f"mock_request stream={bool(request.get('stream'))} role={last_role} "
            f"text_preview={last_text[:80]!r}",
            flush=True,
        )

        # Step 1 (foreground): return a tool call so backend exercises tool loop.
        if "## New Operator Message(s)" in last_text and "## Autonomous Continuation Context" not in last_text:
            self._send_json(
                200,
                {
                    "id": "chatcmpl-mock-tool",
                    "object": "chat.completion",
                    "choices": [
                        {
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": "",
                                "tool_calls": [
                                    {
                                        "id": "call_list_dir_1",
                                        "type": "function",
                                        "function": {
                                            "name": "list_directory",
                                            "arguments": '{"path":"."}',
                                        },
                                    }
                                ],
                            },
                            "finish_reason": "tool_calls",
                        }
                    ],
                },
            )
            return

        # Step 2 (foreground after tool): ask to continue to trigger background handoff.
        if last_role == "tool":
            content = (
                "I have enough context and will continue this in the background.\n"
                "[turn_control]\n"
                '{"decision":"continue","status":"still_working","needs_user_input":false,'
                '"user_message":"I am continuing in the background.","reason":"Need one more autonomous turn."}'
                "\n[/turn_control]"
            )
            self._send_json(
                200,
                {
                    "id": "chatcmpl-mock-continue",
                    "object": "chat.completion",
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": content, "tool_calls": []},
                            "finish_reason": "stop",
                        }
                    ],
                },
            )
            return

        # Background turns: first continue, second yield done.
        if "## Autonomous Continuation Context" in last_text:
            STATE["continuation_calls"] += 1
            if STATE["continuation_calls"] == 1:
                content = (
                    "Continuing autonomous background work.\n"
                    "[turn_control]\n"
                    '{"decision":"continue","status":"still_working","needs_user_input":false,'
                    '"user_message":"Still working in the background.","reason":"One final pass needed."}'
                    "\n[/turn_control]"
                )
            else:
                content = (
                    "Background work complete.\n"
                    "[turn_control]\n"
                    '{"decision":"yield","status":"done","needs_user_input":false,'
                    '"user_message":"Background task complete.","reason":"Task complete."}'
                    "\n[/turn_control]"
                )
            self._send_json(
                200,
                {
                    "id": "chatcmpl-mock-background",
                    "object": "chat.completion",
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": content, "tool_calls": []},
                            "finish_reason": "stop",
                        }
                    ],
                },
            )
            return

        # Fallback response for orientation/persona or unrelated calls.
        self._send_json(
            200,
            {
                "id": "chatcmpl-mock-fallback",
                "object": "chat.completion",
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": "{}", "tool_calls": []},
                        "finish_reason": "stop",
                    }
                ],
            },
        )


def main() -> None:
    parser = argparse.ArgumentParser(description="Run a local mock OpenAI-compatible server")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=19090)
    args = parser.parse_args()

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    try:
        server.serve_forever()
    finally:  # pragma: no cover
        server.server_close()


if __name__ == "__main__":
    main()
