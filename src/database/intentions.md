# Durable Intention Persistence

`intentions.rs` persists the domain records from `crate::intentions` in SQLite. It owns CRUD, querying, idempotent creation, claim leasing, outcome transitions, and crash recovery; it does not decide which intention an agent should create or what work should be performed.

## Idempotent producers

`create_intention_if_absent` uses `(origin, source_reference)` as an atomic idempotency key. Orientation thoughts, external events, and other replayable producers should supply a stable source reference and use this API. Drafts without a source reference always create new work.

## Claim protocol

`claim_next_intention` runs in an immediate transaction, recovers expired leases, and claims one eligible record ordered by priority and age. `claim_intention` applies the same eligibility, lease, and attempt bookkeeping to an exact id, allowing a producer to own the intention it just created without racing another queue item. Both APIs refuse live claims and ineligible work atomically.

A successful claim increments `attempt_count` and records `last_attempt_at` without changing `last_outcome_at`. Only the owning worker can call `transition_claimed_intention`; transitions persist the outcome timestamp and either complete, abandon, block, or reschedule the work.

A blocked intention without `next_eligible_at` is deliberately dormant. Pending and time-bounded blocked intentions become actionable only after both `due_at` and `next_eligible_at` permit it.

## Restart recovery

Claims are leases rather than permanent locks. `recover_expired_intention_claims` releases only elapsed leases, preserving fresh claims that may still have live workers. Recovery preserves the attempt count and records that the interrupted attempt produced no explicit outcome.

## Query and mutation contract

`list_intentions` can filter by lifecycle, origin, open/nonterminal state, and actionable time. `open_only` remains one SQL query and composes with the other selectors; `list_open_intentions` is the common convenience form. `update_intention` modifies descriptive, provenance, and scheduling fields only; claim and lifecycle state can change only through the claim/transition APIs.
