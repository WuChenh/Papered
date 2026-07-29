//! Reindexing operations — full, sections-only, and re-embed.

use crate::error::{PaperedError, Result};
use crate::paper::source::DocumentSource;
use std::path::Path;

impl super::Indexer {
    /// Re-index an existing paper (e.g., after content change).
    /// The old paper record is preserved; only associated data is replaced.
    /// If reindexing fails, the paper remains intact (though may lack sections/chunks/vectors).
    pub async fn reindex_paper(&self, paper_id: &str) -> Result<crate::paper::Paper> {
        let paper = self
            .store
            .get_paper(paper_id)
            .await?
            .ok_or_else(|| PaperedError::not_found(format!("Paper not found: {paper_id}")))?;

        if let Some(ref path_str) = paper.file_path {
            let path = Path::new(path_str);
            let source = DocumentSource::from_path(path).unwrap_or(DocumentSource::Pdf);
            self.ingest_document(path, source, Some(paper_id), true)
                .await
        } else {
            Err(PaperedError::Indexing(format!(
                "Paper {paper_id} has no associated file"
            )))
        }
    }

    /// Re-extract sections (via LLM) and re-embed for a paper that already has MinerU
    /// extraction results persisted (chunks, figures, tables). Skips the expensive PDF
    /// extraction and chunking steps. If no chunks exist, falls back to full reindex.
    pub async fn reindex_sections_only(&self, paper_id: &str) -> Result<crate::paper::Paper> {
        let mut paper = self
            .store
            .get_paper(paper_id)
            .await?
            .ok_or_else(|| PaperedError::not_found(format!("Paper not found: {paper_id}")))?;

        let chunks = self.store.get_chunks(paper_id).await?;
        if chunks.is_empty() {
            tracing::info!(
                "No existing chunks for paper {}, falling back to full reindex",
                paper_id
            );
            if let Some(ref path_str) = paper.file_path {
                let path = Path::new(path_str);
                let source = DocumentSource::from_path(path).unwrap_or(DocumentSource::Pdf);
                return self
                    .ingest_document(path, source, Some(paper_id), true)
                    .await;
            }
            return Err(PaperedError::Indexing(format!(
                "Paper {paper_id} has no associated file"
            )));
        }

        tracing::info!(
            "Sections-only reindex for paper {} ({} existing chunks)",
            paper_id,
            chunks.len()
        );

        // Assemble full text from existing chunks
        let full_text: String = chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        // Delete old sections and section vectors
        if let Err(e) = self.store.delete_sections(paper_id).await {
            tracing::warn!(
                paper_id = %paper_id,
                error = %e,
                "Failed to delete old sections during reindex"
            );
        }
        if let Err(e) = self
            .store
            .delete_by_paper_and_content_type(paper_id, "section")
            .await
        {
            tracing::warn!(
                paper_id = %paper_id,
                error = %e,
                "Failed to delete old section vectors during reindex"
            );
        }

        // Re-extract sections + metadata from existing data
        let (sections, llm_meta) = self.extract_sections(&paper, &full_text).await?;

        // Apply all LLM metadata to paper
        super::helpers::apply_metadata(&mut paper, &llm_meta);
        self.store.update_paper(&paper).await?;
        // Replace stored bio-entities (delete + insert — reindex-safe).
        self.store
            .set_paper_entities(paper_id, &paper.entities)
            .await?;

        // Store new sections and vectors
        self.store.insert_sections(paper_id, &sections).await?;
        self.index_vectors(paper_id, &sections).await.map_err(|e| {
            tracing::error!(
                "Sections-only reindex: vector indexing failed for {}: {}",
                paper_id,
                e
            );
            e
        })?;

        tracing::info!(
            "Sections-only reindex complete for {} ({} sections)",
            paper_id,
            sections.sections.len()
        );

        Ok(paper)
    }

    /// Re-embed existing sections using the current embedding model.
    /// Does NOT call the LLM or access the PDF — reads sections from DB and re-generates vectors only.
    pub async fn reembed_paper(&self, paper_id: &str) -> Result<crate::paper::Paper> {
        let paper = self
            .store
            .get_paper(paper_id)
            .await?
            .ok_or_else(|| PaperedError::not_found(format!("Paper not found: {paper_id}")))?;

        let sections = self.store.get_sections(paper_id).await?;
        if sections.sections.is_empty() {
            return Err(PaperedError::Indexing(format!(
                "Paper {paper_id} has no sections to re-embed"
            )));
        }

        tracing::info!(
            "Re-embedding {} sections for paper {}",
            sections.sections.len(),
            paper_id
        );

        self.store
            .delete_by_paper_and_content_type(paper_id, "section")
            .await?;
        self.index_vectors(paper_id, &sections).await.map_err(|e| {
            tracing::error!("Re-embed: vector indexing failed for {}: {}", paper_id, e);
            e
        })?;

        tracing::info!(
            "Re-embed complete for {} ({} sections)",
            paper_id,
            sections.sections.len()
        );
        Ok(paper)
    }
}
