# Durable Agent Intentions

`intentions.rs` defines the domain boundary for work an agent wants to remember across turns and process restarts. It intentionally contains no runtime-loop behavior: orientation and self-triggering code can create records later without coupling persistence to `agent/mod.rs`.

## Lifecycle

An intention starts `pending`, is atomically `claimed` by one worker, and is released with a recorded outcome. A retry returns it to `pending`; a blocked result remains dormant until it has a `next_eligible_at`; completed and abandoned intentions are terminal. Claim leases let persistence recover work after a crash without stealing a live worker's fresh claim.

`attempt_count` and `last_attempt_at` describe executions, while `last_outcome`, `last_outcome_at`, and `completed_at` describe their durable result. This distinction is important for cadence logic: callers should schedule from recorded outcomes rather than merely from attempted wakeups.

## Identity and provenance

`IntentionOrigin` records why work entered the queue. `SelfAuthored` identifies a goal explicitly proposed and adopted during armed Loose mode, rather than merely inferred from prior reflection. `source_reference` gives the producer a stable idempotency key within an origin, and `related_concern_ids` links the work to the agent's existing concern model. Descriptive text is trimmed, related ids are normalized, and priority is constrained to `0.0..=1.0`.

## Update contract

`AgentIntentionPatch` changes descriptive and scheduling fields only; lifecycle transitions go through the database claim/transition APIs. Nested options on nullable fields distinguish “leave unchanged” from “clear this value.”

`IntentionListFilter::open_only` excludes completed and abandoned records in one query snapshot and composes with origin, exact status, actionable time, and limit filters.
