pub mod file_limits;
pub mod fs;
pub mod image;
pub mod macos;
pub mod paths;
pub mod process;
pub mod str_enum;

use std::sync::LazyLock;

/// Regex for markdown image syntax: `![caption](path)`.
pub static MARKDOWN_IMAGE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"!\[(.*?)\]\((.*?)\)").expect("valid markdown image regex")
});

/// A publisher download-watermark occupying its own line, e.g.
/// `Downloaded from [https://…](https://…) by Some University user on 12 December 2024`.
/// The trailing newline is consumed too, so removing the line never welds the
/// surrounding lines into a blank line that would fake a paragraph break.
/// These are acquisition artifacts, not paper content.
static WATERMARK_LINE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?im)^[ \t]*Downloaded from\b[^\n]*?\buser on\s+\d{1,2}\s+\w+\s+\d{4}\b[^\n]*\r?\n?",
    )
    .expect("valid watermark line regex")
});

/// A download-watermark injected mid-sentence by the PDF extractor (it splits
/// the surrounding sentence and is followed by a paragraph break). Removing it
/// and the break it introduces rejoins the sentence.
static WATERMARK_INLINE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)[ \t]+Downloaded from\b[^\n]*?\buser on\s+\d{1,2}\s+\w+\s+\d{4}\b[ \t]*(?:\r?\n[ \t]*){0,2}",
    )
    .expect("valid watermark inline regex")
});

/// Runs of three or more newlines, collapsed to a single paragraph break.
static EXCESS_NEWLINES_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\n{3,}").expect("valid newline regex"));

/// Runs of two or more spaces, collapsed to one.
static EXCESS_SPACES_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r" {2,}").expect("valid space regex"));

/// A PDF line-break hyphenation artifact: a short lowercase prefix broken off
/// with "- " or "-\n" from a longer lowercase suffix ("es- tablishing" →
/// "establishing", "es-\ntablishing" → "establishing", "pri- marily" →
/// "primarily"). The whitespace after the hyphen is the artifact signal —
/// genuine hyphenated words carry none ("state-of-the-art"). Only short
/// prefixes (≤3 letters) are merged, which leaves deliberate hyphenations with
/// a longer first element ("well- being") and capitalised terms ("T- cell")
/// untouched. Deterministic and free, so it runs ahead of any LLM repair.
static HYPHEN_BREAK_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\b([a-z]{1,3})-[ \n]([a-z]{3,})\b").expect("valid hyphen-break regex")
});

/// Strip publisher download-watermarks from extracted source text. Handles
/// both a watermark on its own line and one injected mid-sentence (rejoining
/// the split sentence), then tidies the blank/space runs left behind. Returns
/// a borrowed reference when there is nothing to clean. The watermark is an
/// acquisition artifact (not paper content): it wastes RAG context budget and
/// clutters the verbatim passages shown to the user.
///
/// Cleanup is deliberately conservative: removing a watermark line never
/// introduces a new blank line inside a paragraph, and double-space collapsing
/// is applied only to the line(s) an inline watermark was removed from —
/// legitimate alignment (e.g. table spacing) elsewhere in the chunk survives.
#[must_use]
pub fn clean_verbatim_text(text: &str) -> std::borrow::Cow<'_, str> {
    if !WATERMARK_LINE_RE.is_match(text) && !WATERMARK_INLINE_RE.is_match(text) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = WATERMARK_LINE_RE.replace_all(text, "").into_owned();
    // Replace inline watermarks with a sentinel so that only the affected
    // line(s) get their double spaces collapsed below; the sentinel itself
    // then becomes the single rejoining space.
    const SENTINEL: char = '\u{E000}'; // Unicode private-use area
    out = WATERMARK_INLINE_RE
        .replace_all(&out, SENTINEL.to_string())
        .into_owned();
    if out.contains(SENTINEL) {
        out = out
            .split('\n')
            .map(|line| {
                if line.contains(SENTINEL) {
                    EXCESS_SPACES_RE
                        .replace_all(line, " ")
                        .replace(SENTINEL, " ")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    // Blank-line runs left where a watermark line sat at a paragraph boundary
    // collapse back to a single paragraph break.
    out = EXCESS_NEWLINES_RE.replace_all(&out, "\n\n").into_owned();
    std::borrow::Cow::Owned(out)
}

/// Rejoin PDF line-break hyphenation artifacts ("es- tablishing" →
/// "establishing"). See [`HYPHEN_BREAK_RE`] for the precise (conservative)
/// pattern. Returns a borrowed reference when there is nothing to fix.
#[must_use]
pub fn fix_pdf_hyphenation(text: &str) -> std::borrow::Cow<'_, str> {
    if !HYPHEN_BREAK_RE.is_match(text) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(HYPHEN_BREAK_RE.replace_all(text, "$1$2").into_owned())
}

/// Build a JSON `extra` metadata string from key-value pairs.
/// Optional values are only included if `Some`.
pub fn build_extra_json(
    required: &[(&str, String)],
    optional: &[(&str, Option<String>)],
) -> String {
    let mut map = serde_json::Map::new();
    for (key, value) in required {
        map.insert(key.to_string(), serde_json::Value::String(value.clone()));
    }
    for (key, value) in optional {
        if let Some(v) = value {
            map.insert(key.to_string(), serde_json::Value::String(v.clone()));
        }
    }
    serde_json::Value::Object(map).to_string()
}

/// Extract a string `key` from a JSON object stored as text (the inverse of
/// [`build_extra_json`] for single-key reads, e.g. `zotero_key` / `lattice_id`
/// in a paper's `extra` field). Returns `None` on malformed JSON or wrong type.
#[must_use]
pub fn parse_extra_key(extra: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(extra).ok()?;
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// SHA-256 of `bytes`, lowercase hex-encoded.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// A single indexing job sent to the background worker.
#[derive(Debug, Clone)]
pub struct IndexJob {
    pub paper_id: String,
    pub file_path: String,
    pub is_reindex: bool,
    pub retry_count: u32,
    /// When true, only re-extract sections and embed (skip PDF parsing).
    pub sections_only: bool,
    /// When true, only re-embed existing sections (no extraction, no LLM, no PDF needed).
    pub reembed_only: bool,
}

impl IndexJob {
    /// Fresh job: no retries, no section-only or re-embed flags.
    pub fn new(paper_id: String, file_path: String) -> Self {
        Self {
            paper_id,
            file_path,
            is_reindex: false,
            retry_count: 0,
            sections_only: false,
            reembed_only: false,
        }
    }
}

/// Estimate token count from text with language awareness.
///
/// Uses a hybrid heuristic:
/// - CJK characters (Chinese, Japanese, Korean): ~1 char per token
/// - Latin/script characters: ~4 chars per token (0.25 tokens/char)
/// - Whitespace and punctuation: counted but grouped with adjacent text
///
/// This is more accurate than a flat byte/char count, especially for
/// mixed-language academic papers.
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk_count = 0usize;
    let mut other_count = 0usize;

    for ch in text.chars() {
        if is_cjk(ch) {
            cjk_count += 1;
        } else if !ch.is_whitespace() {
            other_count += 1;
        }
    }

    cjk_count + other_count.div_ceil(4)
}

/// Check if a character is CJK (Chinese, Japanese, Korean).
pub fn is_cjk(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
        || ('\u{3400}'..='\u{4dbf}').contains(&ch)
        || ('\u{20000}'..='\u{2a6df}').contains(&ch)
        || ('\u{2a700}'..='\u{2b73f}').contains(&ch)
        || ('\u{2b740}'..='\u{2b81f}').contains(&ch)
        || ('\u{3040}'..='\u{309f}').contains(&ch)
        || ('\u{30a0}'..='\u{30ff}').contains(&ch)
        || ('\u{ac00}'..='\u{d7af}').contains(&ch)
        || ('\u{ff00}'..='\u{ffef}').contains(&ch)
}

/// Truncate a string to at most `max_chars` characters, safely handling UTF-8.
/// Returns a borrowed reference when no truncation is needed.
/// If truncation is needed, appends "..." such that the total length does not exceed `max_chars`.
/// For `max_chars` ≤ 3, no ellipsis is appended.
pub fn truncate_chars(s: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    let count = s.chars().count();
    if count <= max_chars {
        std::borrow::Cow::Borrowed(s)
    } else if max_chars <= 3 {
        std::borrow::Cow::Owned(s.chars().take(max_chars).collect())
    } else {
        std::borrow::Cow::Owned(s.chars().take(max_chars - 3).collect::<String>() + "...")
    }
}

/// Synchronously search configured paths for a PDF matching the provided search terms.
pub fn find_pdf_sync(
    search_paths: &[std::path::PathBuf],
    search_terms: &[String],
) -> Option<String> {
    for dir in search_paths {
        if let Some(path) = scan_dir_for_pdf(dir, search_terms) {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    None
}

/// Build PDF filename search terms for a library item: the title truncated to
/// `title_chars` and lowercased with underscores, the lowercased item id, and
/// the DOI with `/` replaced by `_`. Shared by the Zotero and Lattice sources.
pub fn pdf_search_terms(
    title: &str,
    title_chars: usize,
    id: &str,
    doi: Option<&str>,
) -> Vec<String> {
    let mut terms = Vec::new();
    let short_title: String = title.chars().take(title_chars).collect();
    terms.push(short_title.to_lowercase().replace(' ', "_"));
    terms.push(id.to_lowercase());
    if let Some(doi) = doi {
        terms.push(doi.replace('/', "_"));
    }
    terms
}

/// Map a file extension to its MIME type.
/// Returns a default of `"image/png"` for unknown extensions.
pub fn mime_from_ext(ext: &str) -> &'static str {
    match ext.len() {
        3 => match ext.as_bytes() {
            b"jpg" | b"JPG" => "image/jpeg",
            b"gif" | b"GIF" => "image/gif",
            b"bmp" | b"BMP" => "image/bmp",
            _ => "image/png",
        },
        4 => match ext.as_bytes() {
            b"jpeg" | b"JPEG" | b"webp" | b"WEBP" => {
                if ext.eq_ignore_ascii_case("jpeg") {
                    "image/jpeg"
                } else {
                    "image/webp"
                }
            }
            _ => "image/png",
        },
        _ => "image/png",
    }
}

/// Recursively scan a directory for a PDF whose filename contains any of the search terms.
fn scan_dir_for_pdf(dir: &std::path::Path, search_terms: &[String]) -> Option<std::path::PathBuf> {
    if !dir.exists() {
        return None;
    }
    let lower_terms: Vec<String> = search_terms.iter().map(|t| t.to_lowercase()).collect();

    let walker = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .max_depth(6)
        .into_iter()
        .filter_map(std::result::Result::ok);

    for entry in walker {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();

        if lower_terms
            .iter()
            .any(|term| filename.contains(term.as_str()))
        {
            return Some(path.to_path_buf());
        }
    }

    None
}

/// Resolve `path` relative to `base_dir` and verify the canonicalized result is
/// still contained within `base_dir`. Returns `None` if the path resolves
/// outside `base_dir` or cannot be canonicalized.
///
/// This is the canonical helper for defending against archive path-traversal
/// attacks and similar "../../../" injections.
pub fn resolve_within(
    base_dir: &std::path::Path,
    path: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let base = std::fs::canonicalize(base_dir).unwrap_or_else(|_| base_dir.to_path_buf());

    // If the path already exists, canonicalize it directly.
    let candidate = if path.exists() {
        std::fs::canonicalize(path).ok()?
    } else {
        // Otherwise canonicalize the parent and append the file name.
        let parent = path.parent().unwrap_or(std::path::Path::new(""));
        let file_name = path.file_name()?;
        let canonical_parent = std::fs::canonicalize(parent).ok()?;
        canonical_parent.join(file_name)
    };

    candidate.starts_with(&base).then_some(candidate)
}

/// Deduplicate strings in place: trim, filter out empty/"null", dedup
/// case-insensitively. Also trims remaining values.
pub fn dedup_strings_in_place(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|s| {
        let cleaned = s.trim();
        !cleaned.is_empty() && cleaned != "null" && {
            let lower = cleaned.to_lowercase();
            seen.insert(lower)
        }
    });
    for s in items.iter_mut() {
        *s = s.trim().to_string();
    }
}

/// Deduplicate a vector of strings, preserving order. When
/// `case_insensitive` is true, the first-seen spelling is kept;
/// otherwise exact string comparison is used.
pub fn dedup_strings(items: Vec<String>, case_insensitive: bool) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|s| {
            let key: String = if case_insensitive {
                s.to_lowercase()
            } else {
                s.clone()
            };
            seen.insert(key)
        })
        .collect()
}

/// Filter a string reference: trim, reject empty or literal `"null"`.
/// Returns `Some(trimmed)` or `None`.
pub fn filter_non_empty_string(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty() && trimmed != "null").then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_cjk() {
        assert_eq!(estimate_tokens("你好世界"), 4);
        assert_eq!(estimate_tokens("这是一个测试"), 6);
    }

    #[test]
    fn test_estimate_tokens_latin() {
        assert_eq!(estimate_tokens("hello world"), 3);
        assert_eq!(estimate_tokens("test"), 1);
    }

    #[test]
    fn test_estimate_tokens_mixed() {
        assert_eq!(estimate_tokens("hello你好"), 4);
    }

    #[test]
    fn test_is_cjk() {
        assert!(is_cjk('中'));
        assert!(is_cjk('あ'));
        assert!(is_cjk('한'));
        assert!(!is_cjk('a'));
        assert!(!is_cjk('1'));
    }

    #[test]
    fn test_truncate_chars_noop() {
        let s = "short";
        assert!(matches!(
            truncate_chars(s, 10),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(truncate_chars(s, 10), "short");
    }

    #[test]
    fn test_truncate_chars_truncates() {
        let s = "this is a longer string";
        assert_eq!(truncate_chars(s, 5), "th...");
    }

    #[test]
    fn test_truncate_chars_utf8() {
        let s = "你好世界这是一个测试";
        assert_eq!(truncate_chars(s, 4), "你...");
    }

    #[test]
    fn clean_verbatim_strips_download_watermark() {
        let text = "Real findings about enhancers.\n\n\
                    Downloaded from [https://academic.oup.com/nar/article](https://academic.oup.com/nar/article) \
                    by Northwest Agriculture & Forest University user on 12 December 2024\n\n\
                    More real content.";
        let cleaned = clean_verbatim_text(text);
        assert!(
            !cleaned.contains("Downloaded from"),
            "watermark must be removed"
        );
        assert!(!cleaned.contains("user on"), "watermark must be removed");
        assert!(cleaned.contains("Real findings about enhancers."));
        assert!(cleaned.contains("More real content."));
        // The removed line must not leave a large blank gap.
        assert!(!cleaned.contains("\n\n\n"), "blank run must be collapsed");
    }

    #[test]
    fn clean_verbatim_borrows_when_clean() {
        let text = "No watermark here, just plain text.";
        assert!(matches!(
            clean_verbatim_text(text),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn clean_verbatim_strips_inline_watermark_and_rejoins_sentence() {
        // The PDF extractor can inject the watermark mid-sentence, splitting
        // it and introducing a paragraph break. Cleaning must rejoin it.
        let text = "This paper introduces an interpretable \
                    Downloaded from [https://academic.oup.com/nar/article](https://academic.oup.com/nar/article) \
                    by Northwest Agriculture & Forest University user on 12 December 2024\n\n\
                    approach for predicting enhancer activity.";
        let cleaned = clean_verbatim_text(text);
        assert!(
            !cleaned.contains("Downloaded from"),
            "inline watermark must be removed"
        );
        assert!(
            cleaned.contains("introduces an interpretable approach for predicting"),
            "split sentence must be rejoined, got: {cleaned}"
        );
    }

    #[test]
    fn fix_pdf_hyphenation_merges_newline_breaks() {
        // Same artifact with the line break preserved ("-\n" instead of "- ").
        assert_eq!(
            fix_pdf_hyphenation("the es-\ntablishing of cell identity"),
            "the establishing of cell identity"
        );
        assert_eq!(
            fix_pdf_hyphenation("gene expression is pri-\nmarily regulated"),
            "gene expression is primarily regulated"
        );
    }

    #[test]
    fn clean_verbatim_line_watermark_does_not_split_paragraph() {
        // A watermark line removed from inside a paragraph must not leave a
        // blank line that fakes a paragraph break.
        let text = "First part of the sentence\n\
                    Downloaded from [https://academic.oup.com/nar/article](https://academic.oup.com/nar/article) \
                    by Some University user on 12 December 2024\n\
                    continues on the next line.";
        let cleaned = clean_verbatim_text(text);
        assert_eq!(
            cleaned,
            "First part of the sentence\ncontinues on the next line."
        );
    }

    #[test]
    fn clean_verbatim_preserves_double_spaces_outside_watermark_lines() {
        // Legitimate alignment elsewhere in the chunk must survive cleaning —
        // only lines an inline watermark was removed from get space-collapsed.
        let text = "col1  col2  col3\n\n\
                    Downloaded from [https://academic.oup.com/nar/article](https://academic.oup.com/nar/article) \
                    by Some University user on 12 December 2024\n\n\
                    more text";
        let cleaned = clean_verbatim_text(text);
        assert!(
            cleaned.contains("col1  col2  col3"),
            "unaligned double spaces must be preserved, got: {cleaned}"
        );
        assert!(!cleaned.contains("Downloaded from"));
    }

    #[test]
    fn clean_verbatim_preserves_prose_mentioning_download() {
        // A sentence that merely contains the words is not a watermark line.
        let text = "The data was downloaded from the repository for analysis.";
        assert_eq!(clean_verbatim_text(text), text);
    }

    #[test]
    fn fix_pdf_hyphenation_merges_short_prefix_breaks() {
        assert_eq!(
            fix_pdf_hyphenation("gene expression is pri- marily regulated"),
            "gene expression is primarily regulated"
        );
        assert_eq!(
            fix_pdf_hyphenation("the es- tablishing of cell identity"),
            "the establishing of cell identity"
        );
    }

    #[test]
    fn fix_pdf_hyphenation_preserves_genuine_hyphens() {
        // No space after the hyphen → not an artifact.
        assert_eq!(
            fix_pdf_hyphenation("a state-of-the-art model"),
            "a state-of-the-art model"
        );
        // Long first element (>3 letters) → left untouched.
        assert_eq!(
            fix_pdf_hyphenation("a well- known result"),
            "a well- known result"
        );
        // Capitalised term → left untouched.
        assert_eq!(
            fix_pdf_hyphenation("human T- cell assay"),
            "human T- cell assay"
        );
    }

    #[test]
    fn fix_pdf_hyphenation_borrows_when_clean() {
        let text = "no hyphenation artifacts here";
        assert!(matches!(
            fix_pdf_hyphenation(text),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn filter_non_empty_string_rejects_empty() {
        assert_eq!(filter_non_empty_string(""), None);
        assert_eq!(filter_non_empty_string("  "), None);
    }

    #[test]
    fn filter_non_empty_string_rejects_literal_null() {
        assert_eq!(filter_non_empty_string("null"), None);
        assert_eq!(filter_non_empty_string(" null "), None);
    }

    #[test]
    fn filter_non_empty_string_accepts_valid() {
        assert_eq!(filter_non_empty_string("hello"), Some("hello".to_string()));
        assert_eq!(
            filter_non_empty_string("  hello world  "),
            Some("hello world".to_string())
        );
    }
}
