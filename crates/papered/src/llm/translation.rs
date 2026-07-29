//! LLM-based translation of paper content (titles, sections, figure captions).

use crate::error::{PaperedError, Result};
use crate::llm::client::LlmClient;

/// Build the system prompt for translation based on content type.
fn translation_system_prompt(content_type: &str, target_language: &str) -> String {
    let type_hint = match content_type {
        "title" => "a short academic paper title (one line)",
        "abstract" => "an academic paper abstract",
        "section" => "a section of an academic paper",
        "figure_caption" => "a figure caption from a scientific paper",
        _ => "academic content",
    };

    format!(
        "You are an expert academic translator. Translate {type_hint} into {target_language}.\n\
         Rules:\n\
         - Preserve all technical terms, gene/protein names, species names, and proper nouns in their original form.\n\
         - Keep LaTeX formulas, citations ([1], [2,3]), and reference markers unchanged.\n\
         - Maintain the original paragraph structure and formatting.\n\
         - Output ONLY the translated text — no explanations, no notes, no preamble.\n\
         - If the content is already in {target_language}, return it unchanged."
    )
}

/// Translate a single piece of text.
pub async fn translate_text(
    client: &LlmClient,
    text: &str,
    content_type: &str,
    target_language: &str,
) -> Result<String> {
    if text.trim().is_empty() {
        return Ok(String::new());
    }

    let system = translation_system_prompt(content_type, target_language);
    let user = format!("Translate the following into {target_language}:\n\n{text}");

    // Estimate tokens: ~4 chars per token for English, ~2 for CJK.
    // Cap at a reasonable limit to avoid exceeding model context.
    let max_tokens = 16384;

    client
        .generate(&system, &user, max_tokens, 0.3)
        .await
        .map_err(|e| PaperedError::LlmGeneration(format!("Translation failed: {e}")))
}

/// Translate multiple pieces of the same content type in one call.
/// Returns `(content_ref, translated_text)` pairs in the same order.
///
/// For large batches, falls back to individual translations to avoid
/// exceeding context limits.
pub async fn translate_batch(
    client: &LlmClient,
    items: &[(String, String)], // (content_ref, text)
    content_type: &str,
    target_language: &str,
) -> Result<Vec<(String, String)>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }

    // For single items or very long combined text, translate individually.
    let total_chars: usize = items.iter().map(|(_, t)| t.len()).sum();
    if items.len() == 1 || total_chars > 50_000 {
        let mut results = Vec::with_capacity(items.len());
        for (ref_id, text) in items {
            let translated = translate_text(client, text, content_type, target_language).await?;
            results.push((ref_id.clone(), translated));
        }
        return Ok(results);
    }

    // Batch translation: number each item so the model can return them in order.
    let system = translation_system_prompt(content_type, target_language);

    let mut user_parts = Vec::new();
    for (i, (ref_id, text)) in items.iter().enumerate() {
        user_parts.push(format!("[Item {i}] ({ref_id})\n{text}"));
    }
    let user = format!(
        "Translate each of the following {} items into {target_language}. \
         Keep the [Item N] labels in your response:\n\n{}",
        items.len(),
        user_parts.join("\n\n---\n\n")
    );

    let response = client
        .generate(&system, &user, 32768, 0.3)
        .await
        .map_err(|e| PaperedError::LlmGeneration(format!("Batch translation failed: {e}")))?;

    // Parse the response: try to match [Item N] labels back to content_refs.
    let mut results = Vec::with_capacity(items.len());
    let mut current_text = String::new();
    let mut current_idx: Option<usize> = None;

    for line in response.lines() {
        if let Some(rest) = line.strip_prefix("[Item ") {
            // Save previous item.
            if let Some(idx) = current_idx
                && idx < items.len()
            {
                results.push((items[idx].0.clone(), current_text.trim().to_string()));
            }
            // Parse new item index.
            if let Some(bracket_pos) = rest.find(']') {
                let idx_str = &rest[..bracket_pos];
                if let Ok(idx) = idx_str.trim().parse::<usize>() {
                    current_idx = Some(idx);
                    // The rest of the line after "] " may contain text.
                    let after = rest[bracket_pos + 1..]
                        .trim()
                        .trim_start_matches(|c: char| c.is_whitespace() || c == '(' || c == ')');
                    // Skip the "(ref_id)" part if present.
                    current_text = if after.contains(')') {
                        after
                            .split_once(')')
                            .map(|(_, t)| t.trim().to_string())
                            .unwrap_or_default()
                    } else {
                        after.to_string()
                    };
                } else {
                    current_idx = None;
                    current_text = String::new();
                }
            }
        } else if current_idx.is_some() {
            if !current_text.is_empty() {
                current_text.push('\n');
            }
            current_text.push_str(line);
        }
    }
    // Push the last item.
    if let Some(idx) = current_idx
        && idx < items.len()
    {
        results.push((items[idx].0.clone(), current_text.trim().to_string()));
    }

    // If parsing failed (wrong number of results), fall back to individual translations.
    if results.len() != items.len() {
        tracing::warn!(
            "Batch translation parsing returned {} items, expected {} — falling back to individual",
            results.len(),
            items.len()
        );
        let mut fallback = Vec::with_capacity(items.len());
        for (ref_id, text) in items {
            let translated = translate_text(client, text, content_type, target_language).await?;
            fallback.push((ref_id.clone(), translated));
        }
        return Ok(fallback);
    }

    Ok(results)
}

/// Compute a simple content hash for cache invalidation.
pub fn content_hash(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
