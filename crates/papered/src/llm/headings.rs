//! Index-time repair of PDF hyphenation artifacts in heading titles.
//!
//! PDF text extraction can split a word across a line break and rejoin it
//! with a space ("frame-\nwork" → "frame work"). Headings are few and short,
//! so a single small LLM call per paper — run once at index time, before
//! chunks are persisted — repairs them for every downstream consumer (TOC,
//! heading-path search, REST API, UI).
//!
//! A lightweight pre-filter inspects the headings first: if no title contains
//! a pattern that looks like a hyphenation split (single-char fragments, or
//! consecutive short word pairs), the LLM call is skipped entirely.

use crate::llm::client::LlmClient;
use std::collections::HashSet;
use std::sync::LazyLock;

/// Words that legitimately appear as lowercase, space-separated neighbours in
/// headings — function words, Latin terms, and the generic academic vocabulary
/// of sentence-case titles ("A novel method", "Results and discussion",
/// "machine learning"). Two adjacent lowercase words are only treated as a
/// hyphenation split when NEITHER is in this list; a false positive here costs
/// one echo LLM call, a false negative loses a real repair, so the list errs
/// toward common heading words.
static COMMON_HEADING_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // Function words.
        "a",
        "an",
        "and",
        "are",
        "as",
        "at",
        "be",
        "by",
        "for",
        "from",
        "in",
        "into",
        "is",
        "it",
        "its",
        "of",
        "on",
        "or",
        "per",
        "the",
        "to",
        "via",
        "was",
        "were",
        "with",
        // Latin terms common in academic headings.
        "de",
        "et",
        "al",
        "vivo",
        "vitro",
        "situ",
        "silico",
        "novo",
        // Generic academic vocabulary of sentence-case headings.
        "novel",
        "new",
        "method",
        "methods",
        "approach",
        "approaches",
        "analysis",
        "analyses",
        "study",
        "studies",
        "results",
        "discussion",
        "conclusion",
        "conclusions",
        "introduction",
        "overview",
        "review",
        "learning",
        "deep",
        "machine",
        "model",
        "models",
        "network",
        "networks",
        "system",
        "systems",
        "data",
        "based",
        "using",
        "role",
        "effect",
        "effects",
        "mechanism",
        "mechanisms",
        "insights",
        "evidence",
        "prediction",
        "detection",
        "identification",
        "characterization",
        "evaluation",
        "comparison",
        "between",
        "among",
        "across",
        "during",
        "their",
        "these",
        "those",
        "reveals",
        "identifies",
        // Biology vocabulary frequent in this library's headings.
        "gene",
        "genes",
        "expression",
        "regulation",
        "regulatory",
        "protein",
        "proteins",
        "cell",
        "cells",
        "cellular",
        "dna",
        "rna",
        "chromatin",
        "transcription",
        "enhancer",
        "enhancers",
    ]
    .into_iter()
    .collect()
});

/// A short alphabetic fragment (2–3 chars) that is not a recognized word —
/// the shape of one half of a hyphenation split ("lev erages", "st
/// ate-of-the-art"). Measured in chars, not bytes, so a single CJK character
/// (3 bytes in UTF-8) is never mistaken for a 3-letter fragment.
fn is_short_fragment(word: &str) -> bool {
    let len = word.chars().count();
    (2..=3).contains(&len)
        && word.chars().all(|c| c.is_ascii_alphabetic())
        && !COMMON_HEADING_WORDS.contains(word.to_lowercase().as_str())
}

/// A longer word (≥4 chars) starting lowercase — the other half of a split.
/// Hyphens are allowed so "ate-of-the-art" still counts as one long word.
fn is_longer_lowercase_word(word: &str) -> bool {
    word.chars().count() >= 4
        && word.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
        && word.starts_with(char::is_lowercase)
}

/// True when the pair `first second` looks like a single word split across a
/// PDF line break ("frame work", "inter pretable"): both halves are
/// lowercase-starting alphabetic words of ≥4 chars, and neither is a common
/// standalone heading word (which is what keeps "novel method" or "machine
/// learning" from being flagged).
fn looks_like_split_long_pair(first: &str, second: &str) -> bool {
    is_longer_lowercase_word(first)
        && !first.contains('-')
        && is_longer_lowercase_word(second)
        && !second.contains('-')
        && !COMMON_HEADING_WORDS.contains(first.to_lowercase().as_str())
        && !COMMON_HEADING_WORDS.contains(second.to_lowercase().as_str())
}

/// Return true when any heading contains a pattern likely caused by PDF
/// hyphenation: a lone lowercase single-letter fragment ("st ate-of-the-art"
/// style breaks that leave a one-letter piece), a short fragment glued to a
/// longer lowercase word ("lev erages"), or two lowercase long-word halves
/// that are not common standalone words ("frame work", "inter pretable").
fn likely_has_split_words(titles: &[String]) -> bool {
    titles.iter().any(|t| {
        let words: Vec<&str> = t.split_whitespace().collect();
        // A lone lowercase ASCII letter (not the article "a" or pronoun "i").
        // CJK single characters are legitimate words and never flagged.
        if words.iter().any(|w| {
            w.chars().count() == 1
                && w.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && !matches!(*w, "a" | "i")
        }) {
            return true;
        }
        words.windows(2).any(|pair| {
            (is_short_fragment(pair[0]) && is_longer_lowercase_word(pair[1]))
                || (is_short_fragment(pair[1]) && is_longer_lowercase_word(pair[0]))
                || looks_like_split_long_pair(pair[0], pair[1])
        })
    })
}

/// Repair split-word artifacts in `titles` in place via one LLM call.
///
/// A correction is applied only when it differs from the original solely by
/// whitespace changes (a pure word merge/split) — semantic rewrites by the
/// model are rejected. Failures are logged and leave `titles` unchanged.
///
/// A pre-filter skips the LLM call entirely when no heading contains a pattern
/// that looks like a hyphenation artifact, avoiding unnecessary API calls.
pub async fn fix_hyphenated_headings(client: &LlmClient, titles: &mut [String]) {
    if titles.is_empty() || !likely_has_split_words(titles) {
        return;
    }

    let mut prompt = String::from(
        "Below are headings from an academic paper. Some words may have been split by PDF line \
         breaks (e.g. \"inter pretable\" should be \"interpretable\", \"frame work\" should be \
         \"framework\"). Output one line per input line: the input number, a tab, then the \
         heading with broken words rejoined. Merge fragments only when they are not standalone \
         words in context — leave genuine multi-word phrases (\"in vivo\", \"de novo\", \
         \"B cell\") untouched. Never rephrase, translate, or fix grammar; echo correct \
         headings unchanged.\n\n",
    );
    for (i, title) in titles.iter().enumerate() {
        prompt.push_str(&format!("{i}\t{title}\n"));
    }

    let response = match client
        .generate(
            "You repair PDF hyphenation artifacts in academic paper headings. \
             Respond ONLY with the numbered lines, one per line.",
            &prompt,
            8192,
            0.0,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Heading hyphenation repair LLM call failed: {e}");
            return;
        }
    };

    for line in response.lines() {
        let line = line.trim();
        let Some((idx_str, corrected)) = line.split_once('\t') else {
            continue;
        };
        let Ok(idx) = idx_str.trim().parse::<usize>() else {
            continue;
        };
        let corrected = corrected.trim();
        let Some(title) = titles.get_mut(idx) else {
            continue;
        };
        // Safety invariant: accept only pure whitespace changes (word
        // merges/splits). Anything else is a semantic rewrite — reject it.
        let stripped = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        if !corrected.is_empty() && corrected != title && stripped(corrected) == stripped(title) {
            *title = corrected.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flagged(titles: &[&str]) -> bool {
        let owned: Vec<String> = titles.iter().map(|s| s.to_string()).collect();
        likely_has_split_words(&owned)
    }

    #[test]
    fn detects_hyphenation_splits() {
        // Long-word halves split mid-word — the cases the old length bounds
        // (≤3/≤4 chars) could never match.
        assert!(flagged(&["A frame work for enhancer prediction"]));
        assert!(flagged(&["An inter pretable deep learning model"]));
        // Short fragment glued to a longer word.
        assert!(flagged(&["The model lev erages regulatory lexicons"]));
        // Fragment next to a hyphenated remainder.
        assert!(flagged(&["A st ate-of-the-art pipeline"]));
        // Lone lowercase single-letter fragment.
        assert!(flagged(&["The binding x domain revisited"]));
    }

    #[test]
    fn ignores_legitimate_headings() {
        assert!(!flagged(&["Deep learning"]));
        assert!(!flagged(&["In vitro"]));
        assert!(!flagged(&["Results and discussion"]));
        assert!(!flagged(&["A novel method"]));
        // Sentence-case heading full of common words.
        assert!(!flagged(&[
            "A novel deep learning method for the prediction of gene expression"
        ]));
        // Capitalized article at the start is not a fragment.
        assert!(!flagged(&["A framework for interpretable models"]));
    }

    #[test]
    fn cjk_single_char_is_not_a_fragment() {
        // A one-character CJK word is 3 bytes in UTF-8 — byte-length checks
        // would misjudge it; char-count checks must not flag it.
        assert!(!flagged(&["基于 中 间特征的调控元件识别模型"]));
    }
}
