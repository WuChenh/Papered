//! pdf_oxide Adapter
//!
//! Full-featured wrapper around pdf_oxide for local PDF extraction.
//! Leverages every default-feature API for academic paper indexing:
//! XMP metadata, Info dictionary, table of contents, span-level heading
//! detection, artifact removal, image extraction, and structured Markdown.
//! This is a complete offline alternative to MinerU.

use crate::config::PdfExtractionConfig;
use crate::error::{PaperedError, Result};
use crate::paper::mineru::MinerUFigure;
use crate::paper::parser::{ExtractedText, ExtractorSource, RichExtraction};
use crate::paper::source::DocumentSource;
pub use pdf_oxide::PdfDocument;
use std::collections::HashSet;
use std::path::Path;

pub fn extract_with_pdf_oxide(
    path: &Path,
    paper_data_dir: Option<&Path>,
    config: &PdfExtractionConfig,
) -> Result<ExtractedText> {
    let doc = pdf_oxide::PdfDocument::open(path).map_err(|e| {
        PaperedError::PdfParse(format!("pdf_oxide open error: {e}"), Some(Box::new(e)))
    })?;

    // Early guard: encrypted PDFs cannot be extracted.
    if doc.is_encrypted() {
        return Err(PaperedError::PdfParse(
            "PDF is encrypted, cannot extract text".to_string(),
            None,
        ));
    }

    let page_count = doc.page_count().unwrap_or(0);
    if page_count == 0 {
        return Err(PaperedError::PdfParse("PDF has no pages".to_string(), None));
    }
    if page_count > config.max_pages_warning {
        tracing::warn!(
            "PDF has {} pages (above warning threshold {}), extraction may be slow",
            page_count,
            config.max_pages_warning
        );
    }

    // Quick pre-check: image-only / scanned PDF detection.
    let sample_text = doc.extract_all_text().unwrap_or_default();
    if sample_text.trim().len() < 100 && page_count > 2 {
        return Err(PaperedError::PdfParse(
            "PDF appears to be image-only (no extractable text). Consider OCR.".to_string(),
            None,
        ));
    }

    // Page-level content scan: detect near-blank pages.
    let (valid_pages, skipped_pages) = scan_page_content(&doc, page_count, config);
    if skipped_pages > 0 {
        tracing::debug!(
            "pdf_oxide skipped {} near-blank pages (threshold {} chars)",
            skipped_pages,
            config.min_chars_per_page
        );
    }
    if !valid_pages.is_empty() && valid_pages.len() < page_count / 2 {
        tracing::warn!(
            "Only {} of {} pages have meaningful content; PDF may be mostly images/scans",
            valid_pages.len(),
            page_count
        );
    }

    // Remove running headers/footers/page numbers.
    let removed = doc.remove_artifacts(config.artifact_threshold).unwrap_or(0);
    if removed > 0 {
        tracing::debug!("pdf_oxide removed {} artifact regions", removed);
    }

    // Build rich metadata from every available PDF internal source.
    let mut rich = RichExtraction::default();
    populate_xmp_metadata(&doc, &mut rich, config);
    populate_info_metadata(&doc, &mut rich, config);
    populate_outline(&doc, &mut rich);
    populate_span_metadata(&doc, &mut rich);
    populate_page_box(&doc, &mut rich);
    populate_mark_info(&doc, &mut rich);
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        extract_and_save_images(&doc, paper_data_dir, &mut rich, config);
    }))
    .is_err()
    {
        tracing::warn!("pdf_oxide image extraction panicked, continuing with text-only extraction");
        rich.figures.clear();
    }

    // Layout-quality-aware extraction strategy.
    let layout_quality = if config.enable_layout_quality_signals {
        assess_layout_quality(&doc, page_count)
    } else {
        LayoutQuality::default()
    };

    let options = pdf_oxide::converters::ConversionOptions {
        detect_headings: layout_quality.heading_count > 0 || !config.enable_layout_quality_signals,
        extract_tables: layout_quality.avg_spans_per_page > 3.0
            || !config.enable_layout_quality_signals,
        include_images: false,
        embed_images: false,
        ..Default::default()
    };

    // --- Markdown extraction ---
    let combined = doc.to_markdown_all(&options).map_err(|e| {
        PaperedError::PdfParse(format!("pdf_oxide to_markdown_all: {e}"), Some(Box::new(e)))
    })?;

    if combined.trim().len() >= 200 {
        tracing::info!(
            "pdf_oxide extracted {} pages as Markdown ({} chars), {} images, layout={:?}",
            page_count,
            combined.len(),
            rich.figures.len(),
            layout_quality
        );
        return Ok(ExtractedText {
            text: combined,
            source: ExtractorSource::PdfOxide,
            document_source: DocumentSource::Pdf,
            is_structured: true,
            rich: Some(rich),
        });
    }

    // --- Plain text fallback ---
    let text = doc.extract_all_text().map_err(|e| {
        PaperedError::PdfParse(
            format!("pdf_oxide extract_all_text: {e}"),
            Some(Box::new(e)),
        )
    })?;

    if text.trim().is_empty() {
        return Err(PaperedError::PdfParse(
            "pdf_oxide: no text extracted".to_string(),
            None,
        ));
    }

    tracing::info!("pdf_oxide extracted {} chars as plain text", text.len());
    Ok(ExtractedText::plain(
        text,
        ExtractorSource::PdfOxide,
        DocumentSource::Pdf,
    ))
}

// =============================================================================
// Layout quality signals
// =============================================================================

#[derive(Debug, Clone, Default)]
struct LayoutQuality {
    pub heading_count: usize,
    pub avg_spans_per_page: f32,
}

fn assess_layout_quality(doc: &pdf_oxide::PdfDocument, page_count: usize) -> LayoutQuality {
    let mut heading_count = 0usize;
    let mut total_spans = 0usize;

    // Scan first few pages for heading and span signals.
    let scan_pages = page_count.min(5);
    for page_idx in 0..scan_pages {
        if let Ok(spans) = doc.extract_spans(page_idx) {
            total_spans += spans.len();
            for span in &spans {
                if span.font_size >= 14.0
                    && matches!(span.font_weight, pdf_oxide::layout::FontWeight::Bold)
                {
                    heading_count += 1;
                }
            }
        }
    }

    let avg_spans_per_page = if scan_pages > 0 {
        total_spans as f32 / scan_pages as f32
    } else {
        0.0
    };

    LayoutQuality {
        heading_count,
        avg_spans_per_page,
    }
}

/// Scan each page for character count to identify near-blank pages.
fn scan_page_content(
    doc: &pdf_oxide::PdfDocument,
    page_count: usize,
    config: &PdfExtractionConfig,
) -> (Vec<usize>, usize) {
    let mut valid = Vec::new();
    let mut skipped = 0usize;
    for page_idx in 0..page_count {
        let char_count = match doc.extract_spans(page_idx) {
            Ok(spans) => spans.iter().map(|s| s.text.len()).sum(),
            Err(_) => 0,
        };
        if char_count >= config.min_chars_per_page {
            valid.push(page_idx);
        } else {
            skipped += 1;
        }
    }
    (valid, skipped)
}

// =============================================================================
// Metadata population helpers — each targets one PDF internal source
// =============================================================================

fn populate_xmp_metadata(
    doc: &pdf_oxide::PdfDocument,
    rich: &mut RichExtraction,
    config: &PdfExtractionConfig,
) {
    match pdf_oxide::extractors::XmpExtractor::extract(doc) {
        Ok(Some(xmp)) => {
            if let Some(t) = xmp
                .dc_title
                .filter(|s| !s.is_empty() && is_plausible_title(s, config))
            {
                rich.title = Some(t);
            }
            if !xmp.dc_creator.is_empty() {
                rich.authors = xmp.dc_creator;
            }
            if let Some(desc) = xmp.dc_description.filter(|s| !s.is_empty()) {
                rich.abstract_text = Some(desc);
            }
            if !xmp.dc_subject.is_empty() {
                rich.keywords = xmp.dc_subject.clone();
            }
            if let Some(kw) = xmp.pdf_keywords.filter(|s| !s.is_empty()) {
                let extra: Vec<String> = kw
                    .split(&[',', ';'][..])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && !rich.keywords.contains(s))
                    .collect();
                rich.keywords.extend(extra);
            }
            if let Some(tool) = xmp.xmp_creator_tool.filter(|s| !s.is_empty()) {
                append_extra(rich, &format!("CreatorTool: {tool}"));
            }
            if let Some(date) = xmp.xmp_create_date.filter(|s| !s.is_empty()) {
                append_extra(rich, &format!("CreateDate: {date}"));
            }
            tracing::debug!(
                "XMP: title={:?}, authors={}, keywords={}",
                rich.title,
                rich.authors.len(),
                rich.keywords.len()
            );
        }
        Ok(None) => tracing::debug!("no XMP metadata found"),
        Err(e) => tracing::warn!("XMP extraction error: {}", e),
    }
}

/// Fallback: /Info dictionary metadata (title, author, subject).
/// Only fills fields that XMP did not already provide.
fn populate_info_metadata(
    doc: &pdf_oxide::PdfDocument,
    rich: &mut RichExtraction,
    config: &PdfExtractionConfig,
) {
    let trailer = doc.trailer();
    let Some(dict) = trailer.as_dict() else {
        return;
    };
    let Some(info_ref) = dict
        .get("Info")
        .and_then(pdf_oxide::object::Object::as_reference)
    else {
        return;
    };

    let info_obj = match doc.load_object(info_ref) {
        Ok(o) => o,
        Err(_) => return,
    };
    let info = pdf_oxide::editor::DocumentInfo::from_object(&info_obj);

    // Only fill if XMP didn't already provide the field.
    if rich.title.is_none()
        && let Some(t) = info
            .title
            .filter(|s| !s.is_empty() && is_plausible_title(s, config))
    {
        rich.title = Some(t);
    }
    if rich.authors.is_empty()
        && let Some(a) = info.author.filter(|s| !s.is_empty())
    {
        rich.authors = vec![a];
    }
    if rich.abstract_text.is_none()
        && let Some(s) = info.subject.filter(|s| !s.is_empty())
    {
        rich.abstract_text = Some(s);
    }
    if rich.keywords.is_empty()
        && let Some(k) = info.keywords.as_deref().filter(|s| !s.is_empty())
    {
        rich.keywords = k
            .split(&[',', ';'][..])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Some(producer) = info.producer.filter(|s| !s.is_empty()) {
        append_extra(rich, &format!("Producer: {producer}"));
    }
    tracing::debug!(
        "Info dict: title={:?}, author={:?}, keywords={:?}",
        rich.title,
        rich.authors.first(),
        rich.keywords,
    );
}

fn populate_outline(doc: &pdf_oxide::PdfDocument, rich: &mut RichExtraction) {
    match doc.get_outline() {
        Ok(Some(items)) if !items.is_empty() => {
            let depth = max_outline_depth(&items, 0);
            let titles: Vec<String> = items.iter().map(|i| i.title.clone()).collect();
            append_extra(
                rich,
                &format!(
                    "TOC: {} sections, max_depth={}, top={:?}",
                    items.len(),
                    depth,
                    &titles[..titles.len().min(5)]
                ),
            );
            tracing::debug!("TOC: {} items, max depth {}", items.len(), depth);
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("outline extraction error: {}", e),
    }
}

/// Span-level heading and layout detection.
/// Uses font size + font weight to identify title candidates and heading spans.
fn populate_span_metadata(doc: &pdf_oxide::PdfDocument, rich: &mut RichExtraction) {
    let page_count = doc.page_count().unwrap_or(0);
    if page_count == 0 {
        return;
    }

    // Analyze first page spans for title detection.
    match doc.extract_spans(0) {
        Ok(spans) => {
            if spans.is_empty() {
                return;
            }

            // Find the span with the largest font size on page 0 — strong title signal.
            let mut largest_font: f32 = 0.0;
            let mut title_text: Option<String> = None;
            let mut heading_count = 0usize;
            let mut spans_with_artifacts = 0usize;

            for span in &spans {
                if span.font_size > largest_font && !span.text.trim().is_empty() {
                    largest_font = span.font_size;
                    title_text = Some(span.text.clone());
                }
                if span.font_size >= 14.0
                    && matches!(span.font_weight, pdf_oxide::layout::FontWeight::Bold)
                {
                    heading_count += 1;
                }
                if span.artifact_type.is_some() {
                    spans_with_artifacts += 1;
                }
            }

            // Span title is a quality signal only — the LLM has the full text and can
            // extract a complete title. Setting rich.title from a truncated span would
            // incorrectly override the LLM's better extraction.
            // The largest-font text is still recorded in the quality signals below.
            let quality = format!(
                "Spans: page0_fonts={}, largest={:.1}pt, heading_candidates={}, artifact_spans={}",
                spans.len(),
                largest_font,
                heading_count,
                spans_with_artifacts
            );
            append_extra(rich, &quality);

            tracing::debug!(
                "Span analysis: {} spans, largest font {:.1}pt, {} heading candidates, title={:?}",
                spans.len(),
                largest_font,
                heading_count,
                title_text
                    .as_ref()
                    .map(|t| t.chars().take(60).collect::<String>())
            );
        }
        Err(e) => {
            tracing::debug!("Span extraction error on page 0: {}", e);
        }
    }
}

/// Page dimensions from media box (first page).
fn populate_page_box(doc: &pdf_oxide::PdfDocument, rich: &mut RichExtraction) {
    match doc.get_page_media_box(0) {
        Ok((llx, lly, urx, ury)) => {
            let width = urx - llx;
            let height = ury - lly;
            let orientation = if width > height {
                "landscape"
            } else {
                "portrait"
            };
            append_extra(
                rich,
                &format!("Page: {width:.0}x{height:.0}pt, {orientation}"),
            );
            tracing::debug!(
                "Page 0 media box: {:.0}x{:.0}pt {}",
                width,
                height,
                orientation
            );
        }
        Err(e) => tracing::debug!("media box error: {}", e),
    }
}

/// Tagged PDF / structure tree reliability signal.
fn populate_mark_info(doc: &pdf_oxide::PdfDocument, rich: &mut RichExtraction) {
    match doc.mark_info() {
        Ok(info) => {
            let reliable = info.is_structure_reliable();
            append_extra(
                rich,
                &format!(
                    "Tagged: marked={}, suspects={}, reliable={}",
                    info.marked, info.suspects, reliable
                ),
            );
        }
        Err(e) => tracing::debug!("mark_info error: {}", e),
    }
}

// =============================================================================
// Image extraction
// =============================================================================

fn extract_and_save_images(
    doc: &pdf_oxide::PdfDocument,
    paper_data_dir: Option<&Path>,
    rich: &mut RichExtraction,
    config: &PdfExtractionConfig,
) {
    let Some(base_dir) = paper_data_dir else {
        return;
    };
    if !config.extract_images {
        tracing::debug!("extract_images is disabled; skipping image extraction");
        return;
    }
    let page_count = doc.page_count().unwrap_or(0);
    if page_count == 0 {
        return;
    }

    let img_dir = base_dir.join("images");
    let out_format = crate::util::image::parse_image_format(&config.output_format);
    if let Err(e) = std::fs::create_dir_all(&img_dir) {
        tracing::warn!("failed to create image dir {:?}: {}", img_dir, e);
        return;
    }

    let mut fig_index = 0usize;
    let mut skipped_small = 0usize;
    let mut skipped_large = 0usize;
    let mut skipped_file_size = 0usize;
    let mut skipped_duplicate = 0usize;
    let mut seen_hashes: HashSet<String> = HashSet::new();

    for page_idx in 0..page_count {
        let images = match doc.extract_images(page_idx) {
            Ok(imgs) => imgs,
            Err(e) => {
                tracing::debug!("image extraction error page {}: {}", page_idx, e);
                continue;
            }
        };
        if images.is_empty() {
            continue;
        }

        // Get page dimensions for area-ratio filtering.
        let page_area = doc
            .get_page_media_box(page_idx)
            .map(|(llx, lly, urx, ury)| {
                let w = (urx - llx).abs() as f64;
                let h = (ury - lly).abs() as f64;
                w * h
            })
            .unwrap_or(0.0);

        for image in images {
            fig_index += 1;
            let tmp_filename = format!("fig{fig_index}.png");
            let tmp_filepath = img_dir.join(&tmp_filename);
            if let Err(e) = image.save_as_png(&tmp_filepath) {
                tracing::debug!("save image {}: {}", tmp_filepath.display(), e);
                continue;
            }

            // 1. File-size gate (catches tiny placeholders and huge blobs).
            let file_size = match std::fs::metadata(&tmp_filepath) {
                Ok(m) => m.len(),
                Err(_) => {
                    let _ = std::fs::remove_file(&tmp_filepath);
                    continue;
                }
            };
            if file_size < config.min_image_file_size_bytes {
                let _ = std::fs::remove_file(&tmp_filepath);
                skipped_file_size += 1;
                continue;
            }
            if file_size > config.max_image_file_size_bytes {
                let _ = std::fs::remove_file(&tmp_filepath);
                skipped_file_size += 1;
                continue;
            }

            // 2. Dimension gate (short side / long side / area ratio).
            let (w, h) = match crate::util::image::png_dimensions(&tmp_filepath) {
                Some(dims) => dims,
                None => {
                    let _ = std::fs::remove_file(&tmp_filepath);
                    continue;
                }
            };
            let short = w.min(h);
            let long = w.max(h);

            if short <= config.min_image_short_side {
                let _ = std::fs::remove_file(&tmp_filepath);
                skipped_small += 1;
                continue;
            }
            if long > config.max_image_long_side {
                // Also check area ratio to avoid discarding legitimate large figures.
                let img_area = (w as f64) * (h as f64);
                if page_area > 0.0 && img_area / page_area > 0.9 {
                    let _ = std::fs::remove_file(&tmp_filepath);
                    skipped_large += 1;
                    continue;
                }
            }

            // 3. Duplicate detection by content hash (same file size + quick hash).
            let hash = match compute_file_hash_quick(&tmp_filepath) {
                Some(h) => h,
                None => {
                    let _ = std::fs::remove_file(&tmp_filepath);
                    continue;
                }
            };
            if !seen_hashes.insert(hash) {
                let _ = std::fs::remove_file(&tmp_filepath);
                skipped_duplicate += 1;
                continue;
            }

            // 4. Optimize: resize + re-encode to configured format.
            let out_filepath = img_dir.join(format!("fig{fig_index}.tmp"));
            let stem = format!("fig{fig_index}");
            let image_path = match crate::util::image::optimize_image(
                &tmp_filepath,
                &out_filepath,
                config.output_max_long_side,
                config.output_quality,
                out_format,
            ) {
                Ok((optimized_size, actual_format)) => {
                    let placed = crate::util::image::place_optimized_image(
                        Ok((optimized_size, actual_format)),
                        &tmp_filepath,
                        &img_dir,
                        "images",
                        &stem,
                        out_format,
                        false,
                    );
                    let _ = std::fs::remove_file(&tmp_filepath);
                    tracing::debug!(
                        "Optimized fig{fig_index} from {} to {} bytes ({:?})",
                        file_size,
                        optimized_size,
                        actual_format
                    );
                    match placed {
                        Ok((_, rel_path)) => rel_path,
                        Err(e) => {
                            tracing::warn!("Failed to place optimized image fig{fig_index}: {e}");
                            format!("images/{tmp_filename}")
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Image optimization failed for fig{fig_index}: {e}; keeping raw PNG"
                    );
                    let _ = std::fs::remove_file(&out_filepath);
                    format!("images/{tmp_filename}")
                }
            };

            // pdf_oxide cannot reliably associate a caption with a raster image
            // (extraction order need not match figure numbering, and figures may
            // span pages or combine sub-panels), so figures carry no caption.
            rich.figures.push(MinerUFigure {
                caption: None,
                image_path: Some(image_path),
                page_number: Some(page_idx as u32 + 1),
            });
        }
    }

    if skipped_small > 0 {
        tracing::debug!(
            "skipped {} small images (short side <= {}px)",
            skipped_small,
            config.min_image_short_side
        );
    }
    if skipped_large > 0 {
        tracing::debug!("skipped {} large full-page images", skipped_large);
    }
    if skipped_file_size > 0 {
        tracing::debug!(
            "skipped {} images by file size ({}B - {}B)",
            skipped_file_size,
            config.min_image_file_size_bytes,
            config.max_image_file_size_bytes
        );
    }
    if skipped_duplicate > 0 {
        tracing::debug!("skipped {} duplicate images", skipped_duplicate);
    }
    if !rich.figures.is_empty() {
        tracing::info!(
            "extracted {} images from {} pages",
            rich.figures.len(),
            page_count
        );
    }
}

/// Compute a fast content hash for duplicate-image detection.
/// Uses the first 8 KB of the file to avoid hashing multi-MB images in full.
fn compute_file_hash_quick(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 8192];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(crate::util::sha256_hex(&buf))
}

/// Reject titles that are clearly non-titles (numeric IDs, too short, etc.).
/// CJK-aware: counts CJK characters as meaningful content.
fn is_plausible_title(s: &str, config: &PdfExtractionConfig) -> bool {
    let s = s.trim();
    if s.chars().count() < config.min_title_chars {
        return false;
    }
    let meaningful_count = s
        .chars()
        .filter(|c| c.is_alphanumeric() || crate::util::is_cjk(*c))
        .count();
    meaningful_count >= 2 && (meaningful_count as f64) >= (s.chars().count() as f64) * 0.1
}

fn max_outline_depth(items: &[pdf_oxide::OutlineItem], current: usize) -> usize {
    items.iter().fold(current, |max, item| {
        if item.children.is_empty() {
            max
        } else {
            max.max(max_outline_depth(&item.children, current + 1))
        }
    })
}

fn append_extra(rich: &mut RichExtraction, value: &str) {
    let _ = rich.extra.get_or_insert_with(String::new);
    if let Some(ref mut e) = rich.extra {
        if !e.is_empty() {
            *e += "; ";
        }
        *e += value;
    }
}

/// Render a PDF page from an already-open document — avoids reopening the
/// file for each figure when caller already holds a [`PdfDocument`] handle.
pub fn render_page_to_image_from_doc(
    doc: &pdf_oxide::PdfDocument,
    page_idx: usize,
    dpi: u32,
) -> Result<image::DynamicImage> {
    use image::{DynamicImage, RgbaImage};
    use pdf_oxide::rendering::{RenderOptions, render_page};

    let opts = RenderOptions::with_dpi(dpi).as_raw();
    let rendered = render_page(doc, page_idx, &opts).map_err(|e| {
        PaperedError::pdf_parse_with_source(format!("Failed to render PDF page: {e}"), e)
    })?;
    let img =
        RgbaImage::from_raw(rendered.width, rendered.height, rendered.data).ok_or_else(|| {
            PaperedError::io_other("Failed to construct image from rendered PDF page")
        })?;
    Ok(DynamicImage::ImageRgba8(img))
}

/// Per-paper index of normalized page texts. Build once per PDF and reuse
/// for all figure caption lookups — avoids re-opening the document.
pub struct PageTextIndex {
    /// Normalized page text (lowercase, alphanumeric, single spaces).
    texts: Vec<String>,
    /// Raw page text, kept for the caption-structure fallback in
    /// [`Self::locate_figure`] — normalization strips the delimiters
    /// ("|", ":", ".") that mark where a caption begins.
    raw_texts: Vec<String>,
    /// Character counts per page (from extract_text), for the sparse-page
    /// heuristic used by resolve_figure_page.
    char_counts: Vec<usize>,
    /// Whether each page carries at least one embedded raster image.
    page_has_image: Vec<bool>,
    /// Whether any page carries an embedded image. When false the document
    /// is likely vector-drawn (or image extraction is unsupported) and the
    /// image-presence gate in locate_figure is disabled.
    any_image: bool,
}

impl PageTextIndex {
    /// Build the index from an already-open document. Callers that hold a
    /// [`PdfDocument`] for rendering can reuse it here to avoid a second open.
    pub fn build_from_doc(doc: &pdf_oxide::PdfDocument) -> Self {
        let page_count = match doc.page_count() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("PageTextIndex: page_count failed ({e}); indexing 0 pages");
                0
            }
        };
        let mut texts = Vec::with_capacity(page_count);
        let mut raw_texts = Vec::with_capacity(page_count);
        let mut char_counts = Vec::with_capacity(page_count);
        let mut page_has_image = Vec::with_capacity(page_count);
        for page_idx in 0..page_count {
            let page_text = doc.extract_text(page_idx).unwrap_or_default();
            char_counts.push(page_text.len());
            texts.push(normalize_text(page_text.chars()));
            raw_texts.push(page_text);
            page_has_image.push(
                doc.extract_images(page_idx)
                    .is_ok_and(|imgs| !imgs.is_empty()),
            );
        }
        let any_image = page_has_image.iter().any(|&b| b);
        Self {
            texts,
            raw_texts,
            char_counts,
            page_has_image,
            any_image,
        }
    }

    /// Locate the page a figure appears on by matching the LLM caption
    /// against per-page text.
    ///
    /// Tier 1 — label-anchored context matching: find every whole-word
    /// occurrence of "figure n" / "fig n" and score how well the text
    /// *following* each occurrence matches the caption body. A real caption
    /// ("Figure 1. Overview of ...") carries the descriptive text right
    /// after the label and scores high; a body-text cross-reference
    /// ("as shown in Figure 1, the ...") scores low. Whole-word matching
    /// also keeps "figure 1" from matching "figure 10". Compound labels
    /// that already carry a figure word ("Extended Data Fig. 1") are
    /// additionally anchored bare — "fig extended data fig 1" would never
    /// match the caption prefix "Extended Data Fig. 1 |", and caption-only
    /// matching cannot separate it from a main figure with a near-identical
    /// caption.
    ///
    /// When the label occurs but no occurrence is followed by the caption
    /// text, a structural fallback anchors on the caption block itself in
    /// the raw page text: the label at a line start followed by a caption
    /// delimiter ("Fig. 3 | ...", "Figure 3: ..."). This recovers figures
    /// whose LLM-extracted caption was paraphrased or partly invented, so
    /// no substring of it survives in the document. If even that finds
    /// nothing, the figure exists only as an in-text reference (e.g. a
    /// supplementary figure cited but not included in the PDF) and `None`
    /// is returned — no image is rendered rather than a wrong page.
    ///
    /// Tier 2 — caption containment against full pages, used only when the
    /// label pattern is absent from every page (graphical abstracts, labels
    /// pdf_oxide cannot extract). Two-word needles ("Graphical abstract")
    /// match by exact phrase; anything shorter is too generic to trust.
    ///
    /// Scoring uses asymmetric character 4-gram containment, which stays
    /// high even when text extraction splits words with stray spaces
    /// ("Overvie w of the DREAM framew ork").
    ///
    /// Medium-confidence text matches additionally require an embedded
    /// image on or next to the matched page; documents with no embedded
    /// images at all (vector-drawn figures) skip this gate.
    ///
    /// Returns the 0-based page index (before resolve_figure_page adjustment).
    pub fn locate_figure(&self, caption: &str, label: &str) -> Option<usize> {
        let needle = caption_needle(caption);

        // Tier 1: label-anchored context matching.
        let mut patterns: Vec<String> = ["figure ", "fig "]
            .iter()
            .map(|prefix| normalize_text(format!("{prefix}{label}").chars()))
            .collect();
        let bare = normalize_text(label.chars());
        if bare.split_whitespace().any(|w| w == "fig" || w == "figure") && !patterns.contains(&bare)
        {
            patterns.push(bare);
        }
        let mut best: Option<(usize, f32)> = None;
        let mut label_found = false;
        for pat in &patterns {
            if pat.split_whitespace().count() < 2 {
                continue; // label normalized to empty
            }
            for (page_idx, page_text) in self.texts.iter().enumerate() {
                for occ in find_label_occurrences(page_text, pat) {
                    label_found = true;
                    let window = caption_window(page_text, occ + pat.len());
                    let score = ngram_containment(&needle, &window);
                    if self.accept_page(page_idx, score).is_some()
                        && best.is_none_or(|(_, s)| score > s)
                    {
                        best = Some((page_idx, score));
                    }
                }
            }
        }
        if label_found {
            if let Some((page, _)) = best {
                return Some(page);
            }
            // The label exists but no occurrence is followed by the
            // caption text — the LLM likely paraphrased or invented it.
            // Anchor on caption structure in the raw text instead.
            return self.locate_by_caption_structure(&needle, label);
        }

        // Tier 2: caption containment against full pages.
        if needle.split_whitespace().count() >= 2 {
            let mut best: Option<(usize, f32)> = None;
            for (page_idx, page_text) in self.texts.iter().enumerate() {
                let score = ngram_containment(&needle, page_text);
                if self.accept_page(page_idx, score).is_some()
                    && best.is_none_or(|(_, s)| score > s)
                {
                    best = Some((page_idx, score));
                }
                if score >= 0.95 {
                    break;
                }
            }
            return best.map(|(page, _)| page);
        }
        None
    }

    /// Last-resort anchor for paraphrased LLM captions: find pages where
    /// the label introduces a caption block in the raw text — the label at
    /// a line start, immediately followed by a caption delimiter ("|",
    /// ":", ".", "—", "–") and substantial text. Cross-references never
    /// take that shape: "(Fig. 3)" fails the line-start test and
    /// "Fig. 3b shows" fails the delimiter test. The structural evidence
    /// outweighs the image-presence proxy, so matches are accepted without
    /// the medium-band image gate; the needle score only ranks candidates.
    fn locate_by_caption_structure(&self, needle: &str, label: &str) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (page_idx, raw) in self.raw_texts.iter().enumerate() {
            for start in caption_prefix_offsets(raw, label) {
                let window_text = normalize_text(raw[start..].chars());
                let window = caption_window(&window_text, 0);
                if window.split_whitespace().count() < 10 {
                    continue; // a delimiter with nothing after it is no caption
                }
                let score = ngram_containment(needle, &window);
                if best.is_none_or(|(_, s)| score > s) {
                    best = Some((page_idx, score));
                }
            }
        }
        best.map(|(page, _)| page)
    }

    /// Decide whether a text match is strong enough to render `page`.
    fn accept_page(&self, page: usize, score: f32) -> Option<usize> {
        // Verbatim (or near-verbatim) caption match — trust the text alone.
        const HIGH_SCORE: f32 = 0.8;
        // Below this the match is more likely a cross-reference paraphrase.
        const MIN_SCORE: f32 = 0.5;
        if score >= HIGH_SCORE {
            return Some(page);
        }
        if score < MIN_SCORE {
            return None;
        }
        // Medium confidence: require physical evidence of an image on or
        // next to the page (±1 covers full-page-figure layouts). Documents
        // with no embedded images at all skip the gate — their figures are
        // likely vector-drawn.
        if !self.any_image || self.image_near(page) {
            Some(page)
        } else {
            None
        }
    }

    /// Whether any page in `page ± 1` carries an embedded image.
    fn image_near(&self, page: usize) -> bool {
        let lo = page.saturating_sub(1);
        let hi = (page + 1).min(self.page_has_image.len().saturating_sub(1));
        (lo..=hi).any(|i| self.page_has_image.get(i).copied().unwrap_or(false))
    }

    /// After locating a caption on page N, check whether page N-1 is nearly
    /// empty (typical full-page-figure layout: image on N, caption on N+1).
    /// Returns the page to actually render: N-1 if a figure-only page is
    /// detected, otherwise N.
    pub fn resolve_figure_page(&self, caption_page: usize) -> usize {
        const SPARSE_CHAR_THRESHOLD: usize = 300;
        if caption_page == 0 {
            return caption_page;
        }
        let char_count = self.char_counts.get(caption_page - 1).copied().unwrap_or(0);
        if char_count < SPARSE_CHAR_THRESHOLD {
            caption_page - 1
        } else {
            caption_page
        }
    }
}

/// Build the normalized matching needle from a caption: the caption body
/// (label prefix stripped), cut at whole words to ~130 chars. Short first
/// segments ("Graphical abstract.", "(A) Architecture.") fall through to
/// the rest of the caption instead of standing alone.
fn caption_needle(caption: &str) -> String {
    const NEEDLE_MAX_CHARS: usize = 130;
    let body = strip_figure_prefix(caption);
    let words: Vec<&str> = body
        .split_whitespace()
        .scan(0usize, |len, word| {
            *len += word.len() + 1;
            (*len <= NEEDLE_MAX_CHARS).then_some(word)
        })
        .collect();
    normalize_text(words.join(" ").chars())
}

/// Normalize text for matching: lowercase, alphanumeric only, whitespace
/// collapsed to single spaces — so label patterns match across line breaks
/// in extracted page text.
fn normalize_text(s: impl IntoIterator<Item = char>) -> String {
    let raw: String = s
        .into_iter()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Byte offsets of every whole-word occurrence of `pat` in `text`.
/// Whole-word means not adjacent to an ASCII alphanumeric, so "figure 1"
/// does not match inside "figure 10".
fn find_label_occurrences(text: &str, pat: &str) -> Vec<usize> {
    let mut out = Vec::new();
    if pat.is_empty() {
        return out;
    }
    let mut start = 0usize;
    while let Some(rel) = text[start..].find(pat) {
        let abs = start + rel;
        let end = abs + pat.len();
        let before_ok = text[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let after_ok = text[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if before_ok && after_ok {
            out.push(abs);
        }
        start = end;
    }
    out
}

/// The words following a label occurrence — up to `CAPTION_WINDOW_WORDS`
/// words, approximating the caption span (the descriptive text directly
/// after the label in a real caption).
fn caption_window(text: &str, start: usize) -> String {
    const CAPTION_WINDOW_WORDS: usize = 40;
    text[start..]
        .split_whitespace()
        .take(CAPTION_WINDOW_WORDS)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Byte offsets of caption-text starts introduced by `label` in raw page
/// text: the label's alphanumeric tokens at a line start (only whitespace
/// since the previous newline), optionally preceded by a "figure"/"fig"
/// token when the label carries no figure word itself, immediately
/// followed by a caption delimiter ("|", ":", ".", "—", "–"). Tokens
/// must match whole alphanumeric runs, so "fig 3" does not match
/// "Fig. 3a"; cross-references like "(Fig. 3)" additionally fail the
/// line-start test.
fn caption_prefix_offsets(raw: &str, label: &str) -> Vec<usize> {
    const DELIMS: [char; 5] = ['|', ':', '.', '\u{2014}', '\u{2013}'];

    let label_tokens: Vec<String> = label
        .split_whitespace()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|t| !t.is_empty())
        .collect();
    if label_tokens.is_empty() {
        return Vec::new();
    }
    let has_figure_word = label_tokens.iter().any(|t| *t == "fig" || *t == "figure");
    let seqs: Vec<Vec<String>> = if has_figure_word {
        vec![label_tokens]
    } else {
        ["figure", "fig"]
            .iter()
            .map(|intro| {
                let mut s = vec![intro.to_string()];
                s.extend(label_tokens.clone());
                s
            })
            .collect()
    };

    // Alphanumeric runs of the raw text as (start, end) byte offsets.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let (mut run_start, mut in_run) = (0usize, false);
    for (i, c) in raw.char_indices() {
        if c.is_ascii_alphanumeric() {
            if !in_run {
                run_start = i;
                in_run = true;
            }
        } else if in_run {
            runs.push((run_start, i));
            in_run = false;
        }
    }
    if in_run {
        runs.push((run_start, raw.len()));
    }

    let mut offsets = Vec::new();
    for seq in &seqs {
        if runs.len() < seq.len() {
            continue;
        }
        for i in 0..=runs.len() - seq.len() {
            let matched = seq.iter().enumerate().all(|(k, tok)| {
                let (a, b) = runs[i + k];
                raw[a..b].eq_ignore_ascii_case(tok)
            });
            if !matched {
                continue;
            }
            let first_start = runs[i].0;
            let last_end = runs[i + seq.len() - 1].1;
            // Line start: only whitespace since the previous newline.
            let line_start = raw[..first_start].rfind('\n').map(|p| p + 1).unwrap_or(0);
            if !raw[line_start..first_start]
                .chars()
                .all(|c| c.is_whitespace())
            {
                continue;
            }
            // Caption delimiter right after the label (spaces allowed).
            let after = raw[last_end..].trim_start_matches([' ', '\t']);
            let Some(delim) = after.chars().next() else {
                continue;
            };
            if DELIMS.contains(&delim) {
                offsets.push(raw.len() - after.len() + delim.len_utf8());
            }
        }
    }
    offsets
}

/// Fraction of the needle's character 4-grams present in the haystack
/// ([0, 1]). Asymmetric on purpose: pages and caption windows are much
/// longer than the caption needle, so symmetric Jaccard stays near zero
/// even when the caption appears verbatim. Character n-grams (rather than
/// word bigrams) tolerate text-extraction artifacts that split words with
/// stray spaces ("Overvie w of the DREAM framew ork", "st ate-of-the-art")
/// — the character stream still matches almost everywhere around each
/// break. Needles shorter than 3 words fall back to an exact substring
/// check (a 2-word phrase is specific; anything shorter is too generic).
fn ngram_containment(needle: &str, haystack: &str) -> f32 {
    const N: usize = 4;
    if needle.is_empty() {
        return 0.0;
    }
    if needle.split_whitespace().count() < 3 {
        return if haystack.contains(needle) { 1.0 } else { 0.0 };
    }
    let n_chars: Vec<char> = needle.chars().collect();
    if n_chars.len() < N {
        return if haystack.contains(needle) { 1.0 } else { 0.0 };
    }
    let n_grams: std::collections::HashSet<[char; N]> = n_chars
        .windows(N)
        .map(|w| [w[0], w[1], w[2], w[3]])
        .collect();
    let h_chars: Vec<char> = haystack.chars().collect();
    let h_grams: std::collections::HashSet<[char; N]> = h_chars
        .windows(N)
        .map(|w| [w[0], w[1], w[2], w[3]])
        .collect();
    let hits = n_grams.intersection(&h_grams).count();
    hits as f32 / n_grams.len() as f32
}

/// Strip "Figure N." / "Fig. N." / "Supplementary Figure N." prefix from a
/// caption (case-insensitive), returning the descriptive body text.
/// Returns the original if no prefix is recognized.
fn strip_figure_prefix(caption: &str) -> &str {
    let s = caption.trim_start();
    let lower = s.to_ascii_lowercase();
    for prefix in ["supplementary figure ", "figure ", "fig. "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let num_end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(rest.len());
            if num_end > 0 {
                return s[prefix.len() + num_end..].trim_start_matches(['.', ':', ' ']);
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an index from raw page texts (no PDF needed).
    fn index_of(pages: &[&str], pages_with_image: &[usize]) -> PageTextIndex {
        let page_has_image: Vec<bool> = (0..pages.len())
            .map(|i| pages_with_image.contains(&i))
            .collect();
        let any_image = page_has_image.iter().any(|&b| b);
        PageTextIndex {
            texts: pages.iter().map(|p| normalize_text(p.chars())).collect(),
            raw_texts: pages.iter().map(|p| p.to_string()).collect(),
            char_counts: pages.iter().map(|p| p.len()).collect(),
            page_has_image,
            any_image,
        }
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(
            normalize_text("Figure  1.\n\tOverview".chars()),
            "figure 1 overview"
        );
    }

    #[test]
    fn label_occurrences_respect_word_boundaries() {
        let text =
            normalize_text("Figure 10 shows results. Figure 1 overview. prefigure 1".chars());
        let occ = find_label_occurrences(&text, "figure 1");
        // Only the standalone "figure 1" matches — not "figure 10" nor
        // "prefigure 1".
        assert_eq!(occ.len(), 1);
        assert_eq!(&text[occ[0]..occ[0] + 8], "figure 1");
    }

    #[test]
    fn caption_page_beats_cross_reference_page() {
        let pages = [
            "As shown in Figure 1, the framework achieves state of the art results across all benchmarks we evaluate in this work",
            "unrelated introduction text about the history of document analysis systems and their many limitations",
            "Figure 1. Overview of the proposed framework with three parallel extraction paths for PDF documents",
        ];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure(
            "Figure 1. Overview of the proposed framework with three parallel extraction paths for PDF documents.",
            "1",
        );
        assert_eq!(page, Some(2));
    }

    #[test]
    fn figure_1_does_not_match_figure_10_page() {
        let pages = ["Figure 10. Performance comparison across five datasets with error bars"];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure("Figure 1. Architecture diagram of the encoder layers.", "1");
        assert_eq!(page, None);
    }

    #[test]
    fn cited_only_figure_is_rejected() {
        // The label occurs (cross-reference) but the caption text never
        // follows it — the figure is not actually contained in the PDF.
        let pages = [
            "Supplementary Figure S1 shows additional results for experiment B, see Appendix A for the full tables",
            "We further validate our approach on three additional cohorts as described in the methods section above",
        ];
        let idx = index_of(&pages, &[0, 1]);
        let page = idx.locate_figure(
            "Figure S1. Western blot analysis of protein expression levels in treated cells.",
            "S1",
        );
        assert_eq!(page, None);
    }

    #[test]
    fn verbatim_caption_accepted_without_image() {
        // High-confidence (verbatim) matches pass even in image-free
        // documents — vector-drawn figures still get their page.
        let pages =
            ["Figure 3. Architecture of the dual path extraction pipeline with layout analysis"];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure(
            "Figure 3. Architecture of the dual path extraction pipeline with layout analysis.",
            "3",
        );
        assert_eq!(page, Some(0));
    }

    #[test]
    fn medium_score_requires_nearby_image() {
        // The page text paraphrases the caption and the label sits
        // mid-line (a cross-reference shape, so the caption-structure
        // fallback cannot fire): containment lands in the medium band
        // and the image-presence gate decides.
        let citing = "All components were ablated as illustrated in Figure 2. Results of the ablation study on model components are reported in this section with details";
        let caption = "Figure 2. Results of the ablation study on model components and training configurations.";

        // Image on the matched page → accepted.
        let idx = index_of(
            &[citing, "dense text page with no images at all just words"],
            &[0],
        );
        assert_eq!(idx.locate_figure(caption, "2"), Some(0));

        // Images exist elsewhere but not near the match → rejected.
        let idx = index_of(
            &[citing, "dense text page", "another dense text page"],
            &[2],
        );
        assert_eq!(idx.locate_figure(caption, "2"), None);

        // No embedded images anywhere (vector document) → gate disabled.
        let idx = index_of(&[citing, "dense text page"], &[]);
        assert_eq!(idx.locate_figure(caption, "2"), Some(0));
    }

    #[test]
    fn tier2_caption_containment_when_label_absent() {
        // Label pattern absent from every page (e.g. graphical abstract) —
        // full-page caption containment locates the page.
        let pages = [
            "title page with abstract and author information only",
            "graphical abstract summary of the entire pipeline from input documents to structured output",
        ];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure(
            "Graphical abstract. Summary of the entire pipeline from input documents to structured output.",
            "graphical_abstract",
        );
        assert_eq!(page, Some(1));
    }

    #[test]
    fn two_word_caption_located_via_tier2() {
        // Real-world regression: the whole caption is just "Graphical
        // abstract" (2 words) — located by exact phrase on the page.
        let pages = [
            "title page with author information and the paper abstract text",
            "graphical abstract",
        ];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure("Graphical abstract", "graphical_abstract");
        assert_eq!(page, Some(1));
    }

    #[test]
    fn one_word_needle_is_too_generic_for_tier2() {
        let pages = ["overview", "something else entirely"];
        let idx = index_of(&pages, &[]);
        assert_eq!(idx.locate_figure("Overview", "overview"), None);
    }

    #[test]
    fn ngram_containment_tolerates_split_words() {
        // PDF text extraction often splits words with stray spaces;
        // character 4-grams still recognize the caption.
        let score = ngram_containment(
            "overview of the dream framework dream comprises two integral modules the state of the art senet that models enhancer activity",
            "overvie w of the dream framew ork dream comprises two integral modules the st ate of the art senet that models enhancer activit y",
        );
        assert!(score > 0.8, "score was {score}");
    }

    #[test]
    fn split_word_caption_page_is_located() {
        // Real-world regression: Figure 1's caption page extracts with
        // broken words, and the figure is vector-drawn (no embedded image).
        let pages = [
            "body text referencing Figure 1 elsewhere in the paper discusses enhancer activity prediction results",
            "Figure 1. Overvie w of the DREAM framew ork. DREAM comprises two integral modules: the st ate-of-the-art SENet that models enhancer activit y using DNA sequences as the input",
        ];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure(
            "Figure 1. Overview of the DREAM framework. DREAM comprises two integral modules: the state-of-the-art SENet that models enhancer activity using DNA sequences as the input.",
            "1",
        );
        assert_eq!(page, Some(1));
    }

    #[test]
    fn strip_prefix_is_case_insensitive() {
        assert_eq!(
            strip_figure_prefix("FIGURE 2. The main results."),
            "The main results."
        );
        assert_eq!(strip_figure_prefix("Fig. S3: Extra data."), "Extra data.");
        assert_eq!(
            strip_figure_prefix("Supplementary Figure 4. More data."),
            "More data."
        );
    }

    #[test]
    fn compound_label_anchors_on_bare_label() {
        // Real-world regression: Extended Data Fig. 1 shares its caption
        // with Fig. 1 almost verbatim, so caption-only matching picks the
        // wrong page; the bare-label anchor separates the two pages
        // ("fig extended data fig 1" matches nothing in the text).
        let pages = [
            "Fig. 1 | Overview of model architecture, training procedure, datasets and evaluation",
            "body text referencing the models and their evaluation results",
            "Extended Data Fig. 1 | Overview of model architecture, training procedure, datasets, and evaluation metrics",
        ];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure(
            "Overview of model architecture, training procedure, datasets, and evaluation metrics",
            "Extended Data Fig. 1",
        );
        assert_eq!(page, Some(2));
    }

    #[test]
    fn paraphrased_caption_falls_back_to_caption_structure() {
        // Real-world regression: the LLM paraphrased the caption —
        // "Computing likelihood ratios" appears nowhere in the PDF — so
        // no substring match survives. The line-start "Fig. 3 |"
        // structure still anchors the caption page.
        let pages = [
            "We compared models across variant classes (Fig. 3b). Pathogenic variants were evaluated in coding regions",
            "Fig. 3 | Evo 2 enables accurate zero-shot human variant effect prediction. a, Overview of zero-shot variant effect prediction using Evo 2 to assign likelihood scores to human genetic variants",
        ];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure(
            "Human variant effect prediction. a, Computing likelihood ratios for single nucleotide variants and predicting pathogenicity across diverse variant classes with deep learning models",
            "3",
        );
        assert_eq!(page, Some(1));
    }

    #[test]
    fn cross_reference_does_not_trigger_structure_fallback() {
        // "(Fig. 3)." in running text fails the line-start test and
        // "Fig. 3b shows" fails the delimiter test — neither is a
        // caption block, so a figure with no real caption stays None.
        let pages = [
            "pathogenic variants were identified in the cohort (Fig. 3). We next evaluated the model on coding regions and noncoding regions across the genome",
            "Fig. 3b shows the zero-shot evaluation results we obtained for coding and noncoding variant classes in this study",
        ];
        let idx = index_of(&pages, &[]);
        let page = idx.locate_figure(
            "Something entirely different about protein structure and function prediction with language models",
            "3",
        );
        assert_eq!(page, None);
    }
}
