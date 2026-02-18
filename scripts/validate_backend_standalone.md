# validate_backend_standalone.sh

## Purpose
Runs a backend-only smoke test for standalone parity. Starts `ponderer_backend` without the frontend and validates authenticated REST behavior and basic chat persistence.

## Components

### Process bootstrap
- **Does**: Launches backend with bind/token env vars and captures logs to a temp file.

### Auth checks
- **Does**: Verifies `/v1/health` is `401` without bearer token and `ok` with a valid token.

### Chat checks
- **Does**: Lists conversations, creates a new conversation, enqueues an operator message, and confirms message persistence.

### Runtime checks
- **Does**: Confirms `/v1/agent/status` and `/v1/plugins` return expected fields.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| CI/manual smoke runs | Script exits non-zero on parity regressions | Silent failures or removed assertions |
| Backend API | Routes `/v1/health`, `/v1/conversations`, `/v1/conversations/:id/messages`, `/v1/agent/status`, `/v1/plugins` | Endpoint/schema changes require script updates |

## Notes
- Defaults: bind `127.0.0.1:8878`, token `standalone-smoke-token`, log `/tmp/ponderer_backend_smoke.log`.
- Override using `PONDERER_BACKEND_BIND`, `PONDERER_BACKEND_TOKEN`, and `PONDERER_BACKEND_SMOKE_LOG`.
