//! Index-time repair of PDF text-extraction artifacts in paragraph chunks.
//!
//! pdf_oxide can leave stray intra-word spaces in justified text ("framew ork"
//! → "framework", "lev erages" → "leverages"). Line-break hyphenation is
//! already fixed deterministically upstream ([`crate::util::fix_pdf_hyphenation`]);
//! this module handles the harder cases that need language understanding, via
//! the LLM. It is the body-text counterpart to [`crate::llm::headings`].
//!
//! Token cost is kept low by design: a pre-filter selects only paragraph chunks
//! that actually look damaged, those are batched by character budget, and a
//! safety invariant accepts only pure whitespace changes — so the model can
//! never rewrite content. MinerU output is clean and never reaches this path.

use crate::chunker::Chunk;
use crate::llm::client::LlmClient;
use std::collections::HashSet;
use std::sync::LazyLock;

/// Max input chars per LLM batch. Keeps each call small and its output well
/// within the token budget; a single oversized chunk is sent alone.
const BATCH_CHAR_BUDGET: usize = 2500;
/// Output token budget per batch. Must be generous because reasoning models
/// (e.g. deepseek-v4-flash) spend part of the budget on chain-of-thought before
/// producing content. Unused tokens are never generated, so a high ceiling does
/// not raise cost — it only prevents truncation of the actual content.
const MAX_TOKENS: usize = 16384;
/// Hard cap on LLM batches per paper. Each batch can cost up to MAX_TOKENS
/// output tokens, so 8 batches bounds one pathological paper at ~131k output
/// tokens (~20k input chars). Beyond that, the extraction is likely damaged
/// wholesale (e.g. every word split) and per-batch repair is the wrong tool —
/// the rest is skipped with a warning instead of letting token spend grow
/// unbounded.
const MAX_REPAIR_BATCHES: usize = 8;

const SYSTEM_PROMPT: &str = "You repair PDF text-extraction artifacts (stray intra-word spaces) \
in academic paper paragraphs. Respond ONLY with the marked paragraphs, preserving the <<<N>>> \
markers and their order.";

/// Short (≤3-letter) English words that legitimately sit next to long words in
/// running prose ("the protein", "in vivo", "was resolved"). A short fragment
/// adjacent to a long word is only treated as an artifact when it is NOT one of
/// these. Longer words need not be listed — the length check filters first.
/// "de" is included because "de novo" / "de facto" are ubiquitous in academic
/// prose, far more common than a genuine "de <rest>" split fragment. "ray" is
/// included for the "X ray" symbol compound ("an X ray image"), which the pair
/// rule would otherwise catch as ("ray", <long word>).
static SHORT_STOPWORDS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    [
        "an", "as", "at", "be", "by", "de", "do", "he", "if", "in", "is", "it", "me", "my", "no",
        "of", "on", "or", "so", "to", "up", "us", "we", "am", "eg", "ie", "vs", "are", "was",
        "has", "had", "not", "but", "and", "the", "for", "its", "his", "her", "our", "can", "may",
        "use", "via", "per", "all", "any", "few", "how", "who", "why", "ray", "rays",
    ]
    .into_iter()
    .map(String::from)
    .collect()
});

/// Matches a `<<<N>>>` marker (the delimiter between sections of an LLM
/// response). Used to locate section boundaries; content extraction is done
/// programmatically because the `regex` crate lacks look-ahead support.
static MARKER_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"<<<(\d+)>>>").expect("valid marker regex"));

/// True when the token following a lone single-letter word marks it as a
/// legitimate academic symbol rather than a split fragment: an operator or
/// number ("n = 30", "p < 0.05", "r 2"), or a conventional companion noun
/// ("p value", "x axis", "gamma ray" style usages lowercased by the text).
fn is_symbol_companion(next: &str) -> bool {
    if next.starts_with(['=', '<', '>']) || next.starts_with(|c: char| c.is_ascii_digit()) {
        return true;
    }
    let stripped = next.trim_end_matches(|c: char| c.is_ascii_punctuation());
    matches!(
        stripped.to_lowercase().as_str(),
        "value" | "values" | "axis" | "axes" | "ray" | "rays" | "wave" | "waves"
    )
}

/// True when a paragraph chunk shows patterns typical of pdf_oxide extraction
/// damage: a lone single-letter fragment, or a short non-stopword fragment
/// glued to a longer lowercase word ("framew ork", "lev erages"). Used purely
/// as a pre-filter to decide whether to spend an LLM call on the chunk; clean
/// chunks are never sent.
#[must_use]
pub fn chunk_likely_has_artifacts(content: &str) -> bool {
    let words: Vec<&str> = content.split_whitespace().collect();
    // A lone lowercase single letter (not the article/pronoun "a"/"i") is
    // almost always a split fragment in running prose — unless it is a
    // statistical/scientific symbol with its companion right next to it
    // ("p value", "n = 30", "x axis").
    for (i, w) in words.iter().enumerate() {
        if w.len() == 1
            && w.chars().next().is_some_and(char::is_lowercase)
            && !matches!(*w, "a" | "i")
        {
            let next = words.get(i + 1).copied().unwrap_or("");
            if !is_symbol_companion(next) {
                return true;
            }
        }
    }
    words
        .windows(2)
        .any(|pair| split_fragment_pair(pair[0], pair[1]) || split_fragment_pair(pair[1], pair[0]))
}

/// True when `short` (2–3 letters, alphabetic, not a stopword) sits next to
/// `long` (≥5 letters, alphabetic, starting lowercase) — the shape of an
/// intra-word space artifact. Requiring a lowercase start avoids flagging
/// proper nouns ("the Smith").
fn split_fragment_pair(short: &str, long: &str) -> bool {
    if !(2..=3).contains(&short.len()) || long.len() < 5 {
        return false;
    }
    if !short.chars().all(char::is_alphabetic) || !long.chars().all(char::is_alphabetic) {
        return false;
    }
    if !long.starts_with(char::is_lowercase) {
        return false;
    }
    !SHORT_STOPWORDS.contains(&short.to_lowercase())
}

/// Repair intra-word space artifacts in the chunks at `idxs` via batched LLM
/// calls. Chunks are grouped into character-bounded batches; each batch is sent
/// with `<<<N>>>` markers (N = position within the batch) and corrections are
/// applied through [`apply_artifact_corrections`]. Failures are logged and
/// leave chunks unchanged. No-op when `idxs` is empty.
pub async fn fix_body_artifacts(client: &LlmClient, chunks: &mut [Chunk], idxs: &[usize]) {
    if idxs.is_empty() {
        return;
    }
    let mut applied_total = 0usize;
    for batch in budgeted_batches(idxs, chunks) {
        let prompt = build_prompt(&batch, chunks);
        let response = match client
            .generate(SYSTEM_PROMPT, &prompt, MAX_TOKENS, 0.0)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Body artifact repair LLM call failed: {e}");
                continue;
            }
        };
        applied_total += apply_artifact_corrections(chunks, &batch, &response);
    }
    if applied_total > 0 {
        tracing::info!("Repaired PDF text artifacts in {applied_total} chunk(s)");
    }
}

/// [`batch_indices`] capped at [`MAX_REPAIR_BATCHES`] — the token-budget guard
/// for [`fix_body_artifacts`]. Excess batches are dropped with a warning.
fn budgeted_batches(idxs: &[usize], chunks: &[Chunk]) -> Vec<Vec<usize>> {
    let mut batches = batch_indices(idxs, chunks);
    if batches.len() > MAX_REPAIR_BATCHES {
        tracing::warn!(
            total_batches = batches.len(),
            kept = MAX_REPAIR_BATCHES,
            "Body artifact repair budget exceeded; skipping {} batch(es) to bound LLM token cost",
            batches.len() - MAX_REPAIR_BATCHES
        );
        batches.truncate(MAX_REPAIR_BATCHES);
    }
    batches
}

/// Group `idxs` into batches whose summed chunk content stays within
/// [`BATCH_CHAR_BUDGET`]. A single chunk larger than the budget is sent alone.
fn batch_indices(idxs: &[usize], chunks: &[Chunk]) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_chars = 0usize;
    for &i in idxs {
        let chars = chunks[i].content.chars().count();
        if !current.is_empty() && current_chars + chars > BATCH_CHAR_BUDGET {
            batches.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(i);
        current_chars += chars;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// Build the user prompt for one batch: an instruction block followed by each
/// chunk prefixed with its `<<<N>>>` marker.
fn build_prompt(batch: &[usize], chunks: &[Chunk]) -> String {
    let mut prompt = String::from(
        "Below are paragraphs from an academic paper, each preceded by a <<<N>>> marker. Some \
         words may have been split by stray spaces from PDF extraction (e.g. \"framew ork\" \
         should be \"framework\", \"lev erages\" should be \"leverages\"). Output every \
         paragraph again in the same <<<N>>> order, each preceded by its marker, with broken \
         words rejoined. Merge fragments only when they are not standalone words in context — \
         leave genuine phrases (\"in vivo\", \"de novo\", \"B cell\") and all punctuation, \
         citations and formatting untouched. Never rephrase, translate, or fix grammar; echo \
         correct paragraphs unchanged.\n\n",
    );
    for (n, &gi) in batch.iter().enumerate() {
        prompt.push_str(&format!("<<<{n}>>>\n{}\n", chunks[gi].content));
    }
    prompt
}

/// Parse a `<<<N>>>`-delimited LLM `response` and apply each correction to
/// `chunks[batch[n]]`. A correction is accepted only when it preserves the
/// original's non-whitespace character sequence (a pure word merge/split);
/// anything else is a hallucinated rewrite and is rejected. Returns the number
/// of chunks actually modified.
fn apply_artifact_corrections(chunks: &mut [Chunk], batch: &[usize], response: &str) -> usize {
    let stripped = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let mut applied = 0usize;

    // Collect (index, content_start) for each <<<N>>> marker in the response.
    let markers: Vec<(usize, usize)> = MARKER_RE
        .captures_iter(response)
        .filter_map(|cap| {
            let n = cap[1].parse::<usize>().ok()?;
            Some((n, cap.get(0)?.end()))
        })
        .collect();

    for (pos, &(n, start)) in markers.iter().enumerate() {
        // Content runs from after this marker to just before the next "<<<".
        let end = markers
            .get(pos + 1)
            .map_or(response.len(), |&(_, next_start)| {
                response[..next_start].rfind("<<<").unwrap_or(next_start)
            });
        let corrected = response[start..end].trim();
        let Some(&gi) = batch.get(n) else {
            continue;
        };
        let Some(chunk) = chunks.get_mut(gi) else {
            continue;
        };
        let original = chunk.content.clone();
        if !corrected.is_empty()
            && corrected != original
            && stripped(corrected) == stripped(&original)
        {
            chunk.content = corrected.to_string();
            applied += 1;
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunker::ChunkType;

    fn para(content: &str) -> Chunk {
        Chunk::new("p1", "p1_p1", ChunkType::Paragraph, content)
    }

    #[test]
    fn detects_intra_word_space_artifacts() {
        assert!(chunk_likely_has_artifacts(
            "DREAM framew ork leverages regulatory lexicon"
        ));
        assert!(chunk_likely_has_artifacts("the lev erages of the model"));
        // A lone single letter (not a/i) is a split fragment.
        assert!(chunk_likely_has_artifacts("binding x domain interactions"));
    }

    #[test]
    fn ignores_legitimate_short_word_pairs() {
        assert!(!chunk_likely_has_artifacts(
            "the protein structure was resolved"
        ));
        assert!(!chunk_likely_has_artifacts("studied in vivo and de novo"));
        assert!(!chunk_likely_has_artifacts(
            "Enhancers play a critical role in regulating gene expression across species."
        ));
    }

    #[test]
    fn applies_only_pure_whitespace_corrections() {
        let mut chunks = vec![para("DREAM framew ork leverages lexicon")];
        let response = "<<<0>>>\nDREAM framework leverages lexicon\n";
        let applied = apply_artifact_corrections(&mut chunks, &[0], response);
        assert_eq!(applied, 1);
        assert_eq!(chunks[0].content, "DREAM framework leverages lexicon");
    }

    #[test]
    fn rejects_semantic_rewrite() {
        let mut chunks = vec![para("the framew ork works")];
        // The model rewrote wording (not just whitespace) — must be rejected.
        let response = "<<<0>>>\nthe framework functions well\n";
        let applied = apply_artifact_corrections(&mut chunks, &[0], response);
        assert_eq!(applied, 0);
        assert_eq!(chunks[0].content, "the framew ork works");
    }

    #[test]
    fn rejects_out_of_range_marker() {
        let mut chunks = vec![para("some text")];
        let response = "<<<5>>>\nsome text\n";
        let applied = apply_artifact_corrections(&mut chunks, &[0], response);
        assert_eq!(applied, 0);
    }

    #[test]
    fn parses_multiple_sections() {
        let mut chunks = vec![para("framew ork one"), para("lev erages two")];
        let response = "<<<0>>>\nframework one\n<<<1>>>\nleverages two\n";
        let applied = apply_artifact_corrections(&mut chunks, &[0, 1], response);
        assert_eq!(applied, 2);
        assert_eq!(chunks[0].content, "framework one");
        assert_eq!(chunks[1].content, "leverages two");
    }

    #[test]
    fn ignores_legitimate_single_letter_symbols() {
        // Statistical/scientific single-letter symbols with their companions
        // are not split fragments.
        assert!(!chunk_likely_has_artifacts("the p value was significant"));
        assert!(!chunk_likely_has_artifacts("with n = 30 samples per group"));
        assert!(!chunk_likely_has_artifacts("the x axis shows time"));
        assert!(!chunk_likely_has_artifacts("p values, were combined"));
        // Uppercase symbols were never flagged by the lowercase-only rule.
        assert!(!chunk_likely_has_artifacts("an X ray image of the lattice"));
    }

    #[test]
    fn ignores_latin_particles() {
        // "de" joins the stopword list — "de facto"/"de novo" must not be
        // treated as split fragments.
        assert!(!chunk_likely_has_artifacts("the de facto standard method"));
        assert!(!chunk_likely_has_artifacts("studied in vivo and de novo"));
    }

    #[test]
    fn batches_are_capped_by_repair_budget() {
        // 30 chunks of 1000 chars → 15 batches of 2 at the 2500-char budget,
        // truncated to MAX_REPAIR_BATCHES.
        let chunks: Vec<Chunk> = (0..30).map(|_| para(&"x".repeat(1000))).collect();
        let idxs: Vec<usize> = (0..30).collect();
        let batches = budgeted_batches(&idxs, &chunks);
        assert_eq!(batches.len(), MAX_REPAIR_BATCHES);
        // The cap is a pure prefix truncation of the uncapped plan.
        assert_eq!(
            batches,
            batch_indices(&idxs, &chunks)[..MAX_REPAIR_BATCHES].to_vec()
        );
    }

    #[test]
    fn batches_under_budget_are_untouched() {
        let chunks: Vec<Chunk> = (0..5).map(|_| para(&"x".repeat(1000))).collect();
        let idxs: Vec<usize> = (0..5).collect();
        assert_eq!(
            budgeted_batches(&idxs, &chunks),
            batch_indices(&idxs, &chunks)
        );
    }

    #[test]
    fn batches_respect_char_budget() {
        let chunks: Vec<Chunk> = (0..5).map(|_| para(&"x".repeat(1000))).collect();
        let idxs: Vec<usize> = (0..5).collect();
        let batches = batch_indices(&idxs, &chunks);
        // 2500 budget with 1000-char chunks → [2, 2, 1].
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 2, 1]
        );
    }
}
