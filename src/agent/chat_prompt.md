# Private-chat wire messages

The backend sends one canonical identity/behavior system message, a clearly
labeled JSON-quoted historical-context envelope, chronological role-bearing
conversation history, and the current operator text as the final user message.
The operator text is not expanded as a character template, wrapped in a bullet,
or followed by harness instructions. Pending message IDs are excluded from
history, and persisted assistant metadata is removed without changing the
speaker role. Exact previously saved scaffold echoes are not reinforced.

Character fields and placeholders are compiled by `AgentConfig::identity_context`
for every generation lane, not by an optional UI save action. The prompt inspector
stores the initial wire bundle: model, messages, tool schemas, temperature and
token budget. Subsequent tool/result exchanges remain in the inner tool loop;
their execution records and raw provider output are separate from this snapshot.
Character-card `{{char}}` / `{{user}}` placeholders are case-insensitive and
expanded only in card fields, never in current or historical operator messages.
Automatic activity transcripts are not reinjected alongside the real chat
history; they remain stored and available to background cognition. The current
request's durable intention is not repeated as an old unfinished goal. Other
open conversation intentions and durable notes remain available.

Historical memory, orientation, examples, prior-model text and plugin additions
remain non-authoritative. Tool policies and UI-parent ownership are unchanged.

Incompatible CUDA/KV errors explain the Settings recovery path instead of
promising that an identical chat retry will repair an engine build.
Ordinary Agentic answers yield without mandatory bookkeeping blocks; explicit
continuation blocks remain available for unfinished authorized work. Character,
feelings and creative conversation do not trigger memory/handoff chores.
Four consecutive empty `search_memory` results count as no progress even when
the query wording changes. This does not classify distinct successful writes
or changing retrieved evidence as repetition. It applies with iteration limits
disabled and leaves the forced final pass tool-free.
