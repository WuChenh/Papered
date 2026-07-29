//! Figure indexing — multimodal embedding and LLM description.

use crate::error::Result;
use crate::index::indexer::helpers;
use crate::store::vector::VectorRecord;
use futures_util::future::join_all;
use std::path::Path;

impl super::Indexer {
    pub(super) async fn index_figures_and_tables(
        &self,
        paper_id: &str,
        rich: &crate::paper::parser::RichExtraction,
    ) -> Result<()> {
        let mut figures: Vec<crate::index::multimodal::FigureInfo> = Vec::new();
        for (i, fig) in rich.figures.iter().enumerate() {
            let info = crate::index::multimodal::FigureInfo {
                id: format!("{}_fig{}", paper_id, i + 1),
                paper_id: paper_id.to_string(),
                caption: fig.caption.clone(),
                description: None,
                image_path: fig.image_path.clone(),
                page_number: fig.page_number,
                bbox: None,
                figure_label: None,
            };
            figures.push(info);
        }
        self.embed_describe_and_store(paper_id, true, &mut figures)
            .await
    }

    /// Index figures extracted by the focused figure-extraction LLM call
    /// (figure_extraction_prompt.txt). Two-source figure indexing: LLM
    /// metadata (label + caption) + pdf_oxide (page location + rendering).
    /// No MinerU dependency.
    pub(super) async fn index_llm_figures(
        &self,
        pdf_path: &Path,
        paper: &crate::paper::Paper,
        llm_figures: &[crate::paper::parser::LlmFigure],
        paper_data_dir: &Path,
    ) -> Result<()> {
        use crate::paper::pdf_oxide;
        use image::imageops::FilterType;

        const FIGURE_DPI: u32 = 200;
        const MAX_LONG_SIDE: u32 = 2000;

        let figures_dir = paper_data_dir.join("figures");
        std::fs::create_dir_all(&figures_dir).map_err(|e| {
            crate::PaperedError::io_other(format!(
                "Failed to create figures dir {}: {e}",
                figures_dir.display()
            ))
        })?;

        // Open the PDF once — PageTextIndex and all page renders share this handle.
        let pdf_doc = match pdf_oxide::PdfDocument::open(pdf_path) {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!(
                    paper_id = %paper.id,
                    "Could not open PDF for figure extraction ({e}); images will be missing"
                );
                None
            }
        };
        let page_index = pdf_doc
            .as_ref()
            .map(pdf_oxide::PageTextIndex::build_from_doc);
        let mut located_count = 0u32;
        let mut missing_count = 0u32;

        let mut figures: Vec<crate::index::multimodal::FigureInfo> = Vec::new();
        let mut used_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for llm_fig in llm_figures {
            let raw_label = sanitize_label(&llm_fig.label);
            let fig_id = if raw_label.is_empty() {
                // Label sanitized to empty — fall back to sequential index.
                let mut n = figures.len() + 1;
                loop {
                    let candidate = format!("{}_fig{}", paper.id, n);
                    if !used_ids.contains(&candidate) {
                        break candidate;
                    }
                    n += 1;
                }
            } else {
                let candidate = format!("{}_fig_{}", paper.id, raw_label);
                if used_ids.contains(&candidate) {
                    // Sanitized-label collision — append disambiguation suffix.
                    let mut suffix = 2;
                    loop {
                        let alt = format!("{}_{}", candidate, suffix);
                        if !used_ids.contains(&alt) {
                            break alt;
                        }
                        suffix += 1;
                    }
                } else {
                    candidate
                }
            };
            used_ids.insert(fig_id.clone());

            let render_page = page_index.as_ref().and_then(|pi| {
                pi.locate_figure(&llm_fig.caption, &llm_fig.label)
                    .map(|p| pi.resolve_figure_page(p))
            });
            if render_page.is_some() {
                located_count += 1;
            } else if page_index.is_some() {
                missing_count += 1;
            }

            let img_path = if let (Some(page), Some(doc)) = (render_page, pdf_doc.as_ref()) {
                match pdf_oxide::render_page_to_image_from_doc(doc, page, FIGURE_DPI) {
                    Ok(img) => {
                        let resized = if img.width() > MAX_LONG_SIDE || img.height() > MAX_LONG_SIDE
                        {
                            let (w, h) = (img.width(), img.height());
                            let (new_w, new_h) = if w > h {
                                (MAX_LONG_SIDE, (h * MAX_LONG_SIDE / w).max(1))
                            } else {
                                ((w * MAX_LONG_SIDE / h).max(1), MAX_LONG_SIDE)
                            };
                            img.resize_exact(new_w, new_h, FilterType::Lanczos3)
                        } else {
                            img
                        };
                        let rel_path = format!("figures/{fig_id}.jpg");
                        let dest = paper_data_dir.join(&rel_path);
                        match resized.to_rgb8().save(&dest) {
                            Ok(()) => {
                                tracing::debug!(
                                    fig_id = %fig_id,
                                    "Rendered figure page to {}",
                                    dest.display()
                                );
                                Some(rel_path)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to save figure image {}: {}",
                                    dest.display(),
                                    e
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to render page for figure {}: {}", fig_id, e);
                        None
                    }
                }
            } else {
                None
            };

            figures.push(crate::index::multimodal::FigureInfo {
                id: fig_id,
                paper_id: paper.id.clone(),
                caption: Some(llm_fig.caption.clone()),
                description: None,
                image_path: img_path,
                // 1-based page the stored image was rendered from.
                page_number: render_page.map(|p| p as u32 + 1),
                bbox: None,
                figure_label: Some(llm_fig.label.clone()),
            });
        }

        if page_index.is_none() {
            tracing::warn!(
                paper_id = %paper.id,
                "PageTextIndex::build_from_doc unavailable — pdf_oxide could not open PDF for caption matching. Figure images will be missing."
            );
        } else if missing_count > 0 {
            tracing::warn!(
                paper_id = %paper.id,
                located = located_count,
                missing = missing_count,
                "Could not locate {missing_count} of {} figure captions in PDF pages; those figures will have no images.",
                located_count + missing_count
            );
        } else {
            tracing::info!(
                paper_id = %paper.id,
                count = located_count,
                "Located all {} figure captions in PDF pages",
                located_count
            );
        }

        // LLM figures: text-only caption embedding (full-page renders are
        // dominated by surrounding text — no value in multimodal embed).
        // No separate LLM description — the extracted caption is already clean.
        self.embed_describe_and_store(
            &paper.id,
            false, // skip LLM description
            &mut figures,
        )
        .await
    }

    /// Shared pipeline: embed figure captions (text-only) into vectors,
    /// optionally generate LLM descriptions, then persist to store.
    async fn embed_describe_and_store(
        &self,
        paper_id: &str,
        describe: bool,
        figures: &mut [crate::index::multimodal::FigureInfo],
    ) -> Result<()> {
        let mut vector_records: Vec<VectorRecord> = Vec::new();
        let has_figures = !figures.is_empty();
        let emb_client = self.embedding.clone();

        let fig_embed_future = async {
            if !has_figures {
                return Vec::new();
            }
            let mut futures = Vec::new();
            for (i, fig) in figures.iter().enumerate() {
                let text = fig.caption.as_deref().unwrap_or("").to_string();
                let id = fig.id.clone();
                let client = emb_client.clone();
                futures.push(async move {
                    match client.embed_single(&text).await {
                        Ok(r) => Some((text, i, r.embedding)),
                        Err(e) => {
                            tracing::debug!("embed_text failed for {}: {}", id, e);
                            None
                        }
                    }
                });
            }
            join_all(futures).await.into_iter().flatten().collect()
        };

        let describe_future = async {
            if describe {
                self.describe_all_figures(figures).await
            } else {
                Ok(figures.iter().map(|_| None).collect())
            }
        };

        let (fig_embeddings, desc_result) = tokio::join!(fig_embed_future, describe_future);

        for (text, fig_idx, embedding) in fig_embeddings {
            if fig_idx < figures.len() {
                vector_records.push(VectorRecord::figure(
                    paper_id.to_string(),
                    figures[fig_idx].id.clone(),
                    embedding,
                    text,
                ));
            }
        }

        match desc_result {
            Ok(fig_descs) => {
                for (i, desc) in fig_descs.into_iter().enumerate() {
                    if i < figures.len() {
                        figures[i].description = desc;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(paper_id = %paper_id, error = %e, "Failed to batch-describe figures");
            }
        }

        if !figures.is_empty() {
            self.store.insert_figures(paper_id, figures).await?;
        }

        if !vector_records.is_empty() {
            self.store.upsert(&vector_records).await?;
        }

        Ok(())
    }

    async fn describe_all_figures(
        &self,
        figures: &[crate::index::multimodal::FigureInfo],
    ) -> Result<Vec<Option<String>>> {
        if self.llm_client.is_none() {
            return Ok(figures.iter().map(|_| None).collect());
        }

        const MAX_PROMPT_BODY_CHARS: usize = 8_000;
        let prompt_body = build_description_prompt_body(figures);
        if prompt_body.len() <= MAX_PROMPT_BODY_CHARS {
            return self.describe_single_batch(figures, &prompt_body).await;
        }

        // Split into smaller batches to stay within LLM context window
        const BATCH_SIZE: usize = 15;
        let mut fig_descs: Vec<Option<String>> = vec![None; figures.len()];

        let fig_chunks: Vec<(usize, &[crate::index::multimodal::FigureInfo])> = figures
            .chunks(BATCH_SIZE)
            .enumerate()
            .map(|(ci, chunk)| {
                let start_idx = ci * BATCH_SIZE;
                (start_idx, chunk)
            })
            .collect();

        for (fig_start, fig_chunk) in &fig_chunks {
            let body = build_description_prompt_body(fig_chunk);
            match self.describe_single_batch(fig_chunk, &body).await {
                Ok(fds) => {
                    for (i, desc) in fds.into_iter().enumerate() {
                        let target = fig_start + i;
                        if target < fig_descs.len() {
                            fig_descs[target] = desc;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to describe figures batch at index {}", fig_start);
                }
            }
        }

        Ok(fig_descs)
    }

    async fn describe_single_batch(
        &self,
        figures: &[crate::index::multimodal::FigureInfo],
        prompt_body: &str,
    ) -> Result<Vec<Option<String>>> {
        const MAX_TOKENS: usize = 2048;
        let mut prompt = String::new();
        prompt.push_str(prompt_body);
        prompt.push_str(
            "Respond with ONLY valid JSON. The JSON must be an object with these keys:\n",
        );
        if !figures.is_empty() {
            prompt.push_str("  \"figures\": array with one string per figure (index order)\n");
        }

        let response = self.llm_generate(&prompt, MAX_TOKENS).await?;
        let parsed = parse_description_response(&response);

        let fig_descs: Vec<Option<String>> = if !figures.is_empty() {
            parsed
                .get("figures")
                .and_then(|v| v.as_array())
                .map_or_else(
                    || figures.iter().map(|_| None).collect(),
                    |arr| arr.iter().map(opt_string).collect(),
                )
        } else {
            Vec::new()
        };

        Ok(fig_descs)
    }

    async fn llm_generate(&self, prompt: &str, max_tokens: usize) -> Result<String> {
        if let Some(ref client) = self.llm_client {
            // Cached client already has rate limiting attached.
            // Force JSON output so the response can be parsed reliably.
            client.generate_json("", prompt, max_tokens, 0.1).await
        } else {
            let section_endpoint = self.config.resolve_model(&self.config.purposes.section)?;
            let client = crate::llm::client::LlmClient::from_config(
                &section_endpoint,
                self.rate_limiter.clone(),
            )?;
            client.generate_json("", prompt, max_tokens, 0.1).await
        }
    }
}

fn build_description_prompt_body(figures: &[crate::index::multimodal::FigureInfo]) -> String {
    let mut prompt =
        String::from("Describe each figure from this research paper in 1-2 sentences. ");
    prompt.push_str("Focus on what data, method, or result each illustrates.\n\n");

    if !figures.is_empty() {
        prompt.push_str("Figures:\n");
        for (i, fig) in figures.iter().enumerate() {
            let caption = fig.caption.as_deref().unwrap_or("No caption");
            prompt.push_str(&format!("  [FIG {i}] Caption: {caption}\n"));
        }
        prompt.push('\n');
    }
    prompt
}

fn parse_description_response(response: &str) -> serde_json::Value {
    helpers::try_parse_llm_json(response).unwrap_or_else(|e| {
        tracing::debug!("Failed to parse figure description JSON: {e}");
        serde_json::json!({})
    })
}

fn opt_string(v: &serde_json::Value) -> Option<String> {
    v.as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Sanitize an LLM-extracted figure label for use in file names and IDs.
/// Replaces any character that isn't alphanumeric, hyphen, or underscore with
/// an underscore, collapsing consecutive underscores in a single pass —
/// including literal underscores already present in the input.
fn sanitize_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_underscore = false;
    for c in raw.chars() {
        if c == '_' {
            // Literal underscores collapse the same way replacements do.
            if !prev_underscore {
                out.push('_');
                prev_underscore = true;
            }
        } else if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c);
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_label_collapses_literal_underscores() {
        assert_eq!(sanitize_label("a__b"), "a_b");
        assert_eq!(sanitize_label("a__b__c"), "a_b_c");
        // Mixed literal and replacement underscores collapse together.
        assert_eq!(sanitize_label("fig _1"), "fig_1");
    }

    #[test]
    fn sanitize_label_replaces_special_chars() {
        assert_eq!(sanitize_label("Fig 1a"), "Fig_1a");
        assert_eq!(sanitize_label("fig. 2"), "fig_2");
        assert_eq!(
            sanitize_label("Extended Data Fig. 3"),
            "Extended_Data_Fig_3"
        );
        // Leading/trailing underscores are trimmed.
        assert_eq!(sanitize_label("_a_"), "a");
        assert_eq!(sanitize_label("!!!"), "");
        // Hyphens and alphanumerics pass through.
        assert_eq!(sanitize_label("S1-fig"), "S1-fig");
    }
}
