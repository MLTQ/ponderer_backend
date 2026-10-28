# worker.py

## Purpose
UI-owned GGUF experiments, model loading and authenticated streaming HTTP proxy.
The process supervisor lives separately in process_lifetime.py and supports both
macOS and Linux; MLX loading is deferred.

## Components
- `AffectLab`: GGUF metadata, vector build, evidence, profiles and llama.cpp startup.
- `LabHandler`: Private control routes and compatible chat/SSE proxy.
- `main`: Serve mode requires desktop ownership on macOS/Linux and the expected
  backend PID. Its parent watcher starts before runtime initialization.

## Contracts
| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Rust manager | One JSON startup line, --parent-pid, authenticated control routes | Wire protocol |
| Tests/discovery | Existing helpers and native_child_command re-export | Names/signatures |

Parent death ends the worker; native supervisors end/reap their process groups.
GGUF inference retains explicit single-device placement and validated cache settings.
Metadata inspection and non-server experiments still work without a desktop owner.
