//! Chunk, section, and full-text search operations.

use super::query_builder::{MAX_QUERY_VARS, placeholders};
use super::*;
use crate::error::Result;
use crate::paper::Paper;
use crate::paper::section::PaperSections;
use crate::store::vector::ChunkHit;

impl TursoStore {
    // ========================================================================
    // Section operations
    // ========================================================================

    pub(crate) async fn insert_sections(
        &self,
        paper_id: &str,
        sections: &PaperSections,
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().await.db("insert sections tx")?;
        let mut stmt = tx
            .prepare_cached(
                "INSERT OR REPLACE INTO sections (paper_id, section_type, content, content_hash, input_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            )
            .await
            .db("insert sections")?;
        for section in &sections.sections {
            stmt.execute([
                turso::Value::Text(paper_id.to_string()),
                turso::Value::Text(section.section_type.to_string()),
                turso::Value::Text(section.content.clone()),
                turso::Value::Text(section.content_hash.clone()),
                turso::Value::Text(sections.input_hash.clone().unwrap_or_default()),
            ])
            .await
            .db("insert sections")?;
        }
        tx.commit().await.db("insert sections commit")?;
        Ok(())
    }

    pub(crate) async fn get_sections(&self, paper_id: &str) -> Result<PaperSections> {
        let conn = self.read_lock().await;
        let mut stmt = conn
            .prepare_cached(
                "SELECT section_type, content, content_hash FROM sections WHERE paper_id = ?1",
            )
            .await
            .db("get sections")?;
        let mut rows = stmt
            .query([turso::Value::Text(paper_id.to_string())])
            .await
            .db("get sections")?;
        let mut sections = Vec::new();
        while let Some(row) = rows.next().await.db("get sections row")? {
            sections.push(parse_section_from_row(&row, 0)?);
        }
        Ok(PaperSections::new(sections))
    }

    pub(crate) async fn delete_sections(&self, paper_id: &str) -> Result<()> {
        self.exec(
            "DELETE FROM sections WHERE paper_id = ?1",
            vec![turso::Value::Text(paper_id.to_string())],
            "delete sections",
        )
        .await
    }

    pub(crate) async fn get_sections_batch(
        &self,
        paper_ids: &[&str],
    ) -> Result<Vec<PaperSections>> {
        if paper_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read_lock().await;
        let mut all = Vec::new();
        for batch in paper_ids.chunks(MAX_QUERY_VARS) {
            let placeholders = placeholders(batch.len());
            let sql = format!(
                "SELECT paper_id, section_type, content, content_hash FROM sections WHERE paper_id IN ({placeholders}) ORDER BY paper_id"
            );
            let params: Vec<turso::Value> = batch
                .iter()
                .map(|id| turso::Value::Text(id.to_string()))
                .collect();
            let mut rows = conn.query(&sql, params).await.db("get sections batch")?;
            let mut sections_map: std::collections::HashMap<
                String,
                Vec<crate::paper::section::Section>,
            > = std::collections::HashMap::new();
            while let Some(row) = rows.next().await.db("get sections batch row")? {
                let pid = get_text(&row.get_value(0)?).unwrap_or_default();
                sections_map
                    .entry(pid)
                    .or_default()
                    .push(parse_section_from_row(&row, 1)?);
            }
            for pid in batch {
                let sections = sections_map.remove(*pid).unwrap_or_default();
                all.push(PaperSections::new(sections));
            }
        }
        Ok(all)
    }

    // ========================================================================
    // Full-text search
    // ========================================================================

    pub(crate) async fn fulltext_search_with_snippets(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(Paper, f32, String)>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let safe_query = sanitize_fts_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read_lock().await;
        let sql = format!(
            "SELECT {PAPER_COLUMNS}, fts_score(title, abstract_text, keywords, ?1) as score, \
             fts_highlight(title, abstract_text, keywords, '<mark>', '</mark>', ?1) as snippet
             FROM papers
             WHERE fts_match(title, abstract_text, keywords, ?1)
             ORDER BY score DESC
             LIMIT ?2"
        );
        let mut stmt = conn.prepare_cached(&sql).await.db("fts snippets")?;
        let mut rows = stmt
            .query([
                turso::Value::Text(safe_query),
                turso::Value::Integer(limit as i64),
            ])
            .await
            .db("fts snippets")?;
        let mut results = Vec::new();
        while let Some(row) = rows.next().await.db("fts snippets row")? {
            let paper = super::parse_paper_from_row(&row)?;
            // score/snippet trail the 24 PAPER_COLUMNS, so they sit at indices 24 and 25.
            let score = get_real(&row.get_value(24)?).unwrap_or(0.0) as f32;
            let snippet = get_text(&row.get_value(25)?).unwrap_or_default();
            results.push((paper, score, snippet));
        }
        Ok(results)
    }

    // ========================================================================
    // Chunks
    // ========================================================================

    pub(crate) async fn insert_chunks(
        &self,
        paper_id: &str,
        chunks: &[crate::chunker::Chunk],
    ) -> Result<()> {
        let hierarchy = chunk_hierarchy(chunks);
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().await.db("insert chunks tx")?;
        let mut stmt = tx
            .prepare_cached(
                "INSERT OR REPLACE INTO chunks (id, paper_id, parent_id, chunk_type, content, start_pos, end_pos, page_number, metadata, path, level, depth, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, datetime('now'))",
            )
            .await
            .db("insert chunk")?;
        for chunk in chunks {
            let (path, level, depth) =
                hierarchy
                    .get(&chunk.id)
                    .cloned()
                    .unwrap_or((String::new(), 1, 0));
            stmt.execute([
                turso::Value::Text(chunk.id.clone()),
                turso::Value::Text(paper_id.to_string()),
                turso::Value::Text(chunk.parent_id.clone().unwrap_or_default()),
                turso::Value::Text(chunk.chunk_type.to_string()),
                turso::Value::Text(chunk.content.clone()),
                turso::Value::Integer(chunk.start_pos as i64),
                turso::Value::Integer(chunk.end_pos as i64),
                chunk
                    .page_number
                    .map(|p| turso::Value::Integer(p as i64))
                    .unwrap_or(turso::Value::Null),
                turso::Value::Text(
                    chunk
                        .metadata
                        .as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_default(),
                ),
                turso::Value::Text(path),
                turso::Value::Integer(level as i64),
                turso::Value::Integer(depth as i64),
            ])
            .await
            .db("insert chunk")?;
        }
        // Drop the statement before commit: it holds an FTS index write cursor
        // (chunks has FTS indexes) that panics if flushed after the tx commits.
        drop(stmt);
        tx.commit().await.db("commit chunks")?;
        Ok(())
    }

    pub(crate) async fn get_chunks(&self, paper_id: &str) -> Result<Vec<crate::chunker::Chunk>> {
        let conn = self.read_lock().await;
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {CHUNK_COLUMNS} FROM chunks WHERE paper_id = ?1 ORDER BY start_pos"
            ))
            .await
            .db("get chunks")?;
        let mut rows = stmt
            .query([turso::Value::Text(paper_id.to_string())])
            .await
            .db("get chunks")?;
        let mut chunks = Vec::new();
        while let Some(row) = rows.next().await.db("get chunks row")? {
            chunks.push(parse_chunk_from_row(&row)?);
        }
        Ok(chunks)
    }

    pub(crate) async fn get_chunk(
        &self,
        paper_id: &str,
        chunk_id: &str,
    ) -> Result<Option<crate::chunker::Chunk>> {
        let conn = self.read_lock().await;
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {CHUNK_COLUMNS} FROM chunks WHERE paper_id = ?1 AND id = ?2"
            ))
            .await
            .db("get chunk")?;
        let mut rows = stmt
            .query([
                turso::Value::Text(paper_id.to_string()),
                turso::Value::Text(chunk_id.to_string()),
            ])
            .await
            .db("get chunk")?;
        let Some(row) = rows.next().await.db("get chunk row")? else {
            return Ok(None);
        };
        Ok(Some(parse_chunk_from_row(&row)?))
    }

    pub(crate) async fn get_chunk_ancestors(
        &self,
        paper_id: &str,
        chunk_ids: &[&str],
    ) -> Result<Vec<crate::chunker::Chunk>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.read_lock().await;
        let mut all = Vec::new();
        // The engine does not support recursive CTEs, so walk the parent chain
        // level by level: fetch the current frontier of chunks, then queue their
        // parents as the next frontier. `seen` prevents cycles and duplicates;
        // the loop bound mirrors the old CTE's `depth < 50` guard.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut frontier: Vec<String> = chunk_ids.iter().map(|id| id.to_string()).collect();
        for _ in 0..50 {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier: Vec<String> = Vec::new();
            for batch in frontier.chunks(MAX_QUERY_VARS) {
                let placeholders = placeholders(batch.len());
                let sql = format!(
                    "SELECT id, parent_id, content FROM chunks \
                     WHERE paper_id = ?1 AND id IN ({placeholders})"
                );
                let mut params: Vec<turso::Value> = vec![turso::Value::Text(paper_id.to_string())];
                for id in batch {
                    params.push(turso::Value::Text(id.clone()));
                }
                let mut rows = conn.query(&sql, params).await.db("get chunk ancestors")?;
                while let Some(row) = rows.next().await.db("get chunk ancestors row")? {
                    let id = get_text(&row.get_value(0)?).unwrap_or_default();
                    if id.is_empty() || !seen.insert(id.clone()) {
                        continue;
                    }
                    let parent_id = get_text(&row.get_value(1)?);
                    let content = get_text(&row.get_value(2)?).unwrap_or_default();
                    if let Some(pid) = parent_id.as_ref()
                        && !pid.is_empty()
                        && !seen.contains(pid)
                    {
                        next_frontier.push(pid.clone());
                    }
                    all.push(crate::chunker::Chunk {
                        id,
                        paper_id: paper_id.to_string(),
                        parent_id,
                        chunk_type: crate::chunker::ChunkType::Paragraph,
                        content,
                        start_pos: 0,
                        end_pos: 0,
                        page_number: None,
                        metadata: None,
                    });
                }
            }
            frontier = next_frontier;
        }
        Ok(all)
    }

    pub(crate) async fn delete_chunks(&self, paper_id: &str) -> Result<()> {
        self.exec(
            "DELETE FROM chunks WHERE paper_id = ?1",
            vec![turso::Value::Text(paper_id.to_string())],
            "delete chunks",
        )
        .await
    }

    pub(crate) async fn search_papers_by_path(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let safe_query = sanitize_fts_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        // Query tokens used for Rust-side scoring. `fts_score` is not reliable
        // on secondary FTS columns in this engine, so the path index is used
        // only as a fast candidate filter and we rank by token overlap here.
        let qtokens: Vec<String> = safe_query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| t.len() >= 2)
            .collect();
        if qtokens.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.read_lock().await;
        let mut rows = conn
            .query(
                "SELECT c.paper_id, c.path
                 FROM chunks c
                 WHERE c.path IS NOT NULL AND c.path != '' AND fts_match(c.path, ?1)",
                [turso::Value::Text(safe_query)],
            )
            .await
            .db("search papers by path")?;

        use std::collections::HashMap;
        let mut best: HashMap<String, f32> = HashMap::new();
        while let Some(row) = rows.next().await.db("search papers by path row")? {
            let paper_id = get_text(&row.get_value(0)?).unwrap_or_default();
            let path = get_text(&row.get_value(1)?)
                .unwrap_or_default()
                .to_lowercase();
            if paper_id.is_empty() {
                continue;
            }
            let hits = qtokens.iter().filter(|t| path.contains(t.as_str())).count();
            if hits == 0 {
                continue;
            }
            let score = hits as f32 / qtokens.len() as f32;
            best.entry(paper_id)
                .and_modify(|s| {
                    if score > *s {
                        *s = score;
                    }
                })
                .or_insert(score);
        }

        let mut out: Vec<(String, f32)> = best.into_iter().collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(limit);
        Ok(out)
    }

    pub(crate) async fn search_chunks(
        &self,
        paper_ids: &[&str],
        query: &str,
        limit: usize,
    ) -> Result<Vec<ChunkHit>> {
        if paper_ids.is_empty() || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let safe_query = sanitize_fts_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read_lock().await;
        let mut all_hits = Vec::new();
        for batch in paper_ids.chunks(MAX_QUERY_VARS) {
            let placeholders = placeholders(batch.len());
            let sql = format!(
                "SELECT {CHUNK_COLUMNS}, fts_score(c.content, ?1) as score
                 FROM chunks c
                 WHERE c.paper_id IN ({placeholders}) AND fts_match(c.content, ?1)
                 ORDER BY score DESC
                 LIMIT ?{}",
                batch.len() + 2
            );
            let mut params: Vec<turso::Value> = vec![turso::Value::Text(safe_query.clone())];
            for id in batch {
                params.push(turso::Value::Text(id.to_string()));
            }
            params.push(turso::Value::Integer(limit as i64));

            let mut rows = conn.query(&sql, params).await.db("search chunks")?;
            while let Some(row) = rows.next().await.db("search chunks row")? {
                let chunk = parse_chunk_from_row(&row)?;
                let score = get_real(&row.get_value(9)?).unwrap_or(0.0) as f32;
                all_hits.push(ChunkHit { chunk, score });
            }
        }
        all_hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        all_hits.truncate(limit);
        Ok(all_hits)
    }

    pub(crate) async fn search_all_chunks(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ChunkHit>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let safe_query = sanitize_fts_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read_lock().await;
        let sql = format!(
            "SELECT {CHUNK_COLUMNS}, fts_score(c.content, ?1) as score
             FROM chunks c
             WHERE fts_match(c.content, ?1)
             ORDER BY score DESC
             LIMIT ?2"
        );
        let params = vec![
            turso::Value::Text(safe_query),
            turso::Value::Integer(limit as i64),
        ];
        let mut hits = Vec::new();
        let mut rows = conn.query(&sql, params).await.db("search all chunks")?;
        while let Some(row) = rows.next().await.db("search all chunks row")? {
            let chunk = parse_chunk_from_row(&row)?;
            let score = get_real(&row.get_value(9)?).unwrap_or(0.0) as f32;
            hits.push(ChunkHit { chunk, score });
        }
        Ok(hits)
    }
}

/// Compute `(path, level, depth)` for each chunk from its `parent_id` chain.
///
/// - `path` is the `" > "`-joined heading chain. For heading chunks
///   (chapter/section) it includes the chunk's own title; for body chunks
///   (paragraph/figure/table) it is the containing section's path (the body
///   text itself is excluded).
/// - `depth` counts heading ancestors (a chapter root = 0).
/// - `level = depth + 1`.
///
/// Pure and allocation-light so it can be unit-tested without a database.
pub(crate) fn chunk_hierarchy(
    chunks: &[crate::chunker::Chunk],
) -> std::collections::HashMap<String, (String, u32, u32)> {
    use std::collections::HashMap;
    let mut parent: HashMap<&str, Option<&str>> = HashMap::new();
    let mut content: HashMap<&str, &str> = HashMap::new();
    let mut is_heading: HashMap<&str, bool> = HashMap::new();
    for c in chunks {
        parent.insert(c.id.as_str(), c.parent_id.as_deref());
        content.insert(c.id.as_str(), c.content.as_str());
        is_heading.insert(
            c.id.as_str(),
            matches!(
                c.chunk_type,
                crate::chunker::ChunkType::Chapter | crate::chunker::ChunkType::Section
            ),
        );
    }

    let mut out: HashMap<String, (String, u32, u32)> = HashMap::new();
    for c in chunks {
        let mut anc: Vec<&str> = Vec::new();
        let mut p = c.parent_id.as_deref();
        let mut guard = 0;
        while let Some(pid) = p {
            guard += 1;
            if guard > 256 {
                break; // cycle / pathological guard
            }
            anc.push(content.get(pid).copied().unwrap_or(""));
            p = parent.get(pid).and_then(|pp| *pp);
        }
        anc.reverse();
        let depth = anc.len() as u32;
        let path = if is_heading.get(c.id.as_str()).copied().unwrap_or(false) {
            let mut segs = anc.clone();
            segs.push(c.content.as_str());
            segs.join(" > ")
        } else {
            anc.join(" > ")
        };
        out.insert(c.id.clone(), (path, depth + 1, depth));
    }
    out
}
