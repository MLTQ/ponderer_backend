# Durable Intention Integration Test

`intentions.rs` exercises the public crate boundary against a real temporary SQLite file. It verifies source-reference deduplication, exact-id claiming, atomic claim metadata, persistence across database reopen, expired-lease recovery, attempt counting, outcome timestamping, owner-mediated completion, and single-query open-work filtering.

This integration target intentionally avoids compiling internal unit-test modules, so it remains a focused verification path when an unrelated unit-test-only module in the working tree is temporarily broken.
