//! Prompt construction for one-shot paper "takes": a short LLM-generated
//! insight / critique / inspiration draft that the user can edit and save
//! as a note from the paper-detail page.
//!
//! The generation itself is done by the daemon's insights endpoint using the
//! `rag` purpose model; this module only owns the prompt text so it stays
//! unit-testable and reusable.

/// System prompt for insight generation.
const SYSTEM_PROMPT: &str = "You are a brilliant, slightly contrarian senior researcher \
giving a colleague a quick hallway take on a paper. Your takes are famous for being \
concise, specific, and genuinely useful — you never flatter and never write boilerplate.";

/// Hard cap on abstract characters included in the prompt. Abstracts are
/// normally well under 2k chars; this only guards against pathological input.
const MAX_ABSTRACT_CHARS: usize = 12_000;

/// Build the (system, user) prompt pair for a quick take on a paper.
///
/// `venue` / `published_date` / `abstract_text` are optional metadata; the
/// prompt degrades gracefully to title-only when they are missing.
pub fn insight_prompts(
    title: &str,
    authors: &[String],
    venue: Option<&str>,
    published_date: Option<&str>,
    abstract_text: Option<&str>,
) -> (String, String) {
    let mut user = format!("Give your quick take on this paper:\n\nTitle: {title}\n");

    if !authors.is_empty() {
        let mut names: Vec<&str> = authors.iter().take(3).map(String::as_str).collect();
        if authors.len() > 3 {
            names.push("et al.");
        }
        user.push_str(&format!("Authors: {}\n", names.join(", ")));
    }

    let venue_line = match (venue, published_date) {
        (Some(v), Some(d)) => Some(format!("{v}, {d}")),
        (Some(v), None) => Some(v.to_string()),
        (None, Some(d)) => Some(d.to_string()),
        (None, None) => None,
    };
    if let Some(line) = venue_line {
        user.push_str(&format!("Venue: {line}\n"));
    }

    if let Some(abs) = abstract_text.filter(|a| !a.trim().is_empty()) {
        let truncated: String = abs.chars().take(MAX_ABSTRACT_CHARS).collect();
        user.push_str(&format!("\nAbstract:\n{truncated}\n"));
    }

    user.push_str(
        r#"
Reply with EXACTLY three paragraphs, each starting with its label, and nothing else:

💡 Insight: the single most non-obvious takeaway — what the paper really contributes beyond what the abstract advertises, or why it matters. 1–2 sentences. It must be specific to THIS paper; if the sentence could describe any paper in the field, rewrite it.

🔍 Critique: the sharpest weakness — a hidden assumption, a missing baseline, an overclaimed conclusion, or a threat to validity. 1–2 sentences, concrete. Banned: "more experiments are needed", "future work", and other reviewer boilerplate.

🌱 Inspiration: 1–2 concrete, actionable research ideas this paper sparks — a follow-up study, an application to a different problem, or a combination with another method. Name each idea specifically; don't wave at it.

Hard limit: 120 words total. No preamble, no sign-off. Write in the same language as the abstract."#,
    );

    (SYSTEM_PROMPT.to_string(), user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_metadata_prompt_contains_all_parts() {
        let authors = vec!["Ada Lovelace".to_string(), "Alan Turing".to_string()];
        let (system, user) = insight_prompts(
            "A Great Paper",
            &authors,
            Some("Nature"),
            Some("2024"),
            Some("We show that testing prompts is worthwhile."),
        );
        assert!(system.contains("concise, specific"));
        assert!(user.contains("Title: A Great Paper"));
        assert!(user.contains("Authors: Ada Lovelace, Alan Turing"));
        assert!(user.contains("Venue: Nature, 2024"));
        assert!(user.contains("Abstract:\nWe show that testing prompts is worthwhile."));
        assert!(user.contains("💡 Insight:"));
        assert!(user.contains("🔍 Critique:"));
        assert!(user.contains("🌱 Inspiration:"));
    }

    #[test]
    fn title_only_prompt_omits_missing_metadata() {
        let (system, user) = insight_prompts("Bare Title", &[], None, None, None);
        assert!(!system.is_empty());
        assert!(user.contains("Title: Bare Title"));
        assert!(!user.contains("Authors:"));
        assert!(!user.contains("Venue:"));
        assert!(!user.contains("Abstract:"));
        // The three required sections are still requested.
        assert!(user.contains("💡 Insight:"));
    }

    #[test]
    fn author_list_truncates_with_et_al() {
        let authors: Vec<String> = (0..5).map(|i| format!("Author {i}")).collect();
        let (_, user) = insight_prompts("T", &authors, None, None, Some("a"));
        assert!(user.contains("Author 0, Author 1, Author 2, et al."));
        assert!(!user.contains("Author 3"));
    }

    #[test]
    fn overlong_abstract_is_truncated() {
        let long = "x".repeat(MAX_ABSTRACT_CHARS + 100);
        let (_, user) = insight_prompts("T", &[], None, None, Some(&long));
        let abstract_body = user.split("Abstract:\n").nth(1).unwrap();
        assert!(abstract_body.starts_with(&"x".repeat(MAX_ABSTRACT_CHARS)));
        assert!(!abstract_body.contains(&"x".repeat(MAX_ABSTRACT_CHARS + 1)));
    }
}
