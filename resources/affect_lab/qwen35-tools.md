# qwen35-tools.jinja

Bundled Qwen3.5-family chat template from llama.cpp's
`models/templates/Qwen3.5-4B.jinja`, under the adjacent MIT license. It renders
tool definitions, assistant function calls and tool-result messages, and honors
`enable_thinking`. The Qwen3.8-named GGUF used here declares `qwen35` architecture;
its embedded simplified template renders only system/user/assistant text and
silently drops the tool protocol.

Rust embeds/deploys this resource with the Python worker and license. The worker
uses it only for `qwen35`, retaining embedded templates for other architectures.
It passes `--chat-template-file` alongside `--jinja --reasoning off`. Template
identity is included in inference/evidence fingerprints; earlier recommendations
become historical after this format change. No model weights are modified.

Upstream: https://github.com/ggml-org/llama.cpp/blob/master/models/templates/Qwen3.5-4B.jinja
