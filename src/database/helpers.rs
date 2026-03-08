use std::collections::HashSet;

use crate::memory::archive::PromotionOutcome;

pub(super) fn short_conversation_tag(conversation_id: &str) -> String {
    truncate_for_db_digest(conversation_id.trim(), 12)
}

pub(super) fn filter_activity_log_for_conversation(
    content: &str,
    conversation_tag: &str,
    max_lines: usize,
) -> Option<String> {
    let mut lines = content.lines();
    let heading = lines.next().unwrap_or_default().trim();
    let relevant_lines = lines
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return false;
            }

            if trimmed.contains(&format!("[{}]", conversation_tag)) {
                return true;
            }

            let lowered = trimmed.to_ascii_lowercase();
            !lowered.contains("operator [")
                && !lowered.contains("agent [")
                && !lowered.contains("chat [")
                && !lowered.contains("conversation [")
        })
        .map(str::to_string)
        .collect::<Vec<_>>();

    if relevant_lines.is_empty() {
        return None;
    }

    let keep_from = relevant_lines.len().saturating_sub(max_lines.max(1));
    let mut rendered = String::new();
    if !heading.is_empty() {
        rendered.push_str(heading);
        rendered.push_str("\n\n");
    }
    rendered.push_str(&relevant_lines[keep_from..].join("\n"));
    Some(rendered)
}

pub(super) fn summarize_chat_message_for_context(content: &str) -> String {
    use super::chat::{
        CHAT_CONCERNS_BLOCK_END, CHAT_CONCERNS_BLOCK_START, CHAT_MEDIA_BLOCK_END,
        CHAT_MEDIA_BLOCK_START, CHAT_THINKING_BLOCK_END, CHAT_THINKING_BLOCK_START,
        CHAT_TOOL_BLOCK_END, CHAT_TOOL_BLOCK_START, CHAT_TURN_CONTROL_BLOCK_END,
        CHAT_TURN_CONTROL_BLOCK_START,
    };
    let (without_tools, tool_blocks) =
        extract_tagged_blocks(content, CHAT_TOOL_BLOCK_START, CHAT_TOOL_BLOCK_END);
    let (without_thinking, thinking_blocks) = extract_tagged_blocks(
        &without_tools,
        CHAT_THINKING_BLOCK_START,
        CHAT_THINKING_BLOCK_END,
    );
    let (without_media, media_blocks) = extract_tagged_blocks(
        &without_thinking,
        CHAT_MEDIA_BLOCK_START,
        CHAT_MEDIA_BLOCK_END,
    );
    let (without_turn_control, turn_control_blocks) = extract_tagged_blocks(
        &without_media,
        CHAT_TURN_CONTROL_BLOCK_START,
        CHAT_TURN_CONTROL_BLOCK_END,
    );
    let (visible_text, concern_blocks) = extract_tagged_blocks(
        &without_turn_control,
        CHAT_CONCERNS_BLOCK_START,
        CHAT_CONCERNS_BLOCK_END,
    );

    let compact_visible = compact_whitespace(&visible_text);
    let mut tags: Vec<String> = Vec::new();
    if let Some(summary) = summarize_tool_call_blocks(&tool_blocks) {
        tags.push(summary);
    }
    if let Some(summary) = summarize_media_blocks(&media_blocks) {
        tags.push(summary);
    }
    if let Some(summary) = summarize_thinking_blocks(&thinking_blocks) {
        tags.push(summary);
    }
    if let Some(summary) = summarize_turn_control_blocks(&turn_control_blocks) {
        tags.push(summary);
    }
    if let Some(summary) = summarize_concern_blocks(&concern_blocks) {
        tags.push(summary);
    }

    let mut rendered = if compact_visible.is_empty() {
        String::new()
    } else {
        compact_visible
    };

    if !tags.is_empty() {
        if !rendered.is_empty() {
            rendered.push(' ');
        }
        rendered.push('[');
        rendered.push_str(&tags.join(" | "));
        rendered.push(']');
    }

    truncate_for_db_digest(rendered.trim(), 900)
}

pub(super) fn extract_tagged_blocks(
    content: &str,
    start_tag: &str,
    end_tag: &str,
) -> (String, Vec<String>) {
    let mut remaining = content.to_string();
    let mut blocks: Vec<String> = Vec::new();

    loop {
        let Some(start_idx) = remaining.find(start_tag) else {
            break;
        };

        let mut block_start = start_idx + start_tag.len();
        if remaining[block_start..].starts_with('\n') {
            block_start += 1;
        }

        let (block_raw, next_remaining) =
            if let Some(rel_end) = remaining[block_start..].find(end_tag) {
                let block_end = block_start + rel_end;
                let mut suffix_start = block_end + end_tag.len();
                if remaining[suffix_start..].starts_with('\n') {
                    suffix_start += 1;
                }
                (
                    remaining[block_start..block_end].to_string(),
                    format!("{}{}", &remaining[..start_idx], &remaining[suffix_start..]),
                )
            } else {
                (
                    remaining[block_start..].to_string(),
                    remaining[..start_idx].to_string(),
                )
            };

        let trimmed = block_raw.trim();
        if !trimmed.is_empty() {
            blocks.push(trimmed.to_string());
        }
        remaining = next_remaining;
    }

    (remaining, blocks)
}

pub(super) fn summarize_tool_call_blocks(blocks: &[String]) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }

    let mut total = 0usize;
    let mut names: Vec<String> = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut error_count = 0usize;
    let mut parse_failures = 0usize;

    for block in blocks {
        match serde_json::from_str::<serde_json::Value>(block) {
            Ok(serde_json::Value::Array(items)) => {
                total += items.len();
                for item in items {
                    let tool_name = item
                        .get("tool_name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    if let Some(name) = tool_name {
                        if seen_names.insert(name.to_string()) && names.len() < 3 {
                            names.push(name.to_string());
                        }
                    }

                    let is_error_kind = item
                        .get("output_kind")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .map(|kind| kind.eq_ignore_ascii_case("error"))
                        .unwrap_or(false);
                    let output_preview_error = item
                        .get("output_preview")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .map(|preview| {
                            let lowered = preview.to_ascii_lowercase();
                            lowered.contains("[error]") || lowered.contains("error:")
                        })
                        .unwrap_or(false);
                    if is_error_kind || output_preview_error {
                        error_count += 1;
                    }
                }
            }
            _ => parse_failures += 1,
        }
    }

    if total == 0 && parse_failures == 0 {
        return None;
    }

    let mut summary = if total > 0 {
        format!("tools={}", total)
    } else {
        "tools=metadata".to_string()
    };
    if !names.is_empty() {
        let extra = total.saturating_sub(names.len());
        if extra > 0 {
            summary.push_str(&format!(" ({} +{})", names.join(","), extra));
        } else {
            summary.push_str(&format!(" ({})", names.join(",")));
        }
    }
    if error_count > 0 {
        summary.push_str(&format!(", errors={}", error_count));
    }
    if parse_failures > 0 {
        summary.push_str(", raw=true");
    }
    Some(summary)
}

pub(super) fn summarize_media_blocks(blocks: &[String]) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }

    let mut total = 0usize;
    let mut kinds: Vec<String> = Vec::new();
    let mut seen_kinds: HashSet<String> = HashSet::new();
    let mut parse_failures = 0usize;

    for block in blocks {
        match serde_json::from_str::<serde_json::Value>(block) {
            Ok(serde_json::Value::Array(items)) => {
                total += items.len();
                for item in items {
                    let kind = item
                        .get("media_kind")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| item.get("kind").and_then(serde_json::Value::as_str))
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    if let Some(kind) = kind {
                        if seen_kinds.insert(kind.to_string()) && kinds.len() < 2 {
                            kinds.push(kind.to_string());
                        }
                    }
                }
            }
            _ => parse_failures += 1,
        }
    }

    if total == 0 && parse_failures == 0 {
        return None;
    }

    let mut summary = if total > 0 {
        format!("media={}", total)
    } else {
        "media=metadata".to_string()
    };
    if !kinds.is_empty() {
        summary.push_str(&format!(" ({})", kinds.join(",")));
    }
    if parse_failures > 0 {
        summary.push_str(", raw=true");
    }
    Some(summary)
}

pub(super) fn summarize_thinking_blocks(blocks: &[String]) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }

    let mut hints = 0usize;
    for block in blocks {
        if let Ok(serde_json::Value::Array(items)) =
            serde_json::from_str::<serde_json::Value>(block)
        {
            hints += items.len();
        }
    }

    Some(if hints > 0 {
        format!("thinking={} hidden", hints)
    } else {
        format!("thinking={} block(s) hidden", blocks.len())
    })
}

pub(super) fn summarize_turn_control_blocks(blocks: &[String]) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }

    for block in blocks.iter().rev() {
        let payload = block
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            continue;
        };
        let decision = value
            .get("decision")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if decision.is_some() || status.is_some() {
            return Some(format!(
                "turn={}/{}",
                decision.unwrap_or("-"),
                status.unwrap_or("-")
            ));
        }
    }

    Some("turn=metadata".to_string())
}

pub(super) fn summarize_concern_blocks(blocks: &[String]) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }

    let mut total = 0usize;
    for block in blocks {
        if let Ok(serde_json::Value::Array(items)) =
            serde_json::from_str::<serde_json::Value>(block)
        {
            total += items.len();
        }
    }

    Some(if total > 0 {
        format!("concerns={}", total)
    } else {
        "concerns=metadata".to_string()
    })
}

pub(super) fn compact_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn truncate_for_db_digest(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

pub(super) fn outcome_to_db(outcome: &PromotionOutcome) -> &'static str {
    match outcome {
        PromotionOutcome::Promote => "promote",
        PromotionOutcome::Hold => "hold",
    }
}
