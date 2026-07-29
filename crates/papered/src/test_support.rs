//! Shared test helpers, compiled only under `cfg(test)`.

use crate::Prompt;
use crate::chunker::Chunk;
use crate::error::Result;
use crate::index::multimodal::FigureInfo;
use crate::paper::Paper;
use crate::paper::section::PaperSections;
use crate::store::vector::{
    AnnotationSummary, ChunkHit, DuplicatePaperInfo, PaperComment, VectorRecord,
    VectorSearchResult, VectorStore,
};
use crate::util::str_enum::StrLabel;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// In-memory `VectorStore` for tests that need a store without a database.
#[derive(Debug, Clone, Default)]
pub(crate) struct MockVectorStore {
    papers: Arc<Mutex<Vec<Paper>>>,
    sections: Arc<Mutex<HashMap<String, PaperSections>>>,
    figures: Arc<Mutex<HashMap<String, Vec<FigureInfo>>>>,
    search_results: Arc<Mutex<Vec<VectorSearchResult>>>,
    ratings: Arc<Mutex<HashMap<String, i64>>>,
    comments: Arc<Mutex<HashMap<String, Vec<PaperComment>>>>,
}

impl MockVectorStore {
    /// Stage the vector results returned by `search`/`search_with_content_type`.
    pub(crate) fn set_search_results(&self, results: Vec<VectorSearchResult>) {
        *self.search_results.lock().unwrap() = results;
    }
}

#[async_trait]
impl VectorStore for MockVectorStore {
    async fn upsert(&self, _records: &[VectorRecord]) -> Result<()> {
        Ok(())
    }
    async fn search_with_content_type(
        &self,
        _query: &[f32],
        _section_type: Option<&str>,
        _content_type: Option<&str>,
        top_k: usize,
        _min_score: f32,
    ) -> Result<Vec<VectorSearchResult>> {
        let results = self.search_results.lock().unwrap();
        Ok(results.iter().take(top_k).cloned().collect())
    }
    async fn delete_by_paper(&self, _paper_id: &str) -> Result<()> {
        Ok(())
    }
    async fn delete_by_paper_and_content_type(
        &self,
        _paper_id: &str,
        _content_type: &str,
    ) -> Result<()> {
        Ok(())
    }
    async fn count(&self) -> Result<usize> {
        Ok(0)
    }
    async fn insert_paper(&self, paper: &Paper) -> Result<()> {
        let mut papers = self.papers.lock().unwrap();
        papers.retain(|p| p.id != paper.id);
        papers.push(paper.clone());
        Ok(())
    }
    async fn get_paper(&self, paper_id: &str) -> Result<Option<Paper>> {
        let papers = self.papers.lock().unwrap();
        Ok(papers.iter().find(|p| p.id == paper_id).cloned())
    }
    async fn get_papers_by_ids(&self, ids: &[&str]) -> Result<Vec<Paper>> {
        let papers = self.papers.lock().unwrap();
        Ok(papers
            .iter()
            .filter(|p| ids.contains(&p.id.as_str()))
            .cloned()
            .collect())
    }
    async fn get_paper_by_file_hash(&self, _file_hash: &str) -> Result<Option<Paper>> {
        Ok(None)
    }
    async fn list_papers(&self, limit: usize, offset: usize) -> Result<Vec<Paper>> {
        let papers = self.papers.lock().unwrap();
        Ok(papers.iter().skip(offset).take(limit).cloned().collect())
    }
    async fn list_papers_filtered(
        &self,
        status: Option<&str>,
        _paper_type: Option<&str>,
        _keyword: Option<&str>,
        _entity_filter: &crate::paper::EntityFilter,
        _sort_by: Option<&str>,
        sort_desc: bool,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Paper>, usize)> {
        // Mirror the real store: filter by status, order by `updated_at`
        // (the default sort column), then paginate.
        let mut papers: Vec<Paper> = self.papers.lock().unwrap().clone();
        if let Some(s) = status {
            papers.retain(|p| p.status.as_str() == s);
        }
        papers.sort_by(|a, b| {
            let ord = a.updated_at.cmp(&b.updated_at);
            if sort_desc { ord.reverse() } else { ord }
        });
        let total = papers.len();
        let page = papers.into_iter().skip(offset).take(limit).collect();
        Ok((page, total))
    }
    async fn paper_count(&self) -> Result<usize> {
        Ok(self.papers.lock().unwrap().len())
    }
    async fn count_papers_by_status(&self, status: &str) -> Result<usize> {
        let papers = self.papers.lock().unwrap();
        Ok(papers
            .iter()
            .filter(|p| p.status.as_str() == status)
            .count())
    }
    async fn duplicate_scan_papers(&self) -> Result<Vec<DuplicatePaperInfo>> {
        let papers = self.papers.lock().unwrap();
        Ok(papers
            .iter()
            .map(|p| DuplicatePaperInfo {
                id: p.id.clone(),
                title: p.title.clone(),
                authors: p.authors.clone(),
                published_date: p.published_date.clone(),
                file_hash: p.file_hash.clone(),
                status: p.status.to_string(),
                updated_at: p.updated_at,
            })
            .collect())
    }
    async fn delete_paper(&self, paper_id: &str) -> Result<()> {
        let mut papers = self.papers.lock().unwrap();
        papers.retain(|p| p.id != paper_id);
        // Mirror the ON DELETE CASCADE of the real store.
        drop(papers);
        self.ratings.lock().unwrap().remove(paper_id);
        self.comments.lock().unwrap().remove(paper_id);
        Ok(())
    }
    async fn update_paper_status(
        &self,
        paper_id: &str,
        status: &str,
        _error_message: Option<&str>,
        _retry_count: Option<u32>,
    ) -> Result<()> {
        let mut papers = self.papers.lock().unwrap();
        if let Some(p) = papers.iter_mut().find(|p| p.id == paper_id) {
            p.status = status.parse().unwrap_or_default();
        }
        Ok(())
    }
    async fn update_paper_cover(&self, _paper_id: &str, _cover_path: &str) -> Result<()> {
        Ok(())
    }
    async fn set_paper_embedding_model(
        &self,
        _paper_id: &str,
        _embedding_model: &str,
    ) -> Result<()> {
        Ok(())
    }
    async fn update_paper(&self, paper: &Paper) -> Result<()> {
        let mut papers = self.papers.lock().unwrap();
        if let Some(idx) = papers.iter().position(|p| p.id == paper.id) {
            papers[idx] = paper.clone();
        }
        Ok(())
    }
    async fn update_prompt(&self, _prompt: &Prompt) -> Result<()> {
        Ok(())
    }
    async fn paper_entities(&self, paper_id: &str) -> Result<crate::paper::BioEntities> {
        // Mirror the real store: entities live apart from the `papers` row.
        // Tests stage them on the paper before insert; return a clone here so
        // `papers_entities_batch` (default trait impl) behaves like TursoStore.
        let papers = self.papers.lock().unwrap();
        Ok(papers
            .iter()
            .find(|p| p.id == paper_id)
            .map(|p| p.entities.clone())
            .unwrap_or_default())
    }
    async fn insert_sections(&self, paper_id: &str, sections: &PaperSections) -> Result<()> {
        self.sections
            .lock()
            .unwrap()
            .insert(paper_id.to_string(), sections.clone());
        Ok(())
    }
    async fn get_sections(&self, paper_id: &str) -> Result<PaperSections> {
        Ok(self
            .sections
            .lock()
            .unwrap()
            .get(paper_id)
            .cloned()
            .unwrap_or_default())
    }
    async fn delete_sections(&self, paper_id: &str) -> Result<()> {
        self.sections.lock().unwrap().remove(paper_id);
        Ok(())
    }
    async fn get_paper_vectors_with_content_type(
        &self,
        _paper_id: &str,
        _section_type: Option<&str>,
        _content_type: Option<&str>,
    ) -> Result<Vec<(Vec<f32>, String)>> {
        Ok(vec![])
    }
    async fn fulltext_search_with_snippets(
        &self,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<(Paper, f32, String)>> {
        Ok(vec![])
    }
    async fn insert_chunks(&self, _paper_id: &str, _chunks: &[Chunk]) -> Result<()> {
        Ok(())
    }
    async fn get_chunks(&self, _paper_id: &str) -> Result<Vec<Chunk>> {
        Ok(vec![])
    }
    async fn delete_chunks(&self, _paper_id: &str) -> Result<()> {
        Ok(())
    }
    async fn list_prompts(&self) -> Result<Vec<Prompt>> {
        Ok(vec![])
    }
    async fn get_prompt(&self, _prompt_id: &str) -> Result<Option<Prompt>> {
        Ok(None)
    }
    async fn get_default_prompt(&self) -> Result<Option<Prompt>> {
        Ok(None)
    }
    async fn insert_prompt(&self, _prompt: &Prompt) -> Result<()> {
        Ok(())
    }
    async fn delete_prompt(&self, _prompt_id: &str) -> Result<()> {
        Ok(())
    }
    async fn set_default_prompt(&self, _prompt_id: &str) -> Result<()> {
        Ok(())
    }
    async fn search_chunks(
        &self,
        _paper_ids: &[&str],
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<ChunkHit>> {
        Ok(vec![])
    }
    async fn insert_figures(&self, paper_id: &str, figures: &[FigureInfo]) -> Result<()> {
        self.figures
            .lock()
            .unwrap()
            .insert(paper_id.to_string(), figures.to_vec());
        Ok(())
    }
    async fn get_figures(&self, _paper_id: &str) -> Result<Vec<FigureInfo>> {
        Ok(vec![])
    }
    async fn delete_figures(&self, paper_id: &str) -> Result<()> {
        self.figures.lock().unwrap().remove(paper_id);
        Ok(())
    }
    async fn papers_without_vectors(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }
    async fn orphaned_vector_paper_ids(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }
    async fn papers_with_missing_files(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }
    async fn figures_with_missing_images(
        &self,
        _data_dir: &std::path::Path,
    ) -> Result<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn orphaned_data_directories(&self, _data_dir: &std::path::Path) -> Result<Vec<String>> {
        Ok(vec![])
    }
    async fn get_papers_by_status(&self, _status: &str) -> Result<Vec<Paper>> {
        Ok(vec![])
    }
    async fn get_papers_by_status_with_retry_below(
        &self,
        _status: &str,
        _max_retry: u32,
    ) -> Result<Vec<Paper>> {
        Ok(vec![])
    }
    async fn figure_count(&self) -> Result<usize> {
        Ok(0)
    }
    async fn get_paper_rating(&self, paper_id: &str) -> Result<Option<i64>> {
        Ok(self.ratings.lock().unwrap().get(paper_id).copied())
    }
    async fn set_paper_rating(&self, paper_id: &str, rating: i64) -> Result<()> {
        self.ratings
            .lock()
            .unwrap()
            .insert(paper_id.to_string(), rating);
        Ok(())
    }
    async fn delete_paper_rating(&self, paper_id: &str) -> Result<()> {
        self.ratings.lock().unwrap().remove(paper_id);
        Ok(())
    }
    async fn list_paper_comments(&self, paper_id: &str) -> Result<Vec<PaperComment>> {
        Ok(self
            .comments
            .lock()
            .unwrap()
            .get(paper_id)
            .cloned()
            .unwrap_or_default())
    }
    async fn add_paper_comment(&self, paper_id: &str, content: &str) -> Result<PaperComment> {
        let mut comments = self.comments.lock().unwrap();
        let entries = comments.entry(paper_id.to_string()).or_default();
        let id = entries.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        let comment = PaperComment {
            id,
            paper_id: paper_id.to_string(),
            content: content.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        entries.push(comment.clone());
        Ok(comment)
    }
    async fn delete_paper_comment(&self, paper_id: &str, comment_id: i64) -> Result<()> {
        if let Some(entries) = self.comments.lock().unwrap().get_mut(paper_id) {
            entries.retain(|c| c.id != comment_id);
        }
        Ok(())
    }
    async fn annotation_summaries(
        &self,
        paper_ids: &[&str],
    ) -> Result<HashMap<String, AnnotationSummary>> {
        let ratings = self.ratings.lock().unwrap();
        let comments = self.comments.lock().unwrap();
        let mut out = HashMap::new();
        for id in paper_ids {
            let rating = ratings.get(*id).copied();
            let comment_count = comments.get(*id).map_or(0, |c| c.len()) as i64;
            if rating.is_some() || comment_count > 0 {
                out.insert(
                    (*id).to_string(),
                    AnnotationSummary {
                        rating,
                        comment_count,
                    },
                );
            }
        }
        Ok(out)
    }
    async fn clear_all_data(&self) -> Result<()> {
        self.papers.lock().unwrap().clear();
        self.sections.lock().unwrap().clear();
        self.figures.lock().unwrap().clear();
        self.ratings.lock().unwrap().clear();
        self.comments.lock().unwrap().clear();
        Ok(())
    }
}
