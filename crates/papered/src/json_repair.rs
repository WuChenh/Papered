//! JSON repair utilities for LLM responses.
//!
//! Large language models frequently emit JSON with minor syntax issues such as
//! markdown code fences, leading/trailing prose, or trailing commas. This module
//! provides a small, dependency-free repair pipeline before falling back to a
//! stricter parser.

/// Extract a JSON object or array from text that may contain markdown code
/// blocks, leading/trailing prose, or multiple JSON snippets.
///
/// The function prefers fenced code blocks, then returns the largest balanced
/// JSON object/array found in the text.
pub fn extract_json_from_text(text: &str) -> String {
    // Prefer explicit ```json fences.
    if let Some(block) = extract_fenced_block(text, "json") {
        return block;
    }

    // Fall back to any fenced code block.
    if let Some(block) = extract_fenced_block(text, "") {
        return block;
    }

    // Find the largest balanced JSON object/array.
    if let Some((start, end)) = find_largest_balanced_json(text) {
        return text[start..end].to_string();
    }

    text.trim().to_string()
}

fn extract_fenced_block(text: &str, lang: &str) -> Option<String> {
    let fence = if lang.is_empty() {
        "```"
    } else {
        &format!("```{lang}")
    };
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find(fence) {
        let block_start = search_from + start + fence.len();
        if let Some(end) = text[block_start..].find("```") {
            let candidate = text[block_start..block_start + end].trim().to_string();
            if !candidate.is_empty() {
                return Some(candidate);
            }
            search_from = block_start + end + 3;
        } else {
            break;
        }
    }
    None
}

fn find_largest_balanced_json(text: &str) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape_next = false;
    let mut current_start: Option<usize> = None;

    for (byte_offset, ch) in text.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            match ch {
                '{' | '[' => {
                    if depth == 0 {
                        current_start = Some(byte_offset);
                    }
                    depth += 1;
                }
                '}' | ']' if depth > 0 => {
                    depth -= 1;
                    if depth == 0
                        && let Some(start) = current_start
                    {
                        let len = byte_offset + ch.len_utf8() - start;
                        if best.is_none_or(|(_, l)| l < len) {
                            best = Some((start, byte_offset + ch.len_utf8()));
                        }
                        current_start = None;
                    }
                }
                _ => {}
            }
        }
    }

    best
}

/// Attempt to repair common LLM JSON defects.
///
/// Currently handles trailing commas before `]` and `}` at any nesting level,
/// which is the most common syntax error produced by LLMs.
fn try_repair_json(raw: &str) -> Option<serde_json::Value> {
    let trimmed = raw.trim();
    let start = trimmed.find('{').or_else(|| trimmed.find('['))?;
    let end = trimmed.rfind('}').or_else(|| trimmed.rfind(']'))?;
    if end < start {
        return None;
    }

    let inner = &trimmed[start..=end];
    let repaired = remove_trailing_commas(inner);

    serde_json::from_str(&repaired).ok()
}

/// Remove trailing commas that appear immediately before a closing `]` or `}`.
/// Operates on raw bytes, but only touches commas outside of JSON strings.
fn remove_trailing_commas(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in json.char_indices() {
        if escape_next {
            out.push(ch);
            escape_next = false;
            continue;
        }

        if ch == '\\' {
            out.push(ch);
            escape_next = true;
            continue;
        }

        if ch == '"' {
            out.push(ch);
            in_string = !in_string;
            continue;
        }

        if in_string {
            out.push(ch);
            continue;
        }

        // Look for a comma followed only by whitespace and a closing bracket/brace.
        if ch == ',' {
            let after = &json[i + ch.len_utf8()..];
            if let Some(close_idx) = after.find(['}', ']'])
                && after[..close_idx].trim().is_empty()
            {
                // Skip the comma.
                continue;
            }
        }

        out.push(ch);
    }

    out
}

/// Parse an LLM response as JSON, applying extraction and repair strategies.
pub fn parse_llm_json(raw: &str) -> Option<serde_json::Value> {
    let trimmed = raw.trim();

    if let Ok(v) = serde_json::from_str(trimmed) {
        return Some(v);
    }

    let extracted = extract_json_from_text(trimmed);
    if let Ok(v) = serde_json::from_str(&extracted) {
        return Some(v);
    }

    try_repair_json(&extracted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_from_json_fence() {
        let text = "Here is the result:\n```json\n{\"a\": 1}\n```";
        assert_eq!(extract_json_from_text(text), "{\"a\": 1}");
    }

    #[test]
    fn extract_largest_balanced_object() {
        let text = "some {\"x\": 1} extra {\"y\": {\"z\": 2}} text";
        assert_eq!(extract_json_from_text(text), "{\"y\": {\"z\": 2}}");
    }

    #[test]
    fn repair_trailing_comma_in_object() {
        let raw = r#"{"a": 1,}"#;
        assert_eq!(try_repair_json(raw), Some(json!({"a": 1})));
    }

    #[test]
    fn repair_trailing_comma_in_array() {
        let raw = r#"[1, 2, 3,]"#;
        assert_eq!(try_repair_json(raw), Some(json!([1, 2, 3])));
    }

    #[test]
    fn repair_nested_trailing_comma() {
        let raw = r#"{"items": [{"a": 1,},],}"#;
        assert_eq!(try_repair_json(raw), Some(json!({"items": [{"a": 1}]})));
    }

    #[test]
    fn parse_llm_json_with_repair() {
        let raw = "```json\n{\"title\": \"Test\",}\n```";
        assert_eq!(parse_llm_json(raw), Some(json!({"title": "Test"})));
    }
}
