# database/persona.rs

## Purpose
Types and `AgentDatabase` methods related to persona evolution tracking, reflection history, character cards, and system prompt state.

## Components

### `PersonaSnapshot`
- **Does**: Captures the agent's personality state at a point in time, including dynamic `PersonaTraits` dimensions, system prompt, trigger event, LLM self-description, inferred trajectory, and formative experiences
- **Rationale**: Core data structure for "Ludonarrative Assonantic Tracing" -- tracking personality evolution over time

### `PersonaTraits`
- **Does**: Flexible `HashMap<String, f64>` mapping dimension names to 0.0-1.0 scores; avoids fixed personality models and allows researcher-defined axes
- **Interacts with**: `save_persona_snapshot` / `get_persona_history` for JSON serialization round-trip

### `ReflectionRecord`
- **Does**: Logs each self-reflection event with old/new system prompts, reasoning, and guiding principles JSON

### `CharacterCard`
- **Does**: Stores imported character card metadata (format, raw data, derived prompt); singleton pattern -- only one card kept at a time

### Reflection methods
- `save_reflection` / `get_reflection_history` — append and query reflection log

### System prompt / state methods
- `get_current_system_prompt` / `set_current_system_prompt`
- `get_last_reflection_time` / `set_last_reflection_time`

### Character card methods
- `save_character_card` — deletes all existing before inserting (singleton)
- `get_character_card` / `delete_character_card`

### Persona history methods
- `save_persona_snapshot` / `get_persona_history` / `get_persona_history_range` / `get_latest_persona` / `count_persona_snapshots`

## Notes
- Persona traits are serialized to JSON for the `traits_json` column; formative experiences are also JSON arrays
- Date range queries support researcher analysis of personality drift over specific intervals
