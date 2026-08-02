//! `VectorStore` trait implementation for [`TursoStore`].
//!
//! Domain-specific logic lives in focused sub-modules (`papers`, `chunks`,
//! `figures`, `prompts`, `annotations`, `translations`, `health`). This file
//! retains the vector operations inline and delegates all other trait methods
//! to the inherent methods defined in those sub-modules.

use super::{
    DbExt, TursoStore, get_real, get_text,
    query_builder::{MAX_QUERY_VARS, SqlFilter},
    vector_to_sql,
};
use crate::error::Result;
use crate::store::vector::{VectorRecord, VectorSearchResult, VectorStore};
use async_trait::async_trait;

#[async_trait]
impl VectorStore for TursoStore {
    // ========================================================================
    // Vector operations (implemented inline)
    // ========================================================================

    async fn upsert(&self, records: &[VectorRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        // SQLite limits the number of host parameters to 999. Each record uses
        // 6 parameters, so cap batches at 150 records to stay safely under the
        // limit and reduce round trips compared to one-row-at-a-time inserts.
        const VARS_PER_RECORD: usize = 6;
        const BATCH_SIZE: usize = MAX_QUERY_VARS / VARS_PER_RECORD;

        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().await.db("vector upsert tx")?;
        for batch in records.chunks(BATCH_SIZE) {
            let placeholders: Vec<String> = (0..batch.len())
                .map(|i| {
                    let base = i * VARS_PER_RECORD + 1;
                    format!(
                        "(?{0}, ?{1}, ?{2}, ?{3}, ?{4}, vector32(?{5}))",
                        base,
                        base + 1,
                        base + 2,
                        base + 3,
                        base + 4,
                        base + 5
                    )
                })
                .collect();
            let sql = format!(
                "INSERT OR REPLACE INTO vectors (id, paper_id, content_type, section_type, chunk_text, vector) VALUES {}",
                placeholders.join(", ")
            );
            let mut params: Vec<turso::Value> = Vec::with_capacity(batch.len() * VARS_PER_RECORD);
            for rec in batch {
                let id = format!("{}:{}:{}", rec.paper_id, rec.content_type, rec.section_type);
                params.push(turso::Value::Text(id));
                params.push(turso::Value::Text(rec.paper_id.clone()));
                params.push(turso::Value::Text(rec.content_type.clone()));
                params.push(turso::Value::Text(rec.section_type.clone()));
                params.push(turso::Value::Text(rec.chunk_text.clone()));
                params.push(turso::Value::Text(vector_to_sql(&rec.vector)));
            }
            tx.execute(&sql, params).await.db("vector upsert")?;
        }
        tx.commit().await.db("vector upsert commit")?;
        Ok(())
    }

    async fn search_with_content_type(
        &self,
        query_vector: &[f32],
        section_type: Option<&str>,
        content_type: Option<&str>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<VectorSearchResult>> {
        let conn = self.read_lock().await;
        let query_str = vector_to_sql(query_vector);
        let max_distance = (1.0 - min_score) as f64;

        let mut params: Vec<turso::Value> = vec![turso::Value::Text(query_str)];
        let mut filter = SqlFilter::new();

        if let Some(ct) = content_type {
            filter.eq("content_type", turso::Value::Text(ct.to_string()));
        }
        if let Some(st) = section_type {
            filter.eq("section_type", turso::Value::Text(st.to_string()));
        }

        let filter_offset = 1; // ?1 is the query vector
        let filter_clauses = filter.clauses(filter_offset);
        params.extend(filter.params());

        let max_dist_idx = params.len() + 1;
        params.push(turso::Value::Real(max_distance));

        let top_k_idx = params.len() + 1;
        params.push(turso::Value::Integer(top_k as i64));

        let where_sql = if filter_clauses.is_empty() {
            format!("vector_distance_cos(vector, vector32(?1)) <= ?{max_dist_idx}")
        } else {
            format!(
                "{} AND vector_distance_cos(vector, vector32(?1)) <= ?{max_dist_idx}",
                filter_clauses.join(" AND ")
            )
        };

        let sql = format!(
            "SELECT paper_id, content_type, section_type, chunk_text, distance, 1.0 - distance as score
             FROM (
                 SELECT paper_id, content_type, section_type, chunk_text,
                        vector_distance_cos(vector, vector32(?1)) as distance
                 FROM vectors
                 WHERE {where_sql}
                 ORDER BY distance
                 LIMIT ?{top_k_idx}
             )
             ORDER BY score DESC"
        );

        let mut rows = conn.query(&sql, params).await.db("vector search")?;

        let mut results = Vec::new();
        while let Some(row) = rows.next().await.db("vector search row")? {
            let paper_id = get_text(&row.get_value(0)?).unwrap_or_default();
            let content_type = get_text(&row.get_value(1)?).unwrap_or_default();
            let section_type = get_text(&row.get_value(2)?).unwrap_or_default();
            let chunk_text = get_text(&row.get_value(3)?).unwrap_or_default();
            let score = get_real(&row.get_value(5)?).unwrap_or(0.0) as f32;
            results.push(VectorSearchResult {
                paper_id,
                content_type,
                section_type,
                chunk_text,
                score,
            });
        }
        Ok(results)
    }

    async fn delete_by_paper(&self, paper_id: &str) -> Result<()> {
        self.exec(
            "DELETE FROM vectors WHERE paper_id = ?1",
            vec![turso::Value::Text(paper_id.to_string())],
            "vector delete",
        )
        .await
    }

    async fn delete_by_paper_and_content_type(
        &self,
        paper_id: &str,
        content_type: &str,
    ) -> Result<()> {
        self.exec(
            "DELETE FROM vectors WHERE paper_id = ?1 AND content_type = ?2",
            vec![
                turso::Value::Text(paper_id.to_string()),
                turso::Value::Text(content_type.to_string()),
            ],
            "vector delete ct",
        )
        .await
    }

    async fn count(&self) -> Result<usize> {
        self.count_query("SELECT COUNT(*) FROM vectors", Vec::new(), "vector count")
            .await
    }

    async fn get_paper_vectors_with_content_type(
        &self,
        paper_id: &str,
        section_type: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<Vec<(Vec<f32>, String)>> {
        use super::vector_from_text;

        let conn = self.read_lock().await;
        let mut params: Vec<turso::Value> = vec![turso::Value::Text(paper_id.to_string())];
        let mut filter = SqlFilter::new();

        if let Some(st) = section_type {
            filter.eq("section_type", turso::Value::Text(st.to_string()));
        }
        if let Some(ct) = content_type {
            filter.eq("content_type", turso::Value::Text(ct.to_string()));
        }

        let filter_clauses = filter.clauses(1); // ?1 is paper_id
        params.extend(filter.params());

        let mut conditions = vec!["paper_id = ?1".to_string()];
        conditions.extend(filter_clauses);
        let sql = format!(
            "SELECT vector_extract(vector), chunk_text FROM vectors WHERE {} ORDER BY id",
            conditions.join(" AND ")
        );
        let mut rows = conn.query(&sql, params).await.db("get paper vectors")?;
        let mut results = Vec::new();
        while let Some(row) = rows.next().await.db("get paper vectors row")? {
            let vec_text = get_text(&row.get_value(0)?).unwrap_or_default();
            let vector = vector_from_text(&vec_text).unwrap_or_default();
            let text = get_text(&row.get_value(1)?).unwrap_or_default();
            results.push((vector, text));
        }
        Ok(results)
    }

    async fn papers_without_vectors(&self) -> Result<Vec<crate::store::vector::PaperRef>> {
        self.query_all(
            "SELECT id, title FROM papers WHERE id NOT IN (SELECT DISTINCT paper_id FROM vectors)",
            Vec::new(),
            "papers without vectors",
            |row| {
                Ok(crate::store::vector::PaperRef {
                    id: get_text(&row.get_value(0)?).unwrap_or_default(),
                    title: get_text(&row.get_value(1)?).unwrap_or_default(),
                })
            },
        )
        .await
    }

    async fn orphaned_vector_paper_ids(&self) -> Result<Vec<String>> {
        self.query_all(
            "SELECT DISTINCT paper_id FROM vectors WHERE paper_id NOT IN (SELECT id FROM papers)",
            Vec::new(),
            "orphaned vectors",
            |row| Ok(get_text(&row.get_value(0)?)),
        )
        .await
        .map(|ids| ids.into_iter().flatten().collect())
    }

    async fn clear_all_vectors(&self, _new_dimension: usize) -> Result<()> {
        self.exec("DELETE FROM vectors", Vec::new(), "clear vectors")
            .await
    }

    // ========================================================================
    // Paper metadata — delegates to papers.rs
    // ========================================================================

    async fn insert_paper(&self, paper: &crate::paper::Paper) -> Result<()> {
        TursoStore::insert_paper(self, paper).await
    }

    async fn get_paper(&self, paper_id: &str) -> Result<Option<crate::paper::Paper>> {
        TursoStore::get_paper(self, paper_id).await
    }

    async fn get_papers_by_ids(&self, ids: &[&str]) -> Result<Vec<crate::paper::Paper>> {
        TursoStore::get_papers_by_ids(self, ids).await
    }

    async fn get_paper_by_file_hash(&self, file_hash: &str) -> Result<Option<crate::paper::Paper>> {
        TursoStore::get_paper_by_file_hash(self, file_hash).await
    }

    async fn list_papers(&self, limit: usize, offset: usize) -> Result<Vec<crate::paper::Paper>> {
        TursoStore::list_papers(self, limit, offset).await
    }

    async fn list_papers_filtered(
        &self,
        status: Option<&str>,
        paper_type: Option<&str>,
        keyword: Option<&str>,
        entity_filter: &crate::paper::EntityFilter,
        sort_by: Option<&str>,
        sort_desc: bool,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<crate::paper::Paper>, usize)> {
        TursoStore::list_papers_filtered(
            self,
            status,
            paper_type,
            keyword,
            entity_filter,
            sort_by,
            sort_desc,
            limit,
            offset,
        )
        .await
    }

    async fn paper_count(&self) -> Result<usize> {
        TursoStore::paper_count(self).await
    }

    async fn count_papers_by_status(&self, status: &str) -> Result<usize> {
        TursoStore::count_papers_by_status(self, status).await
    }

    async fn duplicate_scan_papers(&self) -> Result<Vec<crate::store::vector::DuplicatePaperInfo>> {
        TursoStore::duplicate_scan_papers(self).await
    }

    async fn delete_paper(&self, paper_id: &str) -> Result<()> {
        TursoStore::delete_paper(self, paper_id).await
    }

    async fn update_paper_status(
        &self,
        paper_id: &str,
        status: &str,
        error_message: Option<&str>,
        retry_count: Option<u32>,
    ) -> Result<()> {
        TursoStore::update_paper_status(self, paper_id, status, error_message, retry_count).await
    }

    async fn update_paper_cover(&self, paper_id: &str, cover_path: &str) -> Result<()> {
        TursoStore::update_paper_cover(self, paper_id, cover_path).await
    }

    async fn set_paper_embedding_model(&self, paper_id: &str, embedding_model: &str) -> Result<()> {
        TursoStore::set_paper_embedding_model(self, paper_id, embedding_model).await
    }

    async fn update_paper(&self, paper: &crate::paper::Paper) -> Result<()> {
        TursoStore::update_paper(self, paper).await
    }

    async fn set_paper_entities(
        &self,
        paper_id: &str,
        entities: &crate::paper::BioEntities,
    ) -> Result<()> {
        TursoStore::set_paper_entities(self, paper_id, entities).await
    }

    async fn paper_entities(&self, paper_id: &str) -> Result<crate::paper::BioEntities> {
        TursoStore::paper_entities(self, paper_id).await
    }

    async fn papers_entities_batch(
        &self,
        paper_ids: &[String],
    ) -> Result<std::collections::HashMap<String, crate::paper::BioEntities>> {
        TursoStore::papers_entities_batch(self, paper_ids).await
    }

    async fn paper_ids_by_entity(&self, kind: &str, value: &str) -> Result<Vec<String>> {
        TursoStore::paper_ids_by_entity(self, kind, value).await
    }

    async fn paper_ids_by_paper_type(&self, paper_type: &str) -> Result<Vec<String>> {
        TursoStore::paper_ids_by_paper_type(self, paper_type).await
    }

    async fn get_papers_by_status(&self, status: &str) -> Result<Vec<crate::paper::Paper>> {
        TursoStore::get_papers_by_status(self, status).await
    }

    async fn get_papers_by_status_with_retry_below(
        &self,
        status: &str,
        max_retry: u32,
    ) -> Result<Vec<crate::paper::Paper>> {
        TursoStore::get_papers_by_status_with_retry_below(self, status, max_retry).await
    }

    // ========================================================================
    // Ratings & comments — delegates to annotations.rs
    // ========================================================================

    async fn get_paper_rating(&self, paper_id: &str) -> Result<Option<i64>> {
        TursoStore::get_paper_rating(self, paper_id).await
    }

    async fn set_paper_rating(&self, paper_id: &str, rating: i64) -> Result<()> {
        TursoStore::set_paper_rating(self, paper_id, rating).await
    }

    async fn delete_paper_rating(&self, paper_id: &str) -> Result<()> {
        TursoStore::delete_paper_rating(self, paper_id).await
    }

    async fn list_paper_comments(
        &self,
        paper_id: &str,
    ) -> Result<Vec<crate::store::vector::PaperComment>> {
        TursoStore::list_paper_comments(self, paper_id).await
    }

    async fn add_paper_comment(
        &self,
        paper_id: &str,
        content: &str,
    ) -> Result<crate::store::vector::PaperComment> {
        TursoStore::add_paper_comment(self, paper_id, content).await
    }

    async fn delete_paper_comment(&self, paper_id: &str, comment_id: i64) -> Result<()> {
        TursoStore::delete_paper_comment(self, paper_id, comment_id).await
    }

    async fn annotation_summaries(
        &self,
        paper_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, crate::store::vector::AnnotationSummary>> {
        TursoStore::annotation_summaries(self, paper_ids).await
    }

    // ========================================================================
    // Sections & FTS — delegates to chunks.rs
    // ========================================================================

    async fn insert_sections(
        &self,
        paper_id: &str,
        sections: &crate::paper::section::PaperSections,
    ) -> Result<()> {
        TursoStore::insert_sections(self, paper_id, sections).await
    }

    async fn get_sections(&self, paper_id: &str) -> Result<crate::paper::section::PaperSections> {
        TursoStore::get_sections(self, paper_id).await
    }

    async fn delete_sections(&self, paper_id: &str) -> Result<()> {
        TursoStore::delete_sections(self, paper_id).await
    }

    async fn get_sections_batch(
        &self,
        paper_ids: &[&str],
    ) -> Result<Vec<crate::paper::section::PaperSections>> {
        TursoStore::get_sections_batch(self, paper_ids).await
    }

    async fn fulltext_search_with_snippets(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(crate::paper::Paper, f32, String)>> {
        TursoStore::fulltext_search_with_snippets(self, query, limit).await
    }

    // ========================================================================
    // Chunks — delegates to chunks.rs
    // ========================================================================

    async fn insert_chunks(&self, paper_id: &str, chunks: &[crate::chunker::Chunk]) -> Result<()> {
        TursoStore::insert_chunks(self, paper_id, chunks).await
    }

    async fn get_chunks(&self, paper_id: &str) -> Result<Vec<crate::chunker::Chunk>> {
        TursoStore::get_chunks(self, paper_id).await
    }

    async fn get_chunk(
        &self,
        paper_id: &str,
        chunk_id: &str,
    ) -> Result<Option<crate::chunker::Chunk>> {
        TursoStore::get_chunk(self, paper_id, chunk_id).await
    }

    async fn get_chunk_ancestors(
        &self,
        paper_id: &str,
        chunk_ids: &[&str],
    ) -> Result<Vec<crate::chunker::Chunk>> {
        TursoStore::get_chunk_ancestors(self, paper_id, chunk_ids).await
    }

    async fn delete_chunks(&self, paper_id: &str) -> Result<()> {
        TursoStore::delete_chunks(self, paper_id).await
    }

    async fn search_papers_by_path(&self, query: &str, limit: usize) -> Result<Vec<(String, f32)>> {
        TursoStore::search_papers_by_path(self, query, limit).await
    }

    async fn search_chunks(
        &self,
        paper_ids: &[&str],
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::store::vector::ChunkHit>> {
        TursoStore::search_chunks(self, paper_ids, query, limit).await
    }

    async fn search_all_chunks(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::store::vector::ChunkHit>> {
        TursoStore::search_all_chunks(self, query, limit).await
    }

    // ========================================================================
    // Prompts — delegates to prompts.rs
    // ========================================================================

    async fn list_prompts(&self) -> Result<Vec<crate::Prompt>> {
        TursoStore::list_prompts(self).await
    }

    async fn get_prompt(&self, prompt_id: &str) -> Result<Option<crate::Prompt>> {
        TursoStore::get_prompt(self, prompt_id).await
    }

    async fn get_default_prompt(&self) -> Result<Option<crate::Prompt>> {
        TursoStore::get_default_prompt(self).await
    }

    async fn insert_prompt(&self, prompt: &crate::Prompt) -> Result<()> {
        TursoStore::insert_prompt(self, prompt).await
    }

    async fn update_prompt(&self, prompt: &crate::Prompt) -> Result<()> {
        TursoStore::update_prompt(self, prompt).await
    }

    async fn delete_prompt(&self, prompt_id: &str) -> Result<()> {
        TursoStore::delete_prompt(self, prompt_id).await
    }

    async fn set_default_prompt(&self, prompt_id: &str) -> Result<()> {
        TursoStore::set_default_prompt(self, prompt_id).await
    }

    // ========================================================================
    // Figures — delegates to figures.rs
    // ========================================================================

    async fn insert_figures(
        &self,
        paper_id: &str,
        figures: &[crate::index::multimodal::FigureInfo],
    ) -> Result<()> {
        TursoStore::insert_figures(self, paper_id, figures).await
    }

    async fn get_figures(
        &self,
        paper_id: &str,
    ) -> Result<Vec<crate::index::multimodal::FigureInfo>> {
        TursoStore::get_figures(self, paper_id).await
    }

    async fn delete_figures(&self, paper_id: &str) -> Result<()> {
        TursoStore::delete_figures(self, paper_id).await
    }

    async fn update_figure_image_path(&self, figure_id: &str, image_path: &str) -> Result<()> {
        TursoStore::update_figure_image_path(self, figure_id, image_path).await
    }

    async fn figures_with_missing_images(
        &self,
        data_dir: &std::path::Path,
    ) -> Result<Vec<crate::store::vector::MissingFigureImage>> {
        TursoStore::figures_with_missing_images(self, data_dir).await
    }

    async fn figure_count(&self) -> Result<usize> {
        TursoStore::figure_count(self).await
    }

    // ========================================================================
    // Health & maintenance — delegates to health.rs
    // ========================================================================

    async fn papers_with_missing_files(&self) -> Result<Vec<crate::store::vector::PaperRef>> {
        TursoStore::papers_with_missing_files(self).await
    }

    async fn orphaned_data_directories(&self, data_dir: &std::path::Path) -> Result<Vec<String>> {
        TursoStore::orphaned_data_directories(self, data_dir).await
    }

    async fn optimize(&self) -> Result<()> {
        TursoStore::optimize(self).await
    }

    async fn store_dimension(&self) -> Option<usize> {
        TursoStore::store_dimension(self).await
    }

    async fn clear_all_data(&self) -> Result<()> {
        TursoStore::clear_all_data(self).await
    }

    async fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        TursoStore::set_meta(self, key, value).await
    }

    async fn get_meta(&self, key: &str) -> Result<Option<String>> {
        TursoStore::get_meta(self, key).await
    }

    // ========================================================================
    // LLM call metrics — delegates to health.rs
    // ========================================================================

    async fn insert_llm_call_metric(
        &self,
        metric: &crate::llm::metrics::LlmCallMetric,
    ) -> Result<()> {
        TursoStore::insert_llm_call_metric(self, metric).await
    }

    async fn llm_call_metrics_summary(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<crate::llm::metrics::LlmCallMetricGroup>> {
        TursoStore::llm_call_metrics_summary(self, since).await
    }

    // ========================================================================
    // Translations — delegates to translations.rs
    // ========================================================================

    async fn upsert_translation(&self, t: &crate::store::vector::TranslationInfo) -> Result<()> {
        TursoStore::upsert_translation(self, t).await
    }

    async fn get_translations(
        &self,
        paper_id: &str,
        lang: &str,
    ) -> Result<Vec<crate::store::vector::TranslationInfo>> {
        TursoStore::get_translations(self, paper_id, lang).await
    }

    async fn get_translation(
        &self,
        paper_id: &str,
        content_type: &str,
        content_ref: &str,
        lang: &str,
    ) -> Result<Option<crate::store::vector::TranslationInfo>> {
        TursoStore::get_translation(self, paper_id, content_type, content_ref, lang).await
    }

    async fn delete_translations(&self, paper_id: &str) -> Result<()> {
        TursoStore::delete_translations(self, paper_id).await
    }

    async fn search_translations(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::store::vector::TranslationInfo>> {
        TursoStore::search_translations(self, query, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::super::chunks::chunk_hierarchy;
    use crate::chunker::{Chunk, ChunkType, chunk_markdown};
    use crate::paper::Paper;
    use crate::store::turso::{TestDb, TursoStore};
    use crate::store::vector::{AnnotationSummary, VectorStore};

    fn chunk(id: &str, parent: Option<&str>, title: &str, ty: ChunkType) -> Chunk {
        let mut c = Chunk::new("paper1", id, ty, title);
        c.parent_id = parent.map(|s| s.to_string());
        c
    }

    #[test]
    fn hierarchy_chapter_section_paragraph() {
        let chunks = vec![
            chunk("s1", None, "Intro", ChunkType::Chapter),
            chunk("s2", Some("s1"), "Background", ChunkType::Section),
            chunk("p1", Some("s2"), "Body text here.", ChunkType::Paragraph),
        ];
        let m = chunk_hierarchy(&chunks);
        assert_eq!(m["s1"].0, "Intro");
        assert_eq!((m["s1"].1, m["s1"].2), (1, 0));
        assert_eq!(m["s2"].0, "Intro > Background");
        assert_eq!((m["s2"].1, m["s2"].2), (2, 1));
        // paragraph path is the containing section's path (body excluded)
        assert_eq!(m["p1"].0, "Intro > Background");
        assert_eq!((m["p1"].1, m["p1"].2), (3, 2));
    }

    #[test]
    fn hierarchy_flat_chunks_have_empty_path() {
        let chunks = vec![chunk("c1", None, "loose paragraph", ChunkType::Paragraph)];
        let m = chunk_hierarchy(&chunks);
        assert_eq!(m["c1"].0, "");
        assert_eq!((m["c1"].1, m["c1"].2), (1, 0));
    }

    #[tokio::test]
    async fn search_papers_by_path_finds_heading() {
        let db = TestDb::new("path");

        let store = TursoStore::new(db.path()).await.expect("open");
        let mut paper = Paper::new("Path Paper");
        paper.id = "qp".to_string();
        store.insert_paper(&paper).await.expect("insert paper");

        let tree = chunk_markdown(
            "qp",
            "# Methods\n\n## Transformer Architecture\n\nbody text\n",
        );
        store
            .insert_chunks("qp", &tree)
            .await
            .expect("insert chunks");

        let hits = store
            .search_papers_by_path("Transformer Architecture", 10)
            .await
            .expect("path search");
        assert!(
            hits.iter().any(|(id, s)| id == "qp" && *s > 0.0),
            "expected qp with positive score: {hits:?}"
        );

        let none = store
            .search_papers_by_path("zzzqqqnonexistent", 10)
            .await
            .expect("empty path search");
        assert!(none.is_empty(), "expected no hits: {none:?}");
    }

    #[tokio::test]
    async fn get_chunk_ancestors_walks_parent_chain() {
        let db = TestDb::new("ancestors");
        let store = TursoStore::new(db.path()).await.expect("open");

        let mut paper = Paper::new("Ancestor Paper");
        paper.id = "paper1".to_string();
        store.insert_paper(&paper).await.expect("insert paper");

        // chapter -> section -> paragraph
        let chunks = vec![
            chunk("s1", None, "Intro", ChunkType::Chapter),
            chunk("s2", Some("s1"), "Background", ChunkType::Section),
            chunk("p1", Some("s2"), "Body text here.", ChunkType::Paragraph),
        ];
        store
            .insert_chunks("paper1", &chunks)
            .await
            .expect("insert chunks");

        // Walking from the leaf paragraph must yield the paragraph itself plus
        // its section and chapter ancestors. This path used a recursive CTE the
        // engine does not support; it is now an iterative parent-chain walk.
        let ancestors = store
            .get_chunk_ancestors("paper1", &["p1"])
            .await
            .expect("get ancestors");
        let ids: std::collections::HashSet<&str> =
            ancestors.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains("p1"), "leaf chunk missing: {ids:?}");
        assert!(ids.contains("s2"), "section ancestor missing: {ids:?}");
        assert!(ids.contains("s1"), "chapter ancestor missing: {ids:?}");

        // The ancestor set must be sufficient to rebuild the heading path.
        let leaf = ancestors.iter().find(|c| c.id == "p1").expect("leaf");
        let heading = crate::retrieval::heading_path_from_ancestors(leaf, &ancestors);
        assert_eq!(heading.as_deref(), Some("Intro > Background"));
    }

    #[tokio::test]
    async fn llm_call_metrics_roundtrip_and_summary() {
        use crate::llm::metrics::{CallKind, LlmCallMetric, TokenUsage};

        let db = TestDb::new("llm_metrics");
        let store = TursoStore::new(db.path()).await.expect("open");

        let ok = LlmCallMetric {
            kind: CallKind::Chat,
            model: "gpt-test".to_string(),
            usage: TokenUsage {
                prompt_tokens: Some(100),
                completion_tokens: Some(20),
            },
            latency_ms: 250,
            success: true,
            error: None,
        };
        let failed = LlmCallMetric {
            kind: CallKind::Chat,
            model: "gpt-test".to_string(),
            usage: TokenUsage::default(),
            latency_ms: 50,
            success: false,
            error: Some("boom".to_string()),
        };
        let embed = LlmCallMetric {
            kind: CallKind::Embedding,
            model: "emb-test".to_string(),
            usage: TokenUsage {
                prompt_tokens: Some(40),
                completion_tokens: None,
            },
            latency_ms: 30,
            success: true,
            error: None,
        };
        store.insert_llm_call_metric(&ok).await.expect("insert ok");
        store
            .insert_llm_call_metric(&failed)
            .await
            .expect("insert failed");
        store
            .insert_llm_call_metric(&embed)
            .await
            .expect("insert embed");

        let all = store
            .llm_call_metrics_summary(None)
            .await
            .expect("summary all");
        assert_eq!(all.len(), 2, "expected two (kind, model) groups: {all:?}");
        let chat = all.iter().find(|g| g.kind == "chat").expect("chat group");
        assert_eq!(chat.model, "gpt-test");
        assert_eq!(chat.calls, 2);
        assert_eq!(chat.failures, 1);
        assert_eq!(chat.prompt_tokens, 100);
        assert_eq!(chat.completion_tokens, 20);
        assert!(
            (chat.avg_latency_ms - 150.0).abs() < 0.01,
            "avg latency: {}",
            chat.avg_latency_ms
        );
        let emb = all
            .iter()
            .find(|g| g.kind == "embedding")
            .expect("embedding group");
        assert_eq!(emb.calls, 1);
        assert_eq!(emb.failures, 0);
        assert_eq!(emb.prompt_tokens, 40);

        // Rows are recorded "now", so a cutoff in the past includes them and
        // a cutoff in the future excludes them.
        let past = chrono::Utc::now() - chrono::Duration::hours(24);
        let recent = store
            .llm_call_metrics_summary(Some(past))
            .await
            .expect("summary recent");
        assert_eq!(recent.len(), 2);

        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let none = store
            .llm_call_metrics_summary(Some(future))
            .await
            .expect("summary future");
        assert!(none.is_empty(), "future cutoff must exclude rows: {none:?}");
    }

    #[tokio::test]
    async fn fulltext_search_with_snippets_returns_score_and_snippet() {
        let db = TestDb::new("fts_snippets");

        let store = TursoStore::new(db.path()).await.expect("open");
        let mut paper = Paper::new("Zebrafish Glycosylation Pathways");
        paper.id = "fts1".to_string();
        paper.abstract_text = Some("A study of glycosylation enzymes in zebrafish.".to_string());
        paper.keywords = vec!["glycosylation".to_string()];
        store.insert_paper(&paper).await.expect("insert paper");

        let results = store
            .fulltext_search_with_snippets("glycosylation", 10)
            .await
            .expect("fts search must not error on hit rows");
        assert_eq!(results.len(), 1, "expected one hit: {results:?}");
        let (found, score, snippet) = &results[0];
        assert_eq!(found.id, "fts1");
        assert!(*score > 0.0, "expected positive score, got {score}");
        assert!(!snippet.is_empty(), "expected non-empty snippet");
    }

    // ========================================================================
    // Bio-entities
    // ========================================================================

    async fn insert_paper_with_id(store: &TursoStore, id: &str, title: &str) {
        let mut paper = Paper::new(title);
        paper.id = id.to_string();
        store.insert_paper(&paper).await.expect("insert paper");
    }

    #[tokio::test]
    async fn paper_entities_usable_on_fresh_db() {
        let db = TestDb::new("entities_fresh");
        let store = TursoStore::new(db.path()).await.expect("open");

        insert_paper_with_id(&store, "p1", "paper").await;
        store
            .set_paper_entities(
                "p1",
                &crate::paper::BioEntities {
                    genes: vec!["OsALS".into()],
                    ..Default::default()
                },
            )
            .await
            .expect("set");
        assert_eq!(
            store.paper_entities("p1").await.expect("get").genes,
            vec!["OsALS".to_string()]
        );
    }

    #[tokio::test]
    async fn paper_entities_set_get_round_trip() {
        let db = TestDb::new("entities_roundtrip");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "Rice ALS paper").await;

        let entities = crate::paper::BioEntities {
            species: vec!["Oryza sativa".into()],
            genes: vec!["OsALS".into(), "TP53".into()],
            techniques: vec!["CRISPR".into()],
            pathways: vec![],
        };
        store
            .set_paper_entities("p1", &entities)
            .await
            .expect("set entities");

        let loaded = store.paper_entities("p1").await.expect("get entities");
        assert_eq!(loaded, entities);

        // Unknown paper -> empty.
        let none = store.paper_entities("nope").await.expect("get empty");
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn paper_entities_reset_replaces_previous() {
        let db = TestDb::new("entities_replace");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "paper").await;

        let first = crate::paper::BioEntities {
            genes: vec!["OLD1".into()],
            ..Default::default()
        };
        store
            .set_paper_entities("p1", &first)
            .await
            .expect("set first");
        let second = crate::paper::BioEntities {
            species: vec!["Rice".into()],
            ..Default::default()
        };
        store
            .set_paper_entities("p1", &second)
            .await
            .expect("set second");

        let loaded = store.paper_entities("p1").await.expect("get");
        assert_eq!(loaded, second, "re-set must replace, not merge");

        // Re-set with empty clears all rows (reindex on a non-bio paper).
        store
            .set_paper_entities("p1", &crate::paper::BioEntities::default())
            .await
            .expect("clear");
        assert!(store.paper_entities("p1").await.expect("get").is_empty());
    }

    #[tokio::test]
    async fn paper_entities_cascade_on_delete_paper() {
        let db = TestDb::new("entities_cascade");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "paper").await;
        let entities = crate::paper::BioEntities {
            genes: vec!["OsALS".into()],
            ..Default::default()
        };
        store
            .set_paper_entities("p1", &entities)
            .await
            .expect("set entities");
        assert_eq!(
            store
                .paper_ids_by_entity("gene", "OsALS")
                .await
                .expect("ids"),
            vec!["p1".to_string()]
        );

        store.delete_paper("p1").await.expect("delete paper");
        assert!(store.paper_entities("p1").await.expect("get").is_empty());
        assert!(
            store
                .paper_ids_by_entity("gene", "OsALS")
                .await
                .expect("ids")
                .is_empty(),
            "entity rows must be cascade-deleted with the paper"
        );
    }

    #[tokio::test]
    async fn paper_ids_by_entity_is_case_sensitive_exact_match() {
        let db = TestDb::new("entities_case");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "paper one").await;
        insert_paper_with_id(&store, "p2", "paper two").await;
        store
            .set_paper_entities(
                "p1",
                &crate::paper::BioEntities {
                    genes: vec!["OsALS".into()],
                    ..Default::default()
                },
            )
            .await
            .expect("set p1");
        store
            .set_paper_entities(
                "p2",
                &crate::paper::BioEntities {
                    genes: vec!["osals".into()],
                    ..Default::default()
                },
            )
            .await
            .expect("set p2");

        assert_eq!(
            store.paper_ids_by_entity("gene", "OsALS").await.unwrap(),
            vec!["p1".to_string()]
        );
        assert_eq!(
            store.paper_ids_by_entity("gene", "osals").await.unwrap(),
            vec!["p2".to_string()]
        );
        assert!(
            store
                .paper_ids_by_entity("gene", "OSALS")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .paper_ids_by_entity("gene", "OsAL")
                .await
                .unwrap()
                .is_empty(),
            "no substring matching"
        );
    }

    #[tokio::test]
    async fn list_papers_filtered_entity_filters_combine_with_and() {
        let db = TestDb::new("entities_filter");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "rice crispr").await;
        insert_paper_with_id(&store, "p2", "rice rnaseq").await;
        insert_paper_with_id(&store, "p3", "mouse crispr").await;

        store
            .set_paper_entities(
                "p1",
                &crate::paper::BioEntities {
                    species: vec!["Oryza sativa".into()],
                    techniques: vec!["CRISPR".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        store
            .set_paper_entities(
                "p2",
                &crate::paper::BioEntities {
                    species: vec!["Oryza sativa".into()],
                    techniques: vec!["RNA-seq".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        store
            .set_paper_entities(
                "p3",
                &crate::paper::BioEntities {
                    species: vec!["Mus musculus".into()],
                    techniques: vec!["CRISPR".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let list = |filter: crate::paper::EntityFilter, paper_type: Option<String>| {
            let store = &store;
            async move {
                store
                    .list_papers_filtered(
                        None,
                        paper_type.as_deref(),
                        None,
                        &filter,
                        None,
                        true,
                        100,
                        0,
                    )
                    .await
                    .expect("list")
                    .0
                    .into_iter()
                    .map(|p| p.id)
                    .collect::<Vec<_>>()
            }
        };

        // Single entity filter.
        let rice = list(
            crate::paper::EntityFilter {
                species: Some("Oryza sativa".into()),
                ..Default::default()
            },
            None,
        )
        .await;
        assert_eq!(rice.len(), 2);

        // Two entity filters AND together.
        let rice_crispr = list(
            crate::paper::EntityFilter {
                species: Some("Oryza sativa".into()),
                technique: Some("CRISPR".into()),
                ..Default::default()
            },
            None,
        )
        .await;
        assert_eq!(rice_crispr, vec!["p1".to_string()]);

        // No match -> empty.
        let none = list(
            crate::paper::EntityFilter {
                species: Some("Oryza sativa".into()),
                pathway: Some("photorespiration".into()),
                ..Default::default()
            },
            None,
        )
        .await;
        assert!(none.is_empty());

        // Entity filter ANDs with paper_type.
        let mut paper = store.get_paper("p1").await.unwrap().unwrap();
        paper.paper_type = Some("research_article".into());
        store.update_paper(&paper).await.unwrap();
        let typed = list(
            crate::paper::EntityFilter {
                species: Some("Oryza sativa".into()),
                ..Default::default()
            },
            Some("research_article".to_string()),
        )
        .await;
        assert_eq!(typed, vec!["p1".to_string()]);
    }

    // ========================================================================
    // Ratings & comments (user annotations)
    // ========================================================================

    /// INSERT OR REPLACE used to resolve a reindex conflict by deleting the
    /// papers row, firing ON DELETE CASCADE on paper_ratings/paper_comments.
    /// The UPSERT form must leave user annotations untouched.
    #[tokio::test]
    async fn insert_paper_reindex_preserves_annotations() {
        let db = TestDb::new("reindex_annotations");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "Original Title").await;

        store.set_paper_rating("p1", 4).await.expect("set rating");
        store
            .add_paper_comment("p1", "worth re-reading")
            .await
            .expect("add comment");

        // Simulate the indexer re-persisting the same paper id (step 8).
        let mut reindexed = Paper::new("Updated Title");
        reindexed.id = "p1".to_string();
        store
            .insert_paper(&reindexed)
            .await
            .expect("reindex insert");

        assert_eq!(
            store.get_paper_rating("p1").await.expect("get rating"),
            Some(4),
            "reindex must not wipe the user's rating"
        );
        let comments = store.list_paper_comments("p1").await.expect("list");
        assert_eq!(comments.len(), 1, "reindex must not wipe comments");
        assert_eq!(comments[0].content, "worth re-reading");
        // The row itself is updated, not just preserved.
        let paper = store.get_paper("p1").await.expect("get").expect("paper");
        assert_eq!(paper.title, "Updated Title");
    }

    #[tokio::test]
    async fn paper_rating_set_overwrites_and_deletes() {
        let db = TestDb::new("rating_roundtrip");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "paper").await;

        assert_eq!(store.get_paper_rating("p1").await.unwrap(), None);
        store.set_paper_rating("p1", 3).await.expect("set 3");
        assert_eq!(store.get_paper_rating("p1").await.unwrap(), Some(3));
        // Overwrite, not duplicate.
        store.set_paper_rating("p1", 5).await.expect("set 5");
        assert_eq!(store.get_paper_rating("p1").await.unwrap(), Some(5));
        store.delete_paper_rating("p1").await.expect("delete");
        assert_eq!(store.get_paper_rating("p1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn add_paper_comment_returns_row_and_lists_oldest_first() {
        let db = TestDb::new("comment_roundtrip");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "paper").await;

        let first = store
            .add_paper_comment("p1", "first note")
            .await
            .expect("add first");
        assert!(first.id > 0, "INSERT RETURNING must yield the row id");
        assert_eq!(first.paper_id, "p1");
        assert_eq!(first.content, "first note");
        assert!(!first.created_at.is_empty(), "created_at must be filled");

        let second = store
            .add_paper_comment("p1", "second note")
            .await
            .expect("add second");
        assert!(
            second.id > first.id,
            "ids must increase: {first:?} {second:?}"
        );

        let listed = store.list_paper_comments("p1").await.expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].content, "first note", "oldest first");
        assert_eq!(listed[1].content, "second note");
        // Unknown paper -> empty, not an error.
        assert!(store.list_paper_comments("nope").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_paper_comment_is_scoped_to_owning_paper() {
        let db = TestDb::new("comment_scoped_delete");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "paper one").await;
        insert_paper_with_id(&store, "p2", "paper two").await;

        let comment = store
            .add_paper_comment("p1", "p1 note")
            .await
            .expect("add comment");

        // Deleting through another paper's id must be a no-op.
        store
            .delete_paper_comment("p2", comment.id)
            .await
            .expect("cross-paper delete");
        assert_eq!(
            store.list_paper_comments("p1").await.unwrap().len(),
            1,
            "comment must survive a delete scoped to a different paper"
        );

        // The owning paper can delete it.
        store
            .delete_paper_comment("p1", comment.id)
            .await
            .expect("owning delete");
        assert!(store.list_paper_comments("p1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn annotation_summaries_covers_all_states() {
        let db = TestDb::new("annotation_summaries");
        let store = TursoStore::new(db.path()).await.expect("open");
        // p1: rating only; p2: comments only; p3: both; p4: neither.
        insert_paper_with_id(&store, "p1", "paper one").await;
        insert_paper_with_id(&store, "p2", "paper two").await;
        insert_paper_with_id(&store, "p3", "paper three").await;
        insert_paper_with_id(&store, "p4", "paper four").await;

        store.set_paper_rating("p1", 5).await.unwrap();
        store.add_paper_comment("p2", "note a").await.unwrap();
        store.add_paper_comment("p2", "note b").await.unwrap();
        store.set_paper_rating("p3", 2).await.unwrap();
        store.add_paper_comment("p3", "note c").await.unwrap();

        let map = store
            .annotation_summaries(&["p1", "p2", "p3", "p4", "missing"])
            .await
            .expect("summaries");

        assert_eq!(
            map.len(),
            3,
            "un-annotated and unknown ids omitted: {map:?}"
        );
        assert_eq!(
            map["p1"],
            AnnotationSummary {
                rating: Some(5),
                comment_count: 0
            }
        );
        assert_eq!(
            map["p2"],
            AnnotationSummary {
                rating: None,
                comment_count: 2
            }
        );
        assert_eq!(
            map["p3"],
            AnnotationSummary {
                rating: Some(2),
                comment_count: 1
            }
        );
        assert!(!map.contains_key("p4"));
        assert!(!map.contains_key("missing"));

        assert!(store.annotation_summaries(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clear_all_data_empties_database() {
        let db = TestDb::new("clear_all_data");
        let store = TursoStore::new(db.path()).await.expect("open");
        insert_paper_with_id(&store, "p1", "paper").await;
        store.set_paper_rating("p1", 4).await.unwrap();
        store.add_paper_comment("p1", "note").await.unwrap();
        let tree = chunk_markdown("p1", "# Intro\n\nbody\n");
        store.insert_chunks("p1", &tree).await.expect("chunks");
        store
            .set_meta("embedding_dimension", "1024")
            .await
            .expect("set meta");

        store.clear_all_data().await.expect("clear all data");

        assert_eq!(store.paper_count().await.unwrap(), 0);
        assert_eq!(store.count().await.unwrap(), 0, "vectors cleared");
        assert!(store.get_chunks("p1").await.unwrap().is_empty());
        // Ratings/comments are gone via the papers ON DELETE CASCADE.
        assert_eq!(store.get_paper_rating("p1").await.unwrap(), None);
        assert!(store.list_paper_comments("p1").await.unwrap().is_empty());
        assert!(
            store
                .annotation_summaries(&["p1"])
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.store_dimension().await,
            None,
            "dimension meta cleared"
        );
    }
}
