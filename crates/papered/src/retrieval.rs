//! Parent document retrieval — fetch chunk context including ancestors and paper metadata.
//!
//! Two-tier architecture:
//!   1. Section vectors identify relevant papers (semantic, coarse)
//!   2. Full-text search on chunks finds exact paragraphs within those papers (lexical, fine)
//!   3. Ancestor chains provide heading context for each matched chunk.

use crate::chunker::Chunk;
use crate::error::Result;
use crate::index::multimodal::FigureInfo;
use crate::paper::Paper;
use crate::store::vector::{ChunkHit, VectorStore};
use crate::util::str_enum::StrLabel;
use indexmap::IndexMap;
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// Minimum remaining characters required to include a truncated source.
const RAG_MIN_SOURCE_CHARS: usize = 50;

use crate::paper::{ALL_META_FIELDS, PUBLIC_META_FIELDS};

/// Max figure objects inlined into a single chunk's context.
const MAX_REFERENCED_OBJECTS: usize = 3;
const REFERENCED_CAPTION_CHARS: usize = 300;
const REFERENCED_DESC_CHARS: usize = 400;
/// Characters scanned after a "Figure" keyword to collect numbers
/// (covers "Figures 1, 2 and 3" and "Fig. 1-3").
const REF_NUMBER_WINDOW: usize = 40;

static FIG_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bfig(?:ure)?s?\.?\s").expect("fig ref regex"));
static NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").expect("number regex"));
static RANGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s*[-\u{2013}]\s*(\d+)").expect("range regex"));

/// Something that can be assembled into a source-level RAG context entry.
pub trait RagContextSource {
    /// The paper whose metadata should be used for the source header.
    fn paper(&self) -> &Paper;
    /// The textual content associated with this source.
    fn content(&self) -> &str;
}

/// Collect the 1-based numbers referenced after `kw_re` keyword matches in `text`
/// (e.g. "Figures 1, 2 and 3" → `[1, 2, 3]`; "Fig. 1-3" → `[1, 2, 3]`).
/// Numbers are returned in first-seen order, deduplicated, capped at 8.
fn extract_referenced_numbers(text: &str, kw_re: &Regex) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for m in kw_re.find_iter(text) {
        let window: String = text[m.end()..].chars().take(REF_NUMBER_WINDOW).collect();
        for cap in RANGE_RE.captures_iter(&window) {
            let (Some(a), Some(b)) = (cap.get(1), cap.get(2)) else {
                continue;
            };
            let (Ok(a), Ok(b)) = (a.as_str().parse::<u32>(), b.as_str().parse::<u32>()) else {
                continue;
            };
            if a <= b && b - a <= 10 {
                for n in a..=b {
                    if seen.insert(n) {
                        out.push(n);
                    }
                }
            }
        }
        for dm in NUMBER_RE.find_iter(&window) {
            if let Ok(n) = dm.as_str().parse::<u32>()
                && seen.insert(n)
            {
                out.push(n);
            }
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

/// Build an inline block appending the caption/description of any figures the
/// chunk explicitly references by number. Returns `None` when the chunk
/// references none or none resolve to usable metadata.
fn build_referenced_context(chunk_content: &str, figures: &[FigureInfo]) -> Option<String> {
    let fig_nums = extract_referenced_numbers(chunk_content, &FIG_REF_RE);
    if fig_nums.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    let mut used = 0usize;

    for n in &fig_nums {
        if used >= MAX_REFERENCED_OBJECTS {
            break;
        }
        let Some(fig) = figures.get((*n as usize).saturating_sub(1)) else {
            continue;
        };
        let entry = match (&fig.caption, &fig.description) {
            (None, None) => continue,
            (Some(c), Some(d)) => format!(
                "[Referenced Figure {n}: {} \u{2014} {}]",
                crate::util::truncate_chars(c, REFERENCED_CAPTION_CHARS),
                crate::util::truncate_chars(d, REFERENCED_DESC_CHARS)
            ),
            (Some(c), None) | (None, Some(c)) => {
                let limit = if fig.caption.is_some() {
                    REFERENCED_CAPTION_CHARS
                } else {
                    REFERENCED_DESC_CHARS
                };
                format!(
                    "[Referenced Figure {n}: {}]",
                    crate::util::truncate_chars(c, limit)
                )
            }
        };
        parts.push(entry);
        used += 1;
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Format extracted sections as `[Type] snippet` blocks joined by blank lines —
/// the fallback RAG context shape used when chunk-level context is unavailable.
#[must_use]
pub fn sections_to_context(
    sections: &crate::paper::section::PaperSections,
    snippet_chars: usize,
) -> String {
    sections
        .sections
        .iter()
        .map(|s| {
            let snippet = crate::util::truncate_chars(&s.content, snippet_chars);
            format!("[{}] {}", s.section_type.as_str(), snippet)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Recover a chunk's heading path by walking `parent_id` links through the
/// given candidates (typically the chunk plus its ancestor chain from
/// [`crate::store::vector::VectorStore::get_chunk_ancestors`]). Returns the
/// chain root-first joined by " > ", or `None` when no ancestors are present.
pub fn heading_path_from_ancestors(chunk: &Chunk, chunks: &[Chunk]) -> Option<String> {
    let mut by_id = HashMap::with_capacity(chunks.len());
    for c in chunks {
        by_id.insert(c.id.as_str(), c);
    }
    heading_path_from_map(chunk, &by_id)
}

/// Recover a heading path using a pre-built `HashMap` of chunk id → chunk.
///
/// Use this variant when walking many chunks against the same ancestor set to
/// avoid rebuilding the lookup map for each chunk.
pub fn heading_path_from_map(chunk: &Chunk, by_id: &HashMap<&str, &Chunk>) -> Option<String> {
    let mut current_id = chunk.parent_id.as_deref();
    let mut chain = std::collections::VecDeque::new();
    while let Some(pid) = current_id {
        let Some(parent) = by_id.get(pid) else { break };
        chain.push_front(parent.content.as_str());
        current_id = parent.parent_id.as_deref();
    }
    if chain.is_empty() {
        None
    } else {
        let joined: String = chain.into_iter().collect::<Vec<_>>().join(" > ");
        Some(joined)
    }
}

/// Fetch a chunk together with its root-first heading path.
///
/// Shared by the REST and MCP "get passage" surfaces: validates the paper ID,
/// returns `Ok(None)` when the chunk does not exist, and tolerates missing
/// ancestor data (the heading path simply becomes `None`).
pub async fn chunk_with_heading_path(
    store: &dyn VectorStore,
    paper_id: &str,
    chunk_id: &str,
) -> Result<Option<(Chunk, Option<String>)>> {
    if !crate::util::paths::is_safe_paper_id(paper_id) {
        return Err(crate::PaperedError::invalid_argument(format!(
            "Invalid paper ID: {paper_id}"
        )));
    }
    let Some(chunk) = store.get_chunk(paper_id, chunk_id).await? else {
        return Ok(None);
    };
    let ancestors = store
        .get_chunk_ancestors(paper_id, &[chunk_id])
        .await
        .unwrap_or_default();
    let heading_path = heading_path_from_ancestors(&chunk, &ancestors);
    Ok(Some((chunk, heading_path)))
}

/// Assemble rich context from chunk hits by walking ancestor chains.
///
/// Groups hits by paper to avoid N+1 queries: each paper and its chunk tree
/// is loaded exactly once, regardless of how many hits belong to it.
pub async fn assemble_chunk_context(
    store: &dyn VectorStore,
    chunk_hits: &[ChunkHit],
) -> Result<Vec<ChunkContext>> {
    if chunk_hits.is_empty() {
        return Ok(Vec::new());
    }

    // Group hits by paper_id to batch-load papers and chunks
    let mut hits_by_paper: HashMap<String, Vec<&ChunkHit>> = HashMap::new();
    for hit in chunk_hits {
        hits_by_paper
            .entry(hit.chunk.paper_id.clone())
            .or_default()
            .push(hit);
    }

    let mut contexts = Vec::with_capacity(chunk_hits.len());

    let paper_ids: Vec<&str> = hits_by_paper.keys().map(|s| s.as_str()).collect();
    let papers = store.get_papers_by_ids(&paper_ids).await?;
    let paper_lookup: HashMap<String, Paper> =
        papers.into_iter().map(|p| (p.id.clone(), p)).collect();

    for (paper_id, hits) in hits_by_paper {
        let paper = match paper_lookup.get(&paper_id) {
            Some(p) => Arc::new(p.clone()),
            None => {
                tracing::warn!("Paper {} not found for chunk hits", paper_id);
                continue;
            }
        };

        // Load only the ancestor chains for the matched chunks instead of the
        // entire chunk tree.
        let chunk_ids: Vec<&str> = hits.iter().map(|h| h.chunk.id.as_str()).collect();
        let ancestor_chunks = store.get_chunk_ancestors(&paper_id, &chunk_ids).await?;

        // Build the ancestor lookup map once per paper to avoid O(|ancestors|)
        // HashMap construction for each hit.
        let mut ancestor_map = HashMap::with_capacity(ancestor_chunks.len());
        for c in &ancestor_chunks {
            ancestor_map.insert(c.id.as_str(), c);
        }

        // Best-effort enrichment: load figures once per paper so chunks that
        // cite "Figure N" can inline the referenced object.
        let figures = store.get_figures(&paper_id).await.unwrap_or_default();

        for hit in hits {
            let heading_path = heading_path_from_map(&hit.chunk, &ancestor_map);
            let referenced_context = build_referenced_context(&hit.chunk.content, &figures);

            contexts.push(ChunkContext {
                paper: Arc::clone(&paper),
                // Strip publisher download-watermarks so they neither waste the
                // context budget nor leak into the model's view of the paper.
                chunk_content: crate::util::clean_verbatim_text(&hit.chunk.content).into_owned(),
                score: hit.score,
                page_number: hit.chunk.page_number,
                chunk_id: hit.chunk.id.clone(),
                heading_path,
                referenced_context,
            });
        }
    }

    // Sort by relevance: higher score = better match (matches the
    // higher-is-better `fts_score` returned by `search_chunks`).
    contexts.sort_by(cmp_chunk_context_score_desc);

    Ok(contexts)
}

/// Comparator that orders [`ChunkContext`] by descending score (best first).
fn cmp_chunk_context_score_desc(a: &ChunkContext, b: &ChunkContext) -> std::cmp::Ordering {
    b.score.total_cmp(&a.score)
}

/// A single chunk with its surrounding context.
#[derive(Debug, Clone)]
pub struct ChunkContext {
    pub paper: Arc<Paper>,
    pub chunk_content: String,
    pub score: f32,
    pub page_number: Option<u32>,
    /// Id of the matched chunk (for structured citations / navigation).
    pub chunk_id: String,
    /// Heading chain of the matched chunk (e.g. "Intro > Methods"), without
    /// the "Section path: " prefix. `None` when the chunk has no heading
    /// ancestors.
    pub heading_path: Option<String>,
    /// Inlined caption/description of figures the chunk explicitly references
    /// (e.g. "Figure 2"). `None` when the chunk cites none or none resolve.
    pub referenced_context: Option<String>,
}

/// Build a RAG context string from chunk contexts with selectable metadata fields.
///
/// Deduplicates by paper and concatenates matched chunks per paper,
/// with ancestor headings as prefix. Enforces a max total char limit.
/// Preserves input order (assumed to be pre-sorted by relevance).
/// Only metadata fields in `include_meta_fields` are rendered in the header.
pub fn build_rag_context_compact(
    contexts: &[ChunkContext],
    max_chars: usize,
    include_meta_fields: &[&str],
) -> String {
    // Group by paper while preserving order
    let mut paper_groups: IndexMap<String, Vec<&ChunkContext>> = IndexMap::new();
    for ctx in contexts {
        paper_groups
            .entry(ctx.paper.id.clone())
            .or_default()
            .push(ctx);
    }

    let mut output = String::with_capacity(max_chars);
    let mut total_chars = 0;
    let mut truncated = false;

    for (_paper_id, group) in paper_groups {
        let paper = &group[0].paper;

        // Header only includes selected metadata fields
        let header_parts =
            paper.build_meta_parts(include_meta_fields, |title| format!("Paper: {title}"));

        if header_parts.is_empty() {
            continue;
        }
        let header = format!("--- {} ---\n", header_parts.join(" "));
        let header_chars = header.chars().count();
        if total_chars + header_chars > max_chars {
            truncated = true;
            break;
        }
        output.push_str(&header);
        total_chars += header_chars;

        for ctx in group {
            let mut chunk_text = String::new();
            let mut chunk_chars = 0usize;
            if let Some(ref path) = ctx.heading_path {
                let heading = format!("Section path: {path}\n");
                chunk_chars += heading.chars().count();
                chunk_text.push_str(&heading);
            }
            if let Some(page) = ctx.page_number {
                let page_str = format!("[Page {page}] ");
                chunk_chars += page_str.chars().count();
                chunk_text.push_str(&page_str);
            }
            chunk_chars += ctx.chunk_content.chars().count();
            chunk_text.push_str(&ctx.chunk_content);
            if let Some(ref referenced) = ctx.referenced_context {
                chunk_chars += 1 + referenced.chars().count();
                chunk_text.push('\n');
                chunk_text.push_str(referenced);
            }
            chunk_chars += 2; // "\n\n"
            chunk_text.push_str("\n\n");

            if total_chars + chunk_chars > max_chars {
                // Include as much of this chunk as fits instead of dropping it
                // (and every later chunk). Chunks arrive sorted by relevance, so
                // a truncated top chunk preserves far more answer-relevant text
                // than discarding it whole — discarding the top chunk was what
                // made the model report "insufficient information".
                let remaining = max_chars.saturating_sub(total_chars);
                if remaining > RAG_MIN_SOURCE_CHARS {
                    let fitted = crate::util::truncate_chars(&chunk_text, remaining);
                    output.push_str(&fitted);
                    total_chars += fitted.chars().count();
                }
                truncated = true;
                break;
            }
            output.push_str(&chunk_text);
            total_chars += chunk_chars;
        }

        if truncated {
            break;
        }
    }

    if truncated {
        output.push_str("...(truncated)\n");
    }

    output
}

/// Build a RAG context string from chunk contexts with all metadata fields.
///
/// Convenience wrapper around `build_rag_context_compact` that includes
/// all available metadata fields (title, authors, year, venue, affiliations,
/// emails, doi, keywords, extra).
pub fn build_rag_context_full_meta(contexts: &[ChunkContext], max_chars: usize) -> String {
    build_rag_context_compact(contexts, max_chars, &ALL_META_FIELDS)
}

/// Build a rich context string from retrieved sources, capping total length.
///
/// This is the canonical source-level RAG context assembler. Each source is
/// numbered and wrapped with paper metadata; content is truncated gracefully
/// when the total context would exceed `max_chars`.
pub fn build_rag_context<T: RagContextSource>(
    sources: &[T],
    max_chars: usize,
    compact: bool,
    include_meta_fields: &[&str],
) -> String {
    let mut context = String::new();
    let mut total_chars = 0usize;
    for (i, src) in sources.iter().enumerate() {
        let header = if compact {
            build_compact_header(i, src.paper(), include_meta_fields)
        } else {
            build_full_header(i, src.paper())
        };

        let header_chars = header.chars().count();
        let content_chars = src.content().chars().count();
        let projected_chars = total_chars + header_chars + content_chars + 2;
        if projected_chars > max_chars {
            let remaining = max_chars.saturating_sub(total_chars + header_chars + 2);
            if remaining > RAG_MIN_SOURCE_CHARS {
                context.push_str(&header);
                let truncated = crate::util::truncate_chars(src.content(), remaining);
                context.push_str(&truncated);
                context.push_str("\n\n");
                total_chars += header_chars + truncated.chars().count() + 2;
            }
            continue;
        }

        context.push_str(&header);
        context.push_str(src.content());
        context.push_str("\n\n");
        total_chars += header_chars + content_chars + 2;
    }
    context
}

fn build_compact_header(index: usize, paper: &Paper, include_meta_fields: &[&str]) -> String {
    let parts = paper.build_meta_parts(include_meta_fields, |title| {
        format!("Source {}: {}", index + 1, title)
    });
    if parts.is_empty() {
        format!("### Source {}\n", index + 1)
    } else {
        format!("### {}\n", parts.join(" "))
    }
}

fn build_full_header(index: usize, paper: &Paper) -> String {
    build_compact_header(index, paper, &PUBLIC_META_FIELDS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct TestSource {
        paper: Paper,
        content: String,
    }

    impl RagContextSource for TestSource {
        fn paper(&self) -> &Paper {
            &self.paper
        }
        fn content(&self) -> &str {
            &self.content
        }
    }

    #[test]
    fn test_build_rag_context_includes_metadata_and_content() {
        let mut paper = Paper::new("Unified RAG");
        paper.authors = vec!["A. Author".to_string(), "B. Author".to_string()];
        paper.published_date = Some("2026-06".to_string());
        paper.venue = Some("RAG Workshop".to_string());

        let source = TestSource {
            paper,
            content: "The context assembly moved to retrieval.rs.".to_string(),
        };

        let ctx = build_rag_context(&[source], 10_000, true, &["title", "authors"]);

        assert!(ctx.contains("Source 1: Unified RAG"));
        assert!(ctx.contains("by A. Author, B. Author"));
        assert!(ctx.contains("The context assembly moved to retrieval.rs."));
    }

    #[test]
    fn test_build_rag_context_full_header_includes_doi_and_keywords() {
        let mut paper = Paper::new("Deep Retrieval");
        paper.authors = vec!["C. Author".to_string()];
        paper.doi = Some("10.1000/test".to_string());
        paper.keywords = vec!["rag".to_string(), "rust".to_string()];
        paper.published_date = Some("2025".to_string());

        let source = TestSource {
            paper,
            content: "Full metadata header test.".to_string(),
        };

        let ctx = build_rag_context(&[source], 10_000, false, &[]);
        assert!(ctx.contains("Source 1: Deep Retrieval"));
        assert!(ctx.contains("DOI: 10.1000/test"));
        assert!(ctx.contains("Keywords: rag, rust"));
        assert!(ctx.contains("Full metadata header test."));
    }

    #[test]
    fn test_build_rag_context_truncates_gracefully() {
        let paper = Paper::new("Short Title");
        let source = TestSource {
            paper,
            content: "a".repeat(500),
        };

        // Header is "### Source 1: Short Title\n" (28 chars) + "\n\n" (2) = 30 overhead per source.
        // With max 100 chars we expect truncation, not an empty context.
        let ctx = build_rag_context(&[source], 100, true, &["title"]);
        assert!(ctx.contains("Source 1: Short Title"));
        assert!(ctx.contains("a"));
        assert!(ctx.len() <= 100);
    }

    #[test]
    fn test_build_rag_context_compact_chunks() {
        let mut paper = Paper::new("Chunk Paper");
        paper.authors = vec!["D. Author".to_string()];

        let ctx = ChunkContext {
            paper: Arc::new(paper),
            chunk_content: "This is the relevant chunk.".to_string(),
            score: 0.9,
            page_number: Some(42),
            chunk_id: "c1".to_string(),
            heading_path: Some("Intro > Background".to_string()),
            referenced_context: None,
        };

        let out = build_rag_context_compact(&[ctx], 10_000, &["title", "authors"]);

        assert!(out.contains("Paper: Chunk Paper"));
        assert!(out.contains("by D. Author"));
        assert!(out.contains("Section path: Intro > Background"));
        assert!(out.contains("[Page 42]"));
        assert!(out.contains("This is the relevant chunk."));
    }

    #[test]
    fn build_rag_context_compact_truncates_top_chunk_instead_of_dropping() {
        let paper = Arc::new(Paper::new("Big Paper"));
        // The most relevant chunk is larger than the whole budget. The old
        // behavior discarded it (and every later chunk), leaving only the paper
        // header — which is what made the model report "insufficient
        // information". The fix includes a truncated prefix of the top chunk.
        let ctx = ChunkContext {
            paper,
            chunk_content: "RELEVANT ".repeat(1000), // ~9000 chars
            score: 0.9,
            page_number: None,
            chunk_id: "c1".to_string(),
            heading_path: Some("Methods > Training".to_string()),
            referenced_context: None,
        };

        let out = build_rag_context_compact(&[ctx], 500, &["title"]);

        assert!(out.contains("Paper: Big Paper"));
        assert!(out.contains("Section path: Methods > Training"));
        assert!(
            out.contains("RELEVANT"),
            "top chunk must be included, not dropped"
        );
        assert!(out.contains("...(truncated)"));
        // Budget respected (header + fitted chunk), plus the trailing marker.
        assert!(
            out.chars().count() <= 500 + 20,
            "len={}",
            out.chars().count()
        );
    }

    #[test]
    fn test_build_rag_context_full_meta_includes_doi() {
        let mut paper = Paper::new("DOI Paper");
        paper.doi = Some("10.1000/doi".to_string());

        let ctx = ChunkContext {
            paper: Arc::new(paper),
            chunk_content: "chunk with doi".to_string(),
            score: 0.5,
            page_number: None,
            chunk_id: "c2".to_string(),
            heading_path: None,
            referenced_context: None,
        };

        let out = build_rag_context_full_meta(&[ctx], 10_000);
        assert!(out.contains("DOI: 10.1000/doi"));
        assert!(out.contains("chunk with doi"));
    }

    #[test]
    fn chunk_context_sorts_highest_score_first() {
        let mk = |score: f32| ChunkContext {
            paper: Arc::new(Paper::new("p")),
            chunk_content: String::new(),
            score,
            page_number: None,
            chunk_id: String::new(),
            heading_path: None,
            referenced_context: None,
        };
        let mut contexts = [mk(0.3), mk(0.9), mk(0.6)];
        contexts.sort_by(cmp_chunk_context_score_desc);
        let scores: Vec<f32> = contexts.iter().map(|c| c.score).collect();
        assert_eq!(scores, vec![0.9, 0.6, 0.3]);
    }

    fn mk_figure(id: &str, caption: Option<&str>, description: Option<&str>) -> FigureInfo {
        FigureInfo {
            id: id.to_string(),
            paper_id: "p".to_string(),
            caption: caption.map(str::to_string),
            description: description.map(str::to_string),
            image_path: None,
            page_number: None,
            bbox: None,
            figure_label: None,
        }
    }

    #[test]
    fn extract_figure_numbers_handles_plural_and_lists() {
        let nums = extract_referenced_numbers("see Figures 1, 2 and 3 for details", &FIG_REF_RE);
        assert_eq!(nums, vec![1, 2, 3]);
    }

    #[test]
    fn extract_figure_numbers_expands_ranges() {
        let nums = extract_referenced_numbers("as shown in Fig. 2-4", &FIG_REF_RE);
        assert_eq!(nums, vec![2, 3, 4]);
    }

    #[test]
    fn extract_numbers_empty_when_unreferenced() {
        assert!(extract_referenced_numbers("no figures here", &FIG_REF_RE).is_empty());
    }

    #[test]
    fn build_referenced_context_inlines_caption() {
        let figures = vec![
            mk_figure("p_fig1", Some("Loss curves"), Some("train vs val")),
            mk_figure("p_fig2", None, Some("ablation")),
        ];

        let ctx = build_referenced_context("Figure 1 summarizes our findings.", &figures)
            .expect("should inline referenced objects");

        assert!(ctx.contains("[Referenced Figure 1: Loss curves \u{2014} train vs val]"));
    }

    #[test]
    fn build_referenced_context_returns_none_without_references() {
        let figures = vec![mk_figure("p_fig1", Some("x"), None)];
        assert!(build_referenced_context("plain prose with no cites", &figures).is_none());
    }

    #[test]
    fn build_referenced_context_skips_out_of_range_numbers() {
        let figures = vec![mk_figure("p_fig1", Some("only one"), None)];
        // References Figure 5 which does not exist -> nothing usable.
        assert!(build_referenced_context("see Figure 5", &figures).is_none());
    }

    fn mk_chunk(id: &str, parent_id: Option<&str>, content: &str) -> Chunk {
        let mut c = Chunk::new("p", id, crate::chunker::ChunkType::Paragraph, content);
        c.parent_id = parent_id.map(str::to_string);
        c
    }

    #[test]
    fn heading_path_walks_parent_chain_root_first() {
        let chapter = mk_chunk("p_ch1", None, "Chapter One");
        let section = mk_chunk("p_s1", Some("p_ch1"), "Methods");
        let leaf = mk_chunk("p_p1", Some("p_s1"), "body text");
        let chain = vec![leaf.clone(), chapter, section];

        assert_eq!(
            heading_path_from_ancestors(&leaf, &chain),
            Some("Chapter One > Methods".to_string())
        );
    }

    #[test]
    fn heading_path_none_without_ancestors() {
        let leaf = mk_chunk("p_p1", None, "body text");
        assert!(heading_path_from_ancestors(&leaf, std::slice::from_ref(&leaf)).is_none());
    }

    #[test]
    fn heading_path_stops_when_chain_breaks() {
        // Parent id references a chunk that is not in the candidate set.
        let leaf = mk_chunk("p_p1", Some("missing"), "body text");
        assert!(heading_path_from_ancestors(&leaf, std::slice::from_ref(&leaf)).is_none());
    }
}
