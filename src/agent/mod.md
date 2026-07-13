# mod.rs

## Purpose
Coordinates the core autonomous agent loop with explicit three-loop architecture (Ambient/Engaged/Dream): polling protocol-v1 plugins, reasoning over normalized events, managing visual state, running periodic ambient maintenance, handling private operator chat, and persisting long-lived behavior through the shared database. It binds the supervised plugin host, tools, reasoning, memory, and UI events.

## Components

### `Agent`
- **Does**: Owns runtime dependencies (tools, config, database, reasoning engines, runtime plugin host) plus per-conversation background-subtask handles, wake-signal primitives for interruptible sleeps, and lifecycle operations (`new`, `run_loop`, `reload_config`, `toggle_pause`, `set_paused`, `runtime_status`, `notify_operator_message_queued`)
- **Interacts with**: `config::AgentConfig`, `database::AgentDatabase`, `skills::SkillEvent`, `tools::ToolRegistry`, `runtime_plugin_host.rs`, `agent::reasoning`, `agent::trajectory`, `agent::orientation`, `agent::journal`, `agent::concerns`

### Living Loop foundation modules (`journal`, `concerns`, `dream`, `self_context`)
- **Does**: Provide typed records for journal entries and concern tracking, a tool-free structured Dream engine, and a bounded temporal self-context renderer
- **Interacts with**: database CRUD/persistence methods plus ambient, Dream, self-directed, and engaged prompt assembly
- **Rationale**: Durable experience should causally inform later model calls while prior generated self-description remains revisable rather than canonical

### `AgentState`
- **Does**: Tracks in-memory runtime state (visual mode, pause flag, rolling outbound-action timestamps, processed event IDs)
- **Interacts with**: `run_loop`, `run_cycle`, and UI-facing event emission

### `AgentEvent` / `AgentVisualState`
- **Does**: Defines UI/event bus payloads describing current state, observations, reasoning traces, actions, orientation updates, journal writes, concern lifecycle updates, live token metrics for streamed replies, errors, `ApprovalRequest { tool_name, reason }` for interactive popups, and `CycleStart { label }` emitted at the top of each major cycle (Engaged, Ambient, Dream, Cycle, Self-directive, Heartbeat) for UI turn grouping.
- **Interacts with**: `ui::app` via shared flume channel; `server.rs` maps all variants to typed WS event types.

### `Agent::grant_session_tool_approval`
- **Does**: Delegates to `ToolRegistry::grant_session_approval` so the named tool bypasses `NeedsApproval` checks for the rest of the process lifetime, then wakes cognition so approval-blocked durable plugin events retry promptly.
- **Interacts with**: `tools/mod.rs` `ToolRegistry::grant_session_approval`; called from `server.rs` `POST /v1/agent/tools/:name/approve`.

### `maybe_notify_needs_approval`
- **Does**: After each agentic pass, scans returned `ToolCallRecord`s for `NeedsApproval` outputs and emits one `AgentEvent::ApprovalRequest` per unique tool name (deduplicated within the pass). Does not post chat messages.
- **Interacts with**: `tools::ToolOutput::NeedsApproval`, `Agent::emit`, `AgentEvent::ApprovalRequest`.

### `run_loop`
- **Does**: Main cognitive loop; restores recent orientation, processed event receipts, and expired intention claims, then executes either legacy single-loop mode or the three-loop mode (`run_engaged_tick`, `run_ambient_tick`, `run_dream_cycle`). Sleep windows are interruptible so queued operator messages can wake the loop immediately.
- **Interacts with**: `maybe_evolve_persona`, `run_engaged_tick`, `run_ambient_tick`, `should_dream`, `run_dream_cycle`, `run_cycle`

### `run_engaged_tick`
- **Does**: Runs operator chat processing and plugin-event handling as the engaged loop, returning normalized filtered events for ambient context. Durable poll receipts advance only after a normally completed pass with an explicit decision or successful action; cancellation, iteration exhaustion, empty output, and approval waits remain pending.
- **Interacts with**: `process_chat_messages`, runtime-plugin polling, `AgenticLoop` plugin-event pass

### `run_ambient_tick`
- **Does**: Runs orientation + disposition execution + optional concern decay + autonomous self-directive scheduling + merged heartbeat scheduling in the ambient loop
- **Interacts with**: `maybe_update_orientation`, `execute_disposition`, `maybe_run_self_directive`, `maybe_run_heartbeat`, `ConcernsManager`

### `maybe_run_self_directive`
- **Does**: Periodically claims at most one durable intention, executes one bounded self-directed micro-task when no operator messages or background subtasks are active, records retry/block/completion outcomes, emits telemetry, and persists autonomy summaries. Operator/private intentions are excluded from global temporal context and route any autonomous progress back only to their source conversation. Self-directed progress cannot terminally settle an operator request: it returns the exact intention to immediate engaged-loop eligibility for message reconciliation. Intentions synthesized by prior model output (`orientation_thought` and `dream`) use a memory-only profile rather than inheriting outward autonomous authority.
- **Interacts with**: `AgenticLoop`, `AgentDatabase` intention/concern/memory/activity-log APIs, `ToolRegistry` via its independent autonomous self-directed capability profile
- **Rationale**: A 5-60 minute cadence derived from the ambient tick provides genuine self-triggering without spending an LLM call every few ambient observations.

### `maybe_run_heartbeat`
- **Does**: Schedules autonomous heartbeat cycles, reads pending checklist/reminder signals, and invokes the tool-calling loop only when work exists
- **Interacts with**: `tools::agentic::AgenticLoop`, `ToolRegistry`, `AgentDatabase::agent_state` and working memory

### `maybe_run_memory_evolution`
- **Does**: Runs periodic replay evaluation for memory backends, stores eval artifacts, and records promotion-policy outcomes
- **Interacts with**: `memory::eval`, `memory::archive`, `AgentDatabase` memory eval/promotion APIs

### `run_cycle`
- **Does**: Polls the supervised runtime-plugin host, filters normalized events, then runs a dedicated agentic pass so package tools and regular tools share the same loop architecture as private chat. It applies the same fail-closed durable-receipt acceptance predicate as the Engaged loop.
- **Interacts with**: runtime-plugin event polling, `tools::agentic::AgenticLoop`, the effect-aware `ToolRegistry`, and `AgentDatabase` memory/chat helpers

### `maybe_update_orientation`
- **Does**: Samples presence + context, optionally captures/evaluates a desktop screenshot (when screen-capture opt-in is enabled), injects recent action digest + previous OODA packet context, computes a coarse signature, skips redundant orientation calls when unchanged, and otherwise runs orientation synthesis and persists snapshot records. Orientation/vision LLM calls are time-bounded so ambient work cannot stall engaged chat responsiveness indefinitely.
- **Interacts with**: `presence/mod.rs` (`PresenceMonitor`), `tools/vision.rs` (`capture_screen_to_path`), `llm_client.rs` (`evaluate_image`), `agent/orientation.rs` (`OrientationEngine`, `OrientationContext`), `database.rs` (`save_orientation_snapshot`), `AgentEvent::OrientationUpdate`
- **Rationale**: Adds situational awareness without changing existing action behavior in phase 2

### `maybe_write_journal_entry`
- **Does**: Applies journal gating (disposition + unchanged-disposition + minimum interval), requests a private entry from `JournalEngine`, persists it, and emits `JournalWritten`
- **Interacts with**: `agent/journal.rs` (`JournalEngine`, `journal_skip_reason`), `database.rs` (`add_journal_entry`, `set_state`)
- **Rationale**: Keeps journaling autonomous but bounded so ambient cycles do not spam repetitive entries

### `maybe_decay_concerns` / `apply_chat_concern_updates`
- **Does**: Applies salience decay each cycle (`7d -> monitoring`, `30d -> background`, `90d -> dormant`) and updates concerns from private-chat interactions via mention touch + structured concern signals
- **Interacts with**: `agent/concerns.rs` (`ConcernsManager`, `ConcernSignal`) and `database.rs` concern persistence
- **Rationale**: Keeps long-lived concern memory fresh without spamming low-value updates

### `process_chat_messages`
- **Does**: Handles unread operator chat messages by conversation thread, prioritizes operator conversations ahead of scheduled-only queues, and acquires an exact source-idempotent durable claim for the full unread-message batch before execution. If persistence or claim ownership is unavailable, execution fails closed and leaves messages unread for retry; an already-terminal claim reconciles the corresponding messages without duplicate execution. It streams live token output during each LLM call, emits per-tool progress updates plus live token-novelty samples, ingests structured concern signals (`[concerns]...[/concerns]`), and can run multiple autonomous turns per thread before final handoff using a structured `[turn_control]...[/turn_control]` protocol. In `direct` mode it runs a single-turn pass (still tool-capable), suppresses continuation/offload, disables runtime-plugin prompt addenda for latency, and uses existing compacted summaries without triggering a refresh LLM call. Scheduled-job conversations skip plugin prompt addenda and still use hard caps for chat turns + tool iterations so unattended work cannot monopolize the engaged loop. Foreground turn caps are optional safety rails; if enabled and exhausted while work can still continue, it offloads to a detached background subtask. It also runs deterministic loop-heat detection on per-turn signatures (action + response + tool set), forces a loop-break yield when repetitive similarity heat reaches configured threshold, persists per-turn user+system prompt payloads for UI inspection, stores a structured OODA packet per completed autonomous turn, retries one transient agentic error, and writes an operator-visible fallback failure message on terminal turn failure.
- **Interacts with**: `database::chat_messages`, `database::chat_conversations`, `database::chat_turns`, `database::chat_turn_tool_calls`, `tools::agentic::AgenticLoop::run_with_history_streaming_and_tool_events`, `ToolRegistry`
- **Rationale**: Uses continuation hints (not synthetic operator messages) for multi-turn autonomy, supports a configurable low-latency direct mode, applies host-owned semantic effect policy to installed tools, compacts long sessions through persisted summary snapshots, and only persists yielded assistant replies while allowing long tasks to continue asynchronously.

### `spawn_background_subtask` / `run_background_chat_subtask` / `reap_finished_background_subtasks`
- **Does**: Starts one detached worker per conversation, keeps subtask uniqueness per thread until the worker is explicitly reaped, executes additional autonomous turns with a dedicated unattended capability profile and the same prompt format, and reports completion/failure back through `AgentEvent`s. Reaping is the sole owner-removal path and preserves `done`, `blocked`, `needs_input`, `loop_break`, `paused`, and `failed` outcomes when settling durable intentions.
- **Interacts with**: `tools::agentic::AgenticLoop`, `database::AgentDatabase` turn lifecycle APIs, `ui::app` live progress drawer via `ToolCallProgress`, `ChatStreaming`, and `TokenMetrics`
- **Rationale**: Prevents long agentic runs from blocking the engaged loop while preserving visibility, per-conversation exclusion, join ownership, and truthful durable-intention outcomes.

### `request_stop`
- **Does**: Advances the shared cancellation generation and wakes the agent loop. Background `spawn_blocking` workers retain their handles and conversation exclusions until cooperative cancellation produces a result that the normal reaper joins.
- **Interacts with**: `AgenticConfig.cancel_generation`, background request generation snapshots, and `reap_finished_background_subtasks`.
- **Rationale**: Tokio cannot forcibly abort a running blocking task; clearing its handle would only detach live work and lose its durable intention outcome.

### `capability_profiles`
- **Does**: Resolves explicit loop capability policies, including distinct `scheduled`, `background`, and `self_directed` unattended profiles, into per-loop `ToolContext` objects with autonomous mode and allow/deny tool lists. Autonomous contexts share one process-wide rolling outbound-action limiter that reserves quota immediately before posting-tool invocation.
- **Interacts with**: `config::AgentConfig.capability_profiles`, `tools::ToolContext`

### Persona evolution helpers
- **Does**: When explicit self-reflection is enabled, capture persona snapshots and run trajectory inference on schedule, then emit a bounded `persona_evolved` lifecycle event to the shared runtime plugin host after persistence succeeds. Initial persona capture is also gated by this opt-in.
- **Interacts with**: `agent::trajectory`, `database::persona_history`, `runtime_plugin_host.rs`, reflection timestamps in `agent_state`

### `collect_prompt_slot_contributions` / `collect_engaged_prompt_contributions`
- **Does**: Queries the shared runtime plugin host for prompt-slot addenda, constructs the engaged-loop query context (conversation ID, loop label, summary, enabled tools), and degrades to an empty contribution set on plugin-host errors or timeout to protect chat latency.
- **Interacts with**: `runtime_plugin_host.rs`, `tools::ToolRegistry`.

### `reload_config`
- **Does**: Rebuilds the LLM-facing engines from the saved config, syncs private-chat mode into DB-backed runtime state, and wakes sleeping cognition.
- **Interacts with**: `agent::{reasoning,orientation,journal,dream,trajectory}` and the runtime control plane through `config_snapshot`.

### `config_snapshot`
- **Does**: Returns the current normalized config to the crate-internal runtime plugin control task without exposing the config lock.
- **Interacts with**: `supervise_runtime_plugins` in `runtime.rs`.
- **Rationale**: Plugin lifecycle reconciliation must continue while cognition is paused or occupied, while all callers still observe the same live config updated by `reload_config`.

### `calculate_tick_duration` / `should_dream` / `run_dream_cycle`
- **Does**: Computes adaptive ambient tick frequency from user-state estimate, decides Dream trigger windows (away/deep-night + interval gate), and makes one bounded, tool-free structured consolidation over journal, concerns, intentions, recent action, prior Dream, and current orientation
- **Interacts with**: `presence/mod.rs`, `agent/dream.rs`, and durable Dream/intention/journal/concern persistence
- **Rationale**: Dream carries revisable continuity forward without scoring personality, mutating the system prompt, or acquiring outward capabilities

### `build_private_chat_agentic_prompt_with_contributions`
- **Does**: Builds the private-chat prompt, injects the optional conversation-scoped session handoff note first, includes bounded thread-safe temporal self-context ahead of ordinary working memory in both Direct and Agentic modes, and extends Agentic prompts with bounded runtime-plugin addenda. All prompt paths receiving journal, memory, Dream, persona, orientation, intention, tool, plugin, or prior-model material include a system-level rule that treats it as untrusted evidence rather than executable instruction. Persisted orientation context carries its observation time and computed age so restart continuity cannot masquerade as a current observation.
- **Interacts with**: `agent/self_context.rs`, `tools::memory::SESSION_HANDOFF_KEY`, `AgentDatabase` working-memory/Dream/intention/concern/persona APIs, and runtime-plugin prompt-slot helpers.

### `build_private_temporal_self_context`
- **Does**: Hydrates private chat with only coarse timestamped ambient state plus open operator intentions whose source reference names the same conversation. It deliberately excludes global Dream, persona, concern, salience, anomaly, mood, and free-form orientation narratives because those stores may have absorbed another thread.
- **Interacts with**: `agent/self_context.rs`, conversation-scoped intention source references, and private/background prompt assembly.
- **Rationale**: Agent-wide continuity remains useful in ambient/Dream loops, but private conversation boundaries require an explicitly classified bridge rather than reusing a global narrative wholesale.

### Chat formatting helpers
- **Does**: Builds operator-chat prompts and serializes tool-call/thinking/media metadata into `[tool_calls]...[/tool_calls]`, `[thinking]...[/thinking]`, and `[media]...[/media]` blocks for inline UI rendering. Per-media `auto_play` is preserved as a generic boolean and defaults to `false` when a tool omits it.
- **Interacts with**: `ui/chat.rs` parser for collapsible tool details and media previews

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | `Agent::new(...).run_loop()` drives autonomous behavior without extra orchestration | Changing constructor or loop entrypoint signatures |
| `config.rs` | Three-loop fields (`enable_ambient_loop`, `ambient_min_interval_secs`, `enable_journal`, `journal_min_interval_secs`, `enable_concerns`, `enable_dream_cycle`, `dream_min_interval_secs`) plus loop controls (`max_tool_iterations`, `disable_tool_iteration_limit`, `max_chat_autonomous_turns`, `max_background_subtask_turns`, `disable_chat_turn_limit`, `disable_background_subtask_turn_limit`, `loop_heat_threshold`, `loop_similarity_threshold`, `loop_signature_window`, `loop_heat_cooldown`) control runtime behavior | Renaming/removing these loop-control fields |
| `ui/app.rs` | `AgentEvent` variants remain stable enough for chat/state rendering, including `ChatStreaming { conversation_id, content, done }`, `TokenMetrics { conversation_id, clear, samples }`, `ToolCallProgress { ... }`, `OrientationUpdate(...)`, `JournalWritten(...)`, `ConcernCreated { ... }`, and `ConcernTouched { ... }` | Renaming/removing emitted event types or metric fields |
| `database.rs` | Chat and memory APIs are available and synchronous; private chat relies on conversation-scoped context plus turn lifecycle APIs (`begin_chat_turn`, `record_chat_turn_tool_call`, `complete_chat_turn`, `fail_chat_turn`, `add_chat_message_in_turn`) | Changing DB API names, turn-state semantics, or message persistence order |
| `tools/mod.rs` | `ToolRegistry` can be shared and used in autonomous context, including supervised package tools | Removing registry injection or package tool registration |
| `tools/agentic.rs` | `AgenticLoop` accepts OpenAI-compatible endpoint and ToolContext for autonomous runs | Changing loop constructor/run signatures |
| `agent/capability_profiles.rs` | Loop context policies are resolved centrally and applied consistently across heartbeat, plugin events, and private chat | Bypassing policy resolver or changing profile semantics |
| `memory/eval.rs` | Replay evaluation functions remain deterministic and serializable | Breaking report schema or candidate IDs |
| `ui/chat.rs` | Embedded chat-metadata delimiters remain stable (`[tool_calls]`, `[thinking]`, `[media]`, `[turn_control]`) | Changing envelope formats without parser update |
| `tools/*` | Tool JSON with `media` arrays is transformed into chat-visible media payloads; any tool may request playback with per-item `auto_play: true` | Changing media extraction shape or autoplay default in formatter |
| `server.rs` | Explicit pause/status controls remain available (`set_paused`, `runtime_status`) for REST API control; `grant_session_tool_approval` is exposed via `POST /v1/agent/tools/:name/approve` | Removing pause/status/approval methods or changing returned status shape |

## Notes
- Current behavior combines periodic runtime-plugin polling with persona maintenance, optional heartbeat automation, and private chat handling.
- Plugin-event handling goes through the same multi-step tool-calling loop used by private chat, so package tools and built-in tools share one decision engine.
- Private chat replies are now scoped per conversation ID to avoid cross-thread prompt contamination.
- Long-running private chats are compacted as `summary snapshot + recent context + new messages`, with snapshots stored in DB and refreshed after configurable message deltas.
- Compaction summaries now include a bounded `Recent Reasoning Digest` synthesized from compacted-window OODA packets so older Observe/Orient/Decide/Act continuity survives transcript compression.
- Private chat emits a structured turn-control block per assistant response; continuation is model-driven (`decision=continue` + no user input needed), with optional turn caps acting only as safety rails.
- Wake signals from operator message enqueue now interrupt ambient/legacy sleep windows, reducing message-to-turn start latency during long tick intervals.
- Due scheduled jobs are claimed and enqueued atomically in SQLite at loop start, then processed by the normal chat/tool loop; run timestamps advance only when enqueue succeeds.
- Scheduled conversations use `autonomous=true` authority rather than interactive private-chat authority, so approval-required tools cannot execute merely because a job became due.
- Sleep windows are schedule-aware (`next_scheduled_job_due_at`), so ambient/legacy waits are capped by the earliest enabled job due time instead of drifting behind long poll intervals.
- Private chat continuation now also requires meaningful forward progress signals (`tool_count > 0` or `status=still_working`) before another autonomous turn is allowed.
- Private-chat execution mode is runtime-switchable: `agentic` (multi-turn continuation) or `direct` (single-turn response). Scheduled-job conversations always remain agentic.
- When private-chat continuation is still justified at the turn cap, work is handed off to a per-conversation background subtask runner instead of forcing an immediate stop.
- Foreground and background autonomous chat turns now maintain a deterministic loop-heat counter from signature similarity (response text + turn-control action + tool set). When heat crosses configured threshold, continuation/offload is blocked and the agent yields with a loop-break message.
- Agentic tool-loop iteration limits are settings-driven (`max_tool_iterations` with optional unbounded mode) for both normal agentic chat and direct operator chat; only scheduled-job conversations retain a separate hard cap.
- Private-chat autonomous turn limits are settings-driven for both foreground (`max_chat_autonomous_turns`) and detached background subtasks (`max_background_subtask_turns`), and each limit can be disabled so continuation is driven solely by model turn-control decisions.
- Turn-control parsing treats visible assistant text as authoritative; block `user_message` is only fallback when visible text is empty and does not resemble a hallucinated `User:`/`Operator:` transcript.
- Turn-control parsing tolerates malformed metadata envelopes (`[turn_control]` without closing marker) and fenced JSON payloads so continuation decisions remain stable across provider quirks.
- Private-chat prompts now include concern-priority context ahead of general working memory to bias retrieval toward ongoing topics.
- Private-chat working-memory injection is now conversation-scoped (`get_working_memory_context_for_conversation`) so daily activity logs do not interleave unrelated threads.
- Private-chat recent conversation context now uses sanitized message summaries from `database.rs` (raw metadata/tool payload blocks are compacted into terse tags).
- Private-chat prompts now include an explicit OODA section (`Observe`, `Orient`, `Decide`) sourced from latest orientation + continuation context before action generation, plus optional `Recent Action Digest` and `Previous OODA Packet` sections.
- Recent Action Digest prompt context is now phase-aware and includes compact reply/error previews in addition to decision/status/tool names.
- Completed autonomous turns now persist an OODA packet (`observe`, `orient`, `decide`, `act`) so subsequent turns and orientation refresh can consume structured turn-history context instead of raw transcript only.
- Concern lifecycle now runs in-loop: decay demotes stale concerns, mention matching reactivates them, and structured concern signals create/touch concerns explicitly.
- Ambient mode merges heartbeat scheduling into ambient ticks instead of a separate pre-cycle call.
- Dream mode is gated by real inactivity (including at least ten quiet minutes during late/deep night) and a minimum interval, then persists a bounded structured continuity artifact; a failed provider attempt is timestamped so it cannot hammer every ambient tick.
- Self-directed and Dream cadences keep separate attempt gates and structured last-outcome timestamps, preserving both backoff safety and truthful introspection.
- Tool-call progress is streamed as events during a turn so the UI can show real-time execution output (for example shell output snippets) before final reply persistence.
- Ambient mode includes a periodic self-directive pass that claims one durable intention, uses its independent autonomous capability profile, and records a leased lifecycle outcome so work can resume after restart.
- Operator requests are source-idempotent intentions keyed by the complete unread batch, with exact foreground claims acquired before execution; unavailable ownership defers the batch without marking messages processed, while background handoff keeps the same claim and records its eventual completion/block/retry result.
- Processed external-event IDs are persisted as a bounded receipt window only after accepted cognition; cancelled, exhausted, empty, and approval-blocked passes retain their host receipt for replay without unbounded state growth.
- Agent-wide loops receive an advisory `Temporal Self-Context` (Dream, public open intentions, concerns, timestamped orientation, optional self-description). Engaged/background chat receives a conversation-scoped variant containing only coarse timed ambient state and that thread's operator intentions; global narrative fields are excluded. Every historical block is explicitly untrusted data.
- Background subtasks reuse the same streaming callbacks, so detached turns still surface incremental tool output, token streaming, and token novelty metrics in activity/chat panes.
- Detached background subtasks use their own `autonomous=true` profile, preserving approval gates after the live operator turn has yielded.
- Background handle ownership is centralized in the reaper: status/activity/spawn checks never discard finished handles opportunistically, and cooperative stop never clears running blocking workers.
- Only an exact background status of `done` completes a durable intention. Input/loop blocks remain blocked, while stop/budget pauses and execution failures become retryable outcomes.
- Each autonomous private-chat turn is persisted in DB before/after execution, including tool-call lineage and terminal state (`completed`, `awaiting_approval`, or `failed`), but only the final yielded assistant message is added to chat history.
- Orientation is now refreshed once per cycle as a log-only signal: it emits `OrientationUpdate`, persists `orientation_snapshots`, and uses an input signature cache to avoid repeated LLM calls when context is unchanged.
- When `enable_screen_capture_in_loop` is true, orientation now includes a screenshot-based desktop observation summary generated via vision evaluation before prompt synthesis. Orientation captures are written to `.ponderer/orientation_latest.png` under the launch/working directory.
- Repeated orientation screenshot-capture failures are warn-once + debug thereafter to avoid log spam; macOS permission failures include a Screen Recording hint.
- Journal generation now runs off orientation disposition (`journal`) with two anti-spam guards: skip when disposition is unchanged from previous cycle, and skip until a minimum interval elapses since the last entry.
- Tool access is enforced by explicit capability profiles per interactive and autonomous loop, with optional config overrides for allow/deny lists.
- Tools declaring `external.publish` share a process-wide one-hour rolling quota. Quota is atomically reserved at each autonomous invocation, so concurrent or multi-call passes cannot overshoot it; ambiguous errors retain their slot because dispatch may have succeeded remotely. Tool names do not participate in this policy.
- Operator messages and per-turn agent outcomes now append to daily memory log keys (`activity-log-YYYY-MM-DD`) for longitudinal context.
- Heartbeat mode is guarded by config + due-time checks and is intentionally quiet when no pending tasks/reminders are found.
- Terminal private-chat turn failures now generate an explicit fallback agent message and streaming completion event instead of silently dropping the turn.
- Memory evolution scheduling is heartbeat-triggered but independently rate-limited by its own interval key in `agent_state`.
- The run loop is intentionally conservative around errors: failures emit events and continue after short backoff.
- The chat system prompt instructs the agent to call `write_session_handoff` when wrapping up a work session. The note is stored under `session-handoff:<conversation_id>` and injected only as the first context section of that conversation's next turn, enabling cold-start resumption without cross-thread consumption.
- Completion nudge (Ponderer-q2y): if the agent returns 0 tool calls to an apparent action request with status=done/still_working, and the turn limit allows another turn, `should_continue` is forced true and the next turn's `continuation_hint` is replaced with an explicit "please actually use tools" prompt. The old completion check at the bottom of the turn loop is now only reached when the nudge could not fire (at turn limit).
- Engaged private-chat prompts now have bounded plugin extension points: runtime plugins can contribute additive `engaged.context` and `engaged.instructions` blocks, but the core prompt framing still belongs to `Agent`.
- Persona evolution now notifies the runtime plugin host after snapshot persistence so side-effect plugins can react (for example, voice-profile drift) without adding domain-specific state to `Agent`.
- Runtime-plugin configuration is no longer applied by the cognitive loop; a sibling control task on the same long-lived Tokio runtime owns it independently of pause and cognitive work.
- The agent now owns one generation-event sink and assigns typed sources to operator chat, background work, heartbeat, self-direction, plugins, orientation, journal, Dream, social, vision, summaries, titles, reasoning, and persona reflection. This replaces chat-only `TokenMetrics` emission.
