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

/// Compute a simple content hash for cache invalidation.
pub fn content_hash(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
