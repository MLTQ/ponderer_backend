# process_lifetime.py

## Purpose
Own subprocess lifetimes on macOS and Linux without Python pre-exec callbacks.
Workers watch their expected backend PID; native supervisors watch their worker
and kill/reap their entire native process group when the owner exits.

## Components
- `watch_parent`: Checks the expected parent before starting and every 100 ms.
- `native_child_command`: Builds a shell-free supervisor invocation.
- `exec_child`: Blocks termination across spawn, supervises a new process group,
  and reaps it on cancellation or parent death. Linux additionally uses prctl.

## Contracts
| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Rust manager | `parent -- executable args...` CLI preserves child stdout/status | CLI |
| Local workers | Supervisor death handling works even after worker SIGKILL | Lifetime semantics |

The supervisor should receive SIGTERM on explicit cancellation so it can reap its
child. Parent polling also handles abrupt owner exit on macOS (no prctl).
