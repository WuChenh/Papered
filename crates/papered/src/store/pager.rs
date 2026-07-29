use crate::error::Result;
use crate::paper::Paper;
use crate::store::vector::VectorStore;
use std::sync::Arc;

/// Cursor-style paginator over `VectorStore::list_papers`.
pub struct PaperPager<'a> {
    store: &'a Arc<dyn VectorStore>,
    batch_size: usize,
    offset: usize,
}

impl PaperPager<'_> {
    pub fn new(store: &Arc<dyn VectorStore>, batch_size: usize) -> PaperPager<'_> {
        PaperPager {
            store,
            batch_size,
            offset: 0,
        }
    }

    /// Fetch the next batch of papers, or `None` when exhausted.
    pub async fn next_batch(&mut self) -> Result<Option<Vec<Paper>>> {
        let batch = self.store.list_papers(self.batch_size, self.offset).await?;
        if batch.is_empty() {
            return Ok(None);
        }
        self.offset += batch.len();
        Ok(Some(batch))
    }

    /// Fetch the next batch together with each paper's sections.
    pub async fn next_batch_with_sections(
        &mut self,
    ) -> Result<Option<(Vec<Paper>, Vec<crate::paper::section::PaperSections>)>> {
        let Some(batch) = self.next_batch().await? else {
            return Ok(None);
        };
        let paper_ids: Vec<&str> = batch.iter().map(|p| p.id.as_str()).collect();
        let sections = self.store.get_sections_batch(&paper_ids).await?;
        Ok(Some((batch, sections)))
    }
}
