use crate::config::PdfExtractionConfig;
use crate::error::{PaperedError, Result};
use crate::paper::mineru::MinerUClient;
use crate::paper::pdf_oxide::extract_with_pdf_oxide;
use crate::paper::source::DocumentSource;
use regex::Regex;
use std::path::Path;

/// Source of PDF text extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractorSource {
    /// MinerU API — layout-aware, structured Markdown output.
    MinerU,
    /// pdf_oxide — fast zero-dependency Rust PDF extraction with Markdown support.
    PdfOxide,
    /// Plain text file (.txt) — no extraction engine needed, raw content.
    PlainText,
    /// Direct import (e.g., standalone image files).
    DirectImport,
    /// LaTeX file (.tex) — raw source text extraction.
    LatexExtract,
    /// Office Open XML document (.docx) — text extraction via docx-rs.
    Docx,
}

/// Rich extraction result from MinerU (optional metadata beyond markdown).
#[derive(Debug, Clone, Default)]
pub struct RichExtraction {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub affiliations: Vec<String>,
    pub emails: Vec<String>,
    pub abstract_text: Option<String>,
    pub figures: Vec<crate::paper::mineru::MinerUFigure>,
    pub keywords: Vec<String>,
    pub urls: Vec<String>,
    pub extra: Option<String>,
    pub doi: Option<String>,
}

/// Extracted text with provenance metadata.
#[derive(Debug, Clone)]
pub struct ExtractedText {
    pub text: String,
    pub source: ExtractorSource,
    /// The original document format (Pdf, Markdown, PlainText, Image).
    pub document_source: DocumentSource,
    /// Whether the text is structured Markdown (from MinerU/pdf_oxide) or raw plain text.
    pub is_structured: bool,
    /// Rich metadata extracted by MinerU (only populated when source is MinerU).
    pub rich: Option<RichExtraction>,
}

impl ExtractedText {
    /// Create a simple plain-text extracted result.
    pub fn plain(text: String, source: ExtractorSource, document_source: DocumentSource) -> Self {
        Self {
            text,
            source,
            document_source,
            is_structured: false,
            rich: None,
        }
    }

    /// Create a structured Markdown extracted result.
    pub fn structured(
        text: String,
        source: ExtractorSource,
        document_source: DocumentSource,
    ) -> Self {
        Self {
            text,
            source,
            document_source,
            is_structured: true,
            rich: None,
        }
    }

    /// Create a structured Markdown result with rich MinerU metadata.
    pub fn structured_with_rich(
        text: String,
        rich: RichExtraction,
        document_source: DocumentSource,
    ) -> Self {
        Self {
            text,
            source: ExtractorSource::MinerU,
            document_source,
            is_structured: true,
            rich: Some(rich),
        }
    }
}

/// Extract text content from a Markdown file with YAML frontmatter parsing.
pub async fn extract_markdown(path: &Path) -> Result<ExtractedText> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(PaperedError::Io)?;

    let (frontmatter, body) = parse_frontmatter(&content);

    let mut rich = RichExtraction::default();
    if let Some(fm) = frontmatter {
        rich.title = fm.get("title").and_then(|v| v.as_str().map(String::from));
        if let Some(authors) = fm.get("authors") {
            rich.authors = extract_authors_from_yaml(authors);
        }
        if let Some(tags) = fm.get("tags").or_else(|| fm.get("keywords")) {
            rich.keywords = extract_keywords_from_yaml(tags);
        }
        rich.doi = fm.get("doi").and_then(|v| v.as_str().map(String::from));
    }

    Ok(ExtractedText {
        text: body,
        source: ExtractorSource::DirectImport,
        document_source: DocumentSource::Markdown,
        is_structured: true,
        rich: Some(rich),
    })
}

/// Wrap already-extracted content in an unstructured `ExtractedText`, using the
/// file stem as the provisional title. Callers perform their own empty checks
/// since the rejection message varies by format.
fn unstructured_result(
    content: String,
    path: &Path,
    source: ExtractorSource,
    document_source: DocumentSource,
) -> ExtractedText {
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string();

    ExtractedText {
        text: content,
        source,
        document_source,
        is_structured: false,
        rich: Some(RichExtraction {
            title: Some(title),
            ..Default::default()
        }),
    }
}

/// Extract text content from a plain text (.txt) file.
///
/// Uses the filename as a provisional title. Empty files are rejected.
/// Metadata is entirely LLM-generated since plain text has no inherent structure.
pub async fn extract_plain_text(path: &Path) -> Result<ExtractedText> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(PaperedError::Io)?;

    if content.trim().is_empty() {
        return Err(PaperedError::invalid_argument(format!(
            "Empty file: {}",
            path.display()
        )));
    }

    Ok(unstructured_result(
        content,
        path,
        ExtractorSource::PlainText,
        DocumentSource::PlainText,
    ))
}

/// Extract text content from a LaTeX (.tex) file.
///
/// Reads the raw source as plain text. LaTeX is not compiled or parsed;
/// the raw content is passed to the LLM for metadata extraction.
/// Filename used as provisional title. Empty files rejected.
pub async fn extract_latex(path: &Path) -> Result<ExtractedText> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(PaperedError::Io)?;

    if content.trim().is_empty() {
        return Err(PaperedError::invalid_argument(format!(
            "Empty file: {}",
            path.display()
        )));
    }

    Ok(unstructured_result(
        content,
        path,
        ExtractorSource::LatexExtract,
        DocumentSource::Latex,
    ))
}

/// Extract plain text from a docx Document by walking its child tree.
fn collect_docx_text(document: &docx_rs::Document) -> String {
    let mut text = String::new();

    for child in &document.children {
        match child {
            docx_rs::DocumentChild::Paragraph(p) => {
                let raw = p.raw_text();
                if !raw.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&raw);
                }
            }
            docx_rs::DocumentChild::Table(t) => {
                collect_table_text(t, &mut text);
            }
            docx_rs::DocumentChild::Section(s) => {
                for child in s.children() {
                    match child {
                        docx_rs::SectionChild::Paragraph(p) => {
                            let raw = p.raw_text();
                            if !raw.is_empty() {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(&raw);
                            }
                        }
                        docx_rs::SectionChild::Table(t) => {
                            collect_table_text(t, &mut text);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    text
}

fn collect_table_text(table: &docx_rs::Table, text: &mut String) {
    for row in &table.rows {
        let docx_rs::TableChild::TableRow(row) = row;
        for cell in &row.cells {
            let docx_rs::TableRowChild::TableCell(cell) = cell;
            for content in &cell.children {
                match content {
                    docx_rs::TableCellContent::Paragraph(p) => {
                        let raw = p.raw_text();
                        if !raw.is_empty() {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(&raw);
                        }
                    }
                    docx_rs::TableCellContent::Table(t) => {
                        collect_table_text(t, text);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Extract text content from a .docx (Office Open XML) file.
///
/// Uses docx-rs to parse the archive and extract plain text from paragraphs,
/// tables, and sections. Filename used as provisional title. Empty files rejected.
pub async fn extract_docx(path: &Path) -> Result<ExtractedText> {
    let path_buf = path.to_path_buf();
    let path_display = path_buf.display().to_string();

    let content = tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&path_buf).map_err(PaperedError::Io)?;

        let docx = docx_rs::read_docx(&bytes).map_err(|e| {
            PaperedError::invalid_argument(format!(
                "Failed to parse docx file '{path_display}': {e}"
            ))
        })?;

        Ok::<String, PaperedError>(collect_docx_text(&docx.document))
    })
    .await
    .map_err(|e| {
        PaperedError::invalid_argument(format!("spawn_blocking panicked for docx parsing: {e}"))
    })??;

    if content.trim().is_empty() {
        return Err(PaperedError::invalid_argument(format!(
            "Empty or unreadable docx file: {}",
            path.display()
        )));
    }

    Ok(unstructured_result(
        content,
        path,
        ExtractorSource::Docx,
        DocumentSource::OfficeDocument,
    ))
}

/// Extract text content from an image file.
///
/// Images have no extractable text, so this creates a placeholder with the
/// image path stored as a figure in the rich extraction for downstream
/// multimodal embedding.
pub async fn extract_image_as_text(path: &Path) -> Result<ExtractedText> {
    let filename = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled Image");

    let image_path = path.to_string_lossy().into_owned();

    let rich = RichExtraction {
        title: Some(filename.to_string()),
        figures: vec![crate::paper::mineru::MinerUFigure {
            caption: Some(filename.to_string()),
            image_path: Some(image_path),
            page_number: None,
        }],
        ..Default::default()
    };

    Ok(ExtractedText {
        text: format!("[Image: {filename}]"),
        source: ExtractorSource::DirectImport,
        document_source: DocumentSource::Image,
        is_structured: false,
        rich: Some(rich),
    })
}

/// Parse YAML frontmatter from a Markdown document.
///
/// Returns `(Some(parsed_value), body)` if valid frontmatter is found between `---` delimiters,
/// or `(None, original_content)` if no frontmatter is present or YAML parsing fails.
pub fn parse_frontmatter(content: &str) -> (Option<serde_json::Value>, String) {
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(content);
    if !content.starts_with("---\n") {
        return (None, content.to_string());
    }
    if let Some(end) = content[4..].find("\n---") {
        let yaml_str = &content[4..4 + end];
        let body = &content[4 + end + 4..];
        match yaml_serde::from_str::<serde_json::Value>(yaml_str) {
            Ok(value) => (Some(value), body.trim().to_string()),
            Err(_) => (None, body.trim().to_string()),
        }
    } else {
        (None, content.to_string())
    }
}

/// Extract a list of author names from a YAML value.
///
/// Delegates to the shared string/array parser.
fn extract_authors_from_yaml(value: &serde_json::Value) -> Vec<String> {
    crate::index::indexer::helpers::parse_string_or_array(Some(value))
}

/// Extract a list of keywords/tags from a YAML value.
///
/// Delegates to the shared string/array parser.
fn extract_keywords_from_yaml(value: &serde_json::Value) -> Vec<String> {
    crate::index::indexer::helpers::parse_string_or_array(Some(value))
}

/// Extract text content from a PDF file.
///
/// Priority:
/// 1. MinerU API (if enabled and available)
/// 2. pdf_oxide (local, zero-dependency, Markdown-capable)
pub async fn extract_pdf_text(
    path: &Path,
    mineru: Option<&MinerUClient>,
    paper_data_dir: Option<&Path>,
    pdf_config: &PdfExtractionConfig,
) -> Result<ExtractedText> {
    if !path.exists() {
        return Err(PaperedError::NotFound(
            format!("PDF file not found: {}", path.display()),
            None,
        ));
    }

    // 1. Try MinerU first
    if let Some(client) = mineru
        && client.is_enabled()
    {
        let bytes = tokio::fs::read(path).await.map_err(PaperedError::Io)?;
        let data_dir = paper_data_dir.unwrap_or_else(|| std::path::Path::new("."));
        match client.extract_pdf(bytes, data_dir, pdf_config).await {
            Ok(result) if !result.markdown.trim().is_empty() => {
                tracing::info!("Using MinerU extraction for {}", path.display());
                let rich = RichExtraction {
                    title: result.title,
                    authors: result.authors,
                    affiliations: result.affiliations,
                    emails: result.emails,
                    abstract_text: result.abstract_text,
                    figures: result.figures,
                    keywords: result.keywords,
                    urls: result.urls,
                    extra: result.extra,
                    doi: None,
                };
                return Ok(ExtractedText::structured_with_rich(
                    result.markdown,
                    rich,
                    DocumentSource::Pdf,
                ));
            }
            Err(e) => {
                tracing::warn!(
                    "MinerU extraction failed for {}: {}, falling back to pdf_oxide",
                    path.display(),
                    e
                );
            }
            _ => {
                tracing::warn!("MinerU returned empty result, falling back to pdf_oxide");
            }
        }
    }

    // 2. pdf_oxide local extraction (spawn_blocking to avoid blocking the async runtime)
    let path_buf = path.to_path_buf();
    let data_dir = paper_data_dir.map(std::path::Path::to_path_buf);
    let pdf_config_clone = pdf_config.clone();
    let path_str = path_buf.display().to_string();
    match tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            extract_with_pdf_oxide(&path_buf, data_dir.as_deref(), &pdf_config_clone)
        }))
        .map_err(|panic_payload| {
            let msg = panic_payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    panic_payload
                        .downcast_ref::<&str>()
                        .map(std::string::ToString::to_string)
                })
                .unwrap_or_else(|| "pdf_oxide panicked with non-string payload".to_string());
            PaperedError::PdfParse(format!("pdf_oxide panicked: {msg}"), None)
        })?
    })
    .await
    {
        Ok(Ok(extracted)) => {
            tracing::info!(
                "Using pdf_oxide extraction for {} (structured={})",
                path_str,
                extracted.is_structured
            );
            Ok(extracted)
        }
        Ok(Err(e)) => {
            tracing::error!("pdf_oxide extraction failed for {}: {}", path_str, e);
            Err(e)
        }
        Err(e) => {
            tracing::error!("pdf_oxide task panicked for {}: {}", path_str, e);
            Err(PaperedError::PdfParse(
                format!("pdf_oxide task panicked: {e}"),
                None,
            ))
        }
    }
}

/// Compute SHA256 hash of file content, streaming with BufReader.
pub fn compute_file_hash(path: &Path) -> Result<String> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path).map_err(PaperedError::Io)?;
    let mut reader = std::io::BufReader::new(&mut file);
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf).map_err(PaperedError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Preprocess and clean extracted text.
///
/// For structured Markdown (MinerU / pdf_oxide), preprocessing is lighter — we mainly
/// normalize whitespace and remove control characters since the output is already clean.
/// For plain text fallback, heavier cleaning is applied.
pub fn preprocess_text(
    extracted: &ExtractedText,
    config: &PdfExtractionConfig,
) -> (String, QualityResult) {
    let text = &extracted.text;

    if text.trim().is_empty() {
        return (
            String::new(),
            QualityResult {
                score: 0,
                issues: vec!["empty".to_string()],
                action: IndexAction::Reject,
            },
        );
    }

    // 1. Normalize whitespace in a single pass
    let mut processed = String::with_capacity(text.len());
    let mut prev_was_space = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                processed.push('\n');
                prev_was_space = false;
            }
            '\t' | '\u{00A0}' | '\u{2000}'..='\u{200B}' | ' ' => {
                if !prev_was_space {
                    processed.push(' ');
                    prev_was_space = true;
                }
            }
            _ => {
                processed.push(ch);
                prev_was_space = false;
            }
        }
    }

    // 2. Remove control characters (keep newlines)
    static CONTROL_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").expect("valid regex")
    });
    processed = CONTROL_RE.replace_all(&processed, "").to_string();

    // 3. Clean up excessive newlines
    static NEWLINES_RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\n{4,}").expect("valid regex"));
    processed = NEWLINES_RE.replace_all(&processed, "\n\n\n").to_string();

    // 4. Rejoin PDF line-break hyphenation (pdf_oxide only). MinerU output is
    //    clean; other sources are not PDF-extracted. Deterministic and free —
    //    the costlier LLM repair of intra-word spaces happens later per chunk.
    if config.fix_text_artifacts && extracted.source == ExtractorSource::PdfOxide {
        processed = crate::util::fix_pdf_hyphenation(&processed).into_owned();
    }

    processed = processed.trim().to_string();

    let quality = assess_quality(&processed, config);
    (processed, quality)
}

/// Action to take based on text quality assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexAction {
    /// Quality is good — proceed with normal indexing.
    Index,
    /// Quality is marginal — allow indexing but mark with a warning.
    IndexWithWarning,
    /// Quality is too low — reject the document.
    Reject,
}

#[derive(Debug, Clone)]
pub struct QualityResult {
    pub score: u8,
    pub issues: Vec<String>,
    pub action: IndexAction,
}

fn estimated_word_count(text: &str) -> usize {
    let whitespace_words = text.split_whitespace().count();
    let cjk_chars = text.chars().filter(|&c| crate::util::is_cjk(c)).count();
    let total_chars = text.chars().count();
    if total_chars > 0 && cjk_chars > total_chars / 2 {
        // For CJK-dominant text: each whitespace group + 2 CJK chars ≈ 1 word
        whitespace_words + cjk_chars / 2
    } else {
        whitespace_words
    }
}

fn assess_quality(text: &str, config: &PdfExtractionConfig) -> QualityResult {
    let trimmed = text.trim();
    let mut issues = Vec::new();
    let mut score: i16 = 100;

    // 1. Length check
    if trimmed.is_empty() || trimmed.len() < config.quality_min_chars {
        score -= 30;
        issues.push("too_short".to_string());
    }

    // 2. Word count check
    let word_count = estimated_word_count(trimmed);
    if word_count < config.quality_min_words {
        score -= 20;
        issues.push("too_few_words".to_string());
    }

    // 3. Line count check
    let line_count = trimmed.lines().count();
    if line_count < config.quality_min_lines {
        score -= 20;
        issues.push("too_few_lines".to_string());
    }

    // 4. Alphanumeric ratio check (CJK-aware)
    let total_chars = trimmed.chars().count().max(1);
    let meaningful_count = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric() || crate::util::is_cjk(*c))
        .count();
    let ratio = meaningful_count as f32 / total_chars as f32;
    if ratio < config.quality_min_alphanumeric_ratio {
        score -= 40;
        issues.push("low_alphanumeric_ratio".to_string());
    }

    let score = score.clamp(0, 100) as u8;

    let action = if score < config.quality_reject_threshold {
        IndexAction::Reject
    } else if score < config.quality_warn_threshold {
        IndexAction::IndexWithWarning
    } else {
        IndexAction::Index
    };

    QualityResult {
        score,
        issues,
        action,
    }
}

/// Minimal sanitization: deduplicate arrays, trim whitespace, filter empty/null strings.
/// All intelligent cleaning (author name normalization, email filtering, DOI formatting,
/// null-value pruning) is delegated to the LLM extraction prompt.
pub fn sanitize_paper_metadata(meta: &mut PaperMetadata) {
    crate::util::dedup_strings_in_place(&mut meta.authors);
    crate::util::dedup_strings_in_place(&mut meta.affiliations);
    crate::util::dedup_strings_in_place(&mut meta.emails);
    crate::util::dedup_strings_in_place(&mut meta.keywords);
    meta.title = meta
        .title
        .take()
        .as_deref()
        .and_then(crate::util::filter_non_empty_string);
    meta.venue = meta
        .venue
        .take()
        .as_deref()
        .and_then(crate::util::filter_non_empty_string);
    meta.doi = meta
        .doi
        .take()
        .as_deref()
        .and_then(crate::util::filter_non_empty_string);
    meta.abstract_text = meta
        .abstract_text
        .take()
        .as_deref()
        .and_then(crate::util::filter_non_empty_string);
}

/// A figure extracted by the LLM during structured paper analysis.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmFigure {
    /// The figure's label exactly as written in the paper — "1", "S1", "3a",
    /// "A.1", etc. Used directly as the figure identifier.
    pub label: String,
    pub caption: String,
}

#[derive(Debug, Clone, Default)]
pub struct PaperMetadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub affiliations: Vec<String>,
    pub emails: Vec<String>,
    pub corresponding_author: Vec<String>,
    pub published_date: Option<String>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    pub urls: Vec<String>,
    pub data_availability: Option<String>,
    pub paper_type: Option<String>,
    /// Bio-entities extracted alongside sections (species, genes, techniques,
    /// pathways). Empty for non-biology papers.
    pub entities: crate::paper::BioEntities,
    /// Additional metadata as a JSON string for flexible storage.
    pub extra: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_valid() {
        let content = "---\ntitle: Test Paper\nauthors: [Alice, Bob]\n---\n\nThis is the body.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_some());
        assert_eq!(body, "This is the body.");
    }

    #[test]
    fn test_parse_frontmatter_no_delimiter() {
        let content = "Just plain markdown without frontmatter.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn test_parse_frontmatter_invalid_yaml() {
        let content = "---\nnot valid yaml: [unclosed\n---\n\nBody here.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, "Body here.");
    }
}
