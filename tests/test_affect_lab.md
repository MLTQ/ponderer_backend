# test_affect_lab.py

## Purpose
Regression tests for GGUF validation, explicit loading, steering isolation,
streaming, cancellation and native subprocess ownership without real weights.

## Components
- `AffectLabTests`: Uses temporary GGUF and fake llama-server fixtures.
- `inactive`: Checks /proc on Linux and ps on macOS, including zombie state.
- Parent-death test: Kills the owner and verifies supervisor/native process exit
  on both macOS and Linux.

## Contracts
Discovery tests and frontend lifecycle scripts reuse these fixtures. Temporary
paths are canonicalized so macOS /var and /private/var aliases match the model
identity recorded by the worker. Tests need loopback sockets and process inspection.

Run `python3 -m unittest discover -s ponderer_backend/tests -p 'test_affect*.py'`.
