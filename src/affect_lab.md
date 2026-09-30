# affect_lab.rs

Supervises the optional local GGUF provider and its experimental vector compiler.
The manager embeds the standard-library Python host and public-API C++ extractor,
deploys them into the artifact directory, and starts the worker with a private token.

The worker serializes inference and extraction. It snapshots manual or request-local
profiles, restarts llama.cpp when profile or vector identity changes, and verifies
exact model/vector fingerprints. A process-group supervisor kills all native
descendants on owner death, including compiler subprocesses.

`start` requires the desktop backend parent-pipe marker and Linux parent-death
signaling. Worker startup loads metadata only; native weights load on demand.
`select_for_agent` applies a session override and remembers the prior provider.
`config_to_save` preserves the prior durable provider when unrelated settings are
saved. `restore_provider` and `stop` return to it and reap the local worker.

Authenticated routes are `GET /v1/affect-lab` and POST actions `start`, `stop`,
`use-for-agent`, `build`, `compare`, `profile`, and `cancel`. The compatible inference
API is private loopback HTTP. Both the simple client and agentic streaming client
reach it through their existing URL/model/key configuration.

See the frontend repository's `docs/AFFECT_LAB.md` for scientific limits, artifact
format, dependencies, experiments and verification. This module never starts a
service, persists a provider token, or changes model weights.
