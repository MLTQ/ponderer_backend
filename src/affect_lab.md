# affect_lab.rs

Supervises the optional local GGUF provider and its experimental vector compiler.
The manager embeds the standard-library Python host, discovery module and public-API C++ extractor,
deploys them into the artifact directory, and starts the worker with a private token.

The worker serializes inference and extraction. It snapshots manual or request-local
profiles, restarts llama.cpp when profile or vector identity changes, and verifies
exact model/vector fingerprints. A process-group supervisor kills all native
descendants on owner death, including compiler subprocesses. Children are spawned
by a worker-lifetime thread: Linux's parent-death signal tracks the spawning thread,
so an HTTP/load job ending must not terminate an otherwise UI-owned engine.

`start` requires the desktop backend parent-pipe marker and Linux parent-death
signaling. Worker startup loads metadata only; the cancellable `load` job allocates
weights explicitly, or inference loads them on demand. Cancelling a queued job
remains effective even if a later completion clears the global cancellation flag.
Startup options include context (1,024..1,048,576 tokens), unified KV, separate K/V
types and flash attention. A 200k/Q4_1 preset leaves executable/device placement
unchanged. Quantized V with flash attention off is rejected before launch.
GPU placement defaults to `gpu_layers=-1` (native `--gpu-layers all`), requires a
single validated `gpu_device`, and explicitly passes `--device`, `--split-mode
none`, `--main-gpu 0` and `--fit off`. Zero selects CPU (`--device none`); positive
counts permit explicit partial offload. No automatic device split or context/CPU
fallback occurs. Device placement is retained across steering reloads and recorded
in evidence signatures; startup OOM errors identify the requested device/settings.
Extraction remains an explicitly CPU-only separate runtime.

For `qwen35` architecture the worker deploys a bundled, MIT-licensed upstream
Qwen3.5 tool-capable Jinja template. This replaces tool-blind GGUF conversion
templates without modifying weights; other families retain embedded templates.
The template SHA-256 is recorded in inference fingerprints. All active control
vectors are passed in one comma-separated `--control-vector-scaled` argument:
recent engines silently retain only the last value when this flag is repeated.
Old-format evidence becomes historical and must be retested before adoption.

`POST /v1/affect-lab/devices` accepts `server_binary`, runs only `--list-devices`,
and returns that engine's IDs/names and optional total/free MiB. It requires the same
UI-parent/Linux safeguard, loads no weights, is limited to ten seconds and 64 KiB
per output stream, and kills the probe on timeout/drop/owner death. The UI does not
query GPUs periodically. Engine inventory IDs are not `nvidia-smi` indices.
The private proxy accepts bounded 16 MiB request bodies; inference clients use a
bounded one-hour deadline for the loopback local alias, leaving other clients at
their existing deadlines. The same memory settings are retained across profile
reloads and included in comparison reports.
`select_for_agent` applies a session override and remembers the prior provider.
`config_to_save` preserves the prior durable provider when unrelated settings are
saved. `restore_provider` and `stop` return to it and reap the local worker.

Authenticated routes are `GET /v1/affect-lab` and POST actions `start`, `stop`,
`use-for-agent`, `devices`, `build`, `compare`, `profile`, `load`, `review`, `discover`, `study`, and `cancel`. Status
includes editable starter/verified built recipes and held-out test prompts. Compare
accepts either the legacy single-concept experiment or a bounded full profile and
one to six prompts, testing neutral/half/full mixes. Reports retain all vector/
recipe/model identities, generation settings, raw outputs and strict smoke-check
results. Review writes only the current report by matching its ID, never a submitted
path, with bounded notes and independent operator affect/quality judgments.
Profiles support signed coefficients with an absolute budget of one and explicit
whole-profile amplification in 1..4 (default one). Discovery uses actual matched
responses to fixed training tasks, scans signs/ranges/amplification, then tests
untouched confirmation tasks with repeated seeds and matched shuffled-vector
controls. Rubric scores are schema-constrained, anonymous, neutral same-model
judgments, not independent validation. Task-clustered bootstrap intervals and
quality/integrity gates fail closed. Ordinary and mildly elicited probes are
reported separately. Studies test up to four controls and signed combinations.
Reports checkpoint partial evidence, fingerprint recipes/vectors/model/pipeline,
restore matching history, and mark changed artifacts/settings as historical.
Retest accepts only the latest matching unchanged concept, not a caller-supplied
report path. Neither discovery nor study mutates the requested agent mix.
The compatible inference
API is private loopback HTTP. Both the simple client and agentic streaming client
reach it through their existing URL/model/key configuration.

See the frontend repository's `docs/AFFECT_LAB.md` for scientific limits, artifact
format, dependencies, experiments and verification. This module never starts a
service, persists a provider token, or changes model weights.
