# character_card.rs

## Purpose
Parses character cards in multiple formats (TavernAI V2 JSON, TavernAI V2 PNG, W++, Boostyle) into a unified `ParsedCharacter` representation, and converts them into system prompts for the LLM.

## Components

### `ParsedCharacter`
- **Does**: Unified character representation with fields: `name`, `description`, `personality`, `scenario`, `example_dialogue`, `system_prompt`
- **Interacts with**: `character_to_system_prompt` (converts to prompt), `database::CharacterCard` (persisted after import)

### `parse_character_card(path)`
- **Does**: Entry point that auto-detects format. Tries PNG extraction first (for `.png` files), then TavernAI V2 JSON, W++, and Boostyle in order. Returns `(ParsedCharacter, format_string, raw_content)`.
- **Interacts with**: UI import dialog, `config::AgentConfig` character fields

### `parse_png_character_card(path)`
- **Does**: Extracts base64-encoded character JSON from PNG `tEXt` chunk with keyword "chara", decodes it, and parses as TavernAI V2
- **Rationale**: TavernAI/SillyTavern community standard for distributing character cards as PNG images

### `parse_tavernai_v2(content)`
- **Does**: Parses `TavernAICardV2` JSON (`spec`, `spec_version`, `data` with name/description/personality/scenario/mes_example/system_prompt`)

### `parse_wpp_format(content)`
- **Does**: Parses W++ structured format (`[character("Name"){Personality("traits") Mind("traits") Description("text")}]`)
- **Rationale**: Legacy format from some character card communities

### `parse_boostyle_format(content)`
- **Does**: Parses plain-text labeled sections (`Name:`, `Personality:`, `Description:`, `Scenario:`, `Example Dialogue:`)

### `character_to_system_prompt(character)`
- **Does**: Assembles a system prompt string from `ParsedCharacter` fields, using the explicit `system_prompt` if present or building from components
- **Interacts with**: `agent::Agent` (sets the LLM system prompt from imported character)

### `TavernAICardV2` / `TavernAIData`
- **Does**: Serde structs matching the TavernAI V2 JSON spec

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| UI import flow | `parse_character_card(path)` returns `Result<(ParsedCharacter, String, String)>` | Changing return tuple structure |
| `agent::Agent` | `character_to_system_prompt` returns a valid system prompt string | Changing output format |
| `database.rs` | Format string ("tavernai_v2", "tavernai_v2_png", "wpp", "boostyle") stored in `character_cards.format` | Changing format identifiers |

## Notes
- PNG chunk parsing is done manually (same approach as `comfy_workflow.rs`), no PNG library dependency.
- W++ parser is basic -- only extracts `character()`, `Personality()`, `Mind()`, and `Description()` blocks. Nested or multi-line W++ may not parse correctly.
- Boostyle parser is line-based and does not support multi-line field values.
- Format detection is sequential: first match wins. A file that happens to be valid TavernAI V2 JSON will never be tried as W++.
