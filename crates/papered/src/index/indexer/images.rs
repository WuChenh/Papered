//! Image indexing — standalone image files using multimodal or text embeddings.

use crate::error::{PaperedError, Result};
use crate::paper::{Paper, PaperSource, PaperStatus};
use crate::store::vector::VectorRecord;
use std::path::Path;

impl super::Indexer {
    /// Index a standalone image file using multimodal embedding.
    ///
    /// Copies the image to the paper data directory, generates an LLM vision
    /// description (if available), embeds via multimodal or text embedding,
    /// and stores as a `content_type = "image"` vector record.
    pub(crate) async fn index_image(
        &self,
        path: &Path,
        paper_id: &str,
        paper_data_dir: &Path,
        file_hash: &str,
    ) -> Result<Paper> {
        let filename = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled Image");

        let out_format =
            crate::util::image::parse_image_format(&self.config.pdf_extraction.output_format);
        let tmp_path = paper_data_dir.join(format!("{filename}.tmp"));

        tokio::fs::copy(path, &tmp_path).await.map_err(|e| {
            PaperedError::io_other(format!(
                "Failed to copy image {} to {}: {}",
                path.display(),
                tmp_path.display(),
                e
            ))
        })?;

        let optimize_result = tokio::task::spawn_blocking({
            let src = tmp_path.clone();
            let dst = tmp_path.clone();
            let max_side = self.config.pdf_extraction.output_max_long_side;
            let quality = self.config.pdf_extraction.output_quality;
            move || crate::util::image::optimize_image(&src, &dst, max_side, quality, out_format)
        })
        .await
        .map_err(|e| PaperedError::Indexing(format!("Image optimization task failed: {e}")))?;

        let (_image_name, dest_path) = match optimize_result {
            Ok((_, actual_format)) => {
                let ext = crate::util::image::format_extension(actual_format);
                let name = format!("{filename}.{ext}");
                let dest = paper_data_dir.join(&name);
                if let Err(e) = std::fs::rename(&tmp_path, &dest) {
                    tracing::warn!("Failed to rename optimized image: {e}");
                }
                (name, dest)
            }
            Err(e) => {
                let ext = out_format.default_extension();
                let name = format!("{filename}.{ext}");
                let dest = paper_data_dir.join(&name);
                tracing::warn!(
                    "Failed to optimize standalone image {name}: {e}; using original copy"
                );
                let _ = std::fs::remove_file(&tmp_path);
                if let Err(e) = std::fs::copy(path, &dest) {
                    tracing::warn!("Failed to copy original image {}: {}", path.display(), e);
                }
                (name, dest)
            }
        };
        tracing::info!("Copied image to {}", dest_path.display());

        let mut paper = Paper::new(filename);
        paper.id = paper_id.to_string();
        paper.file_path = Some(path.to_string_lossy().into_owned());
        paper.file_hash = Some(file_hash.to_string());
        paper.status = PaperStatus::Indexed;
        paper.embedding_model = self.config.embedding_fingerprint();
        paper.source = Some(PaperSource::Manual);

        self.store.insert_paper(&paper).await?;

        let description = self.describe_image(&dest_path).await;
        let description_text = description.as_deref().unwrap_or(filename);

        let mut vec_records: Vec<VectorRecord> = Vec::new();

        let vector = match crate::llm::embed::embed_image_or_text(
            &self.embedding,
            &dest_path,
            description_text,
        )
        .await
        {
            Ok(embedding) => Some(embedding),
            Err(e) => {
                tracing::warn!(
                    paper_id = %paper_id,
                    error = %e,
                    "Image embedding failed"
                );
                None
            }
        };

        if let Some(vector) = vector {
            vec_records.push(VectorRecord {
                paper_id: paper_id.to_string(),
                section_type: "image".to_string(),
                vector,
                chunk_text: description_text.to_string(),
                content_type: "image".to_string(),
            });
        }

        if vec_records.is_empty() {
            return Err(PaperedError::Indexing(format!(
                "Failed to generate any embedding for image {filename} (paper_id={paper_id})."
            )));
        }

        self.store.upsert(&vec_records).await?;
        if let Some(fingerprint) = self.config.embedding_fingerprint()
            && let Err(e) = self
                .store
                .set_paper_embedding_model(paper_id, &fingerprint)
                .await
        {
            tracing::warn!(
                paper_id = %paper_id,
                error = %e,
                "Failed to set embedding model"
            );
        }

        tracing::info!(
            "Indexed image: {} (paper_id={}, {} vectors)",
            filename,
            paper_id,
            vec_records.len()
        );
        Ok(paper)
    }

    /// Generate a natural language description of an image using LLM vision.
    ///
    /// Returns `None` if no vision model is configured (purposes.vision is empty)
    /// or if the request fails.
    async fn describe_image(&self, path: &Path) -> Option<String> {
        let client = self.vision_client.as_ref()?;
        let b64 = crate::llm::embed::image_to_base64(path).await.ok()?;
        let prompt = "Describe this image in 1-2 sentences. Include key visual elements, subjects, style, and any text or details that would make it searchable.";
        client
            .generate_with_images(
                "",
                prompt,
                &[b64],
                self.config.section.max_output_tokens,
                0.1,
            )
            .await
            .ok()
    }
}
