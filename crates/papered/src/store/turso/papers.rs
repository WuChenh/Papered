//! Paper metadata CRUD, entity operations, and status queries.

use super::query_builder::{MAX_QUERY_VARS, SqlFilter, placeholders};
use super::*;
use crate::error::Result;
use crate::paper::Paper;
use crate::store::vector::DuplicatePaperInfo;

impl TursoStore {
    /// `UPDATE papers SET {column} = ?1, updated_at = now WHERE id = ?3`.
    /// `column` must be a fixed column name — it is interpolated into the SQL.
    pub(crate) async fn update_paper_column(
        &self,
        paper_id: &str,
        column: &str,
        value: &str,
        ctx: &'static str,
    ) -> Result<()> {
        self.exec(
            &format!("UPDATE papers SET {column} = ?1, updated_at = ?2 WHERE id = ?3"),
            vec![
                turso::Value::Text(value.to_string()),
                turso::Value::Text(chrono::Utc::now().to_rfc3339()),
                turso::Value::Text(paper_id.to_string()),
            ],
            ctx,
        )
        .await
    }

    pub(crate) async fn insert_paper(&self, paper: &Paper) -> Result<()> {
        let conn = self.conn.lock().await;
        let values = Self::paper_values(paper)?;
        // True UPSERT, not INSERT OR REPLACE: REPLACE resolves a conflict by
        // deleting the old row first, which fires ON DELETE CASCADE on the
        // child tables (paper_ratings, paper_comments, …) and silently wipes
        // user annotations on every reindex. ON CONFLICT DO UPDATE keeps the
        // row (and its children) intact. Every column except the primary key
        // is refreshed — the indexer supplies the full Paper, and `papers`
        // has no insert-only columns (no created_at).
        let updates = PAPER_COLUMNS
            .split(", ")
            .filter(|col| *col != "id")
            .map(|col| format!("{col} = excluded.{col}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO papers ({PAPER_COLUMNS}) VALUES ({}) \
             ON CONFLICT(id) DO UPDATE SET {updates}",
            (1..=values.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut stmt = conn.prepare_cached(&sql).await.db("insert paper")?;
        stmt.execute(values).await.db("insert paper")?;
        Ok(())
    }

    pub(crate) async fn get_paper(&self, paper_id: &str) -> Result<Option<Paper>> {
        self.query_one(
            &format!("SELECT {PAPER_COLUMNS} FROM papers WHERE id = ?1 LIMIT 1"),
            vec![turso::Value::Text(paper_id.to_string())],
            "get paper",
            parse_paper_from_row,
        )
        .await
    }

    pub(crate) async fn get_papers_by_ids(&self, ids: &[&str]) -> Result<Vec<Paper>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut all = Vec::new();
        for batch in ids.chunks(MAX_QUERY_VARS) {
            let placeholders = placeholders(batch.len());
            let sql = format!("SELECT {PAPER_COLUMNS} FROM papers WHERE id IN ({placeholders})");
            let params: Vec<turso::Value> = batch
                .iter()
                .map(|id| turso::Value::Text(id.to_string()))
                .collect();
            all.extend(
                self.query_all(&sql, params, "get papers by ids", parse_paper_from_row)
                    .await?,
            );
        }
        Ok(all)
    }

    pub(crate) async fn get_paper_by_file_hash(&self, file_hash: &str) -> Result<Option<Paper>> {
        self.query_one(
            &format!("SELECT {PAPER_COLUMNS} FROM papers WHERE file_hash = ?1 LIMIT 1"),
            vec![turso::Value::Text(file_hash.to_string())],
            "get paper by hash",
            parse_paper_from_row,
        )
        .await
    }

    pub(crate) async fn list_papers(&self, limit: usize, offset: usize) -> Result<Vec<Paper>> {
        self.query_all(
            &format!(
                "SELECT {PAPER_COLUMNS} FROM papers ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2"
            ),
            vec![
                turso::Value::Integer(limit as i64),
                turso::Value::Integer(offset as i64),
            ],
            "list papers",
            parse_paper_from_row,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn list_papers_filtered(
        &self,
        status: Option<&str>,
        paper_type: Option<&str>,
        keyword: Option<&str>,
        entity_filter: &crate::paper::EntityFilter,
        sort_by: Option<&str>,
        sort_desc: bool,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Paper>, usize)> {
        let conn = self.read_lock().await;
        let mut filter = SqlFilter::new();

        if let Some(s) = status {
            filter.eq("status", turso::Value::Text(s.to_string()));
        }
        if let Some(pt) = paper_type {
            filter.eq("paper_type", turso::Value::Text(pt.to_string()));
        }
        if let Some(kw) = keyword
            && !kw.is_empty()
        {
            // `keywords` is stored as a JSON array string (e.g. `["a","b"]`).
            // Escape LIKE wildcards so that `%` and `_` in the keyword are
            // treated literally. Then match the keyword as a complete JSON
            // array element — "keyword" must be a full quoted string value,
            // not a substring of a longer keyword (e.g. "gene" must NOT match
            // "gene editing"). Four boundary patterns cover every position in
            // the array: first element, middle element, last element, sole element.
            let esc = kw
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let first = format!("%[\"{esc}\",%");
            let middle = format!("%,\"{esc}\",%");
            let last = format!("%,\"{esc}\"]%");
            let sole = format!("%[\"{esc}\"]%");
            filter.raw(
                "(keywords LIKE ? ESCAPE '\\' OR keywords LIKE ? ESCAPE '\\' \
                 OR keywords LIKE ? ESCAPE '\\' OR keywords LIKE ? ESCAPE '\\')",
                vec![
                    turso::Value::Text(first),
                    turso::Value::Text(middle),
                    turso::Value::Text(last),
                    turso::Value::Text(sole),
                ],
            );
        }
        for (kind, value) in entity_filter.pairs() {
            filter.raw(
                "EXISTS (SELECT 1 FROM paper_entities pe \
                 WHERE pe.paper_id = papers.id AND pe.kind = ? AND pe.value = ?)",
                vec![
                    turso::Value::Text(kind.to_string()),
                    turso::Value::Text(value.to_string()),
                ],
            );
        }

        let params = filter.params();
        let where_clause = filter.where_clause(0);

        let order_clause = match sort_by {
            Some("title") => {
                if sort_desc {
                    "title DESC"
                } else {
                    "title ASC"
                }
            }
            Some("published_date") => {
                if sort_desc {
                    "published_date DESC"
                } else {
                    "published_date ASC"
                }
            }
            _ => {
                if sort_desc {
                    "updated_at DESC"
                } else {
                    "updated_at ASC"
                }
            }
        };

        let count_sql = format!("SELECT COUNT(*) FROM papers {where_clause}");
        let mut count_rows = conn
            .query(&count_sql, params.clone())
            .await
            .db("filter count")?;
        let total = if let Some(row) = count_rows.next().await.db("filter count row")? {
            get_int(&row.get_value(0)?).unwrap_or(0) as usize
        } else {
            0
        };

        let mut select_params = params.clone();
        select_params.push(turso::Value::Integer(limit as i64));
        select_params.push(turso::Value::Integer(offset as i64));
        let sql = format!(
            "SELECT {PAPER_COLUMNS} FROM papers {where_clause} ORDER BY {order_clause} LIMIT ?{} OFFSET ?{}",
            params.len() + 1,
            params.len() + 2
        );
        let mut rows = conn.query(&sql, select_params).await.db("list filtered")?;
        let mut papers = Vec::new();
        while let Some(row) = rows.next().await.db("list filtered row")? {
            papers.push(parse_paper_from_row(&row)?);
        }
        Ok((papers, total))
    }

    pub(crate) async fn paper_count(&self) -> Result<usize> {
        self.count_query("SELECT COUNT(*) FROM papers", Vec::new(), "paper count")
            .await
    }

    pub(crate) async fn count_papers_by_status(&self, status: &str) -> Result<usize> {
        self.count_query(
            "SELECT COUNT(*) FROM papers WHERE status = ?1",
            vec![turso::Value::Text(status.to_string())],
            "count by status",
        )
        .await
    }

    pub(crate) async fn duplicate_scan_papers(&self) -> Result<Vec<DuplicatePaperInfo>> {
        self.query_all(
            concat!(
                "SELECT id, title, authors, published_date, file_hash, status, updated_at ",
                "FROM papers"
            ),
            Vec::new(),
            "duplicate scan",
            |row| {
                let updated_at = get_text(&row.get_value(6)?)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(chrono::Utc::now);
                Ok(DuplicatePaperInfo {
                    id: get_text(&row.get_value(0)?).unwrap_or_default(),
                    title: get_text(&row.get_value(1)?).unwrap_or_default(),
                    authors: parse_json(&row.get_value(2)?),
                    published_date: get_text(&row.get_value(3)?),
                    file_hash: get_text(&row.get_value(4)?),
                    status: get_text(&row.get_value(5)?).unwrap_or_default(),
                    updated_at,
                })
            },
        )
        .await
    }

    pub(crate) async fn delete_paper(&self, paper_id: &str) -> Result<()> {
        self.exec(
            "DELETE FROM papers WHERE id = ?1",
            vec![turso::Value::Text(paper_id.to_string())],
            "delete paper",
        )
        .await
    }

    /// Delete many papers as batched `DELETE ... WHERE id IN (...)` statements.
    /// Each statement auto-commits once, so the tantivy-backed FTS cascade
    /// (chunks, figures, translations) hits one commit per statement instead
    /// of one per paper. An explicit transaction is deliberately avoided:
    /// libsql's FTS Drop hook asserts on cascade flushes inside manual
    /// transactions (turso_core 0.7.2 `index_method/fts.rs` "transaction
    /// already committed, cannot flush").
    pub(crate) async fn delete_papers(&self, paper_ids: &[&str]) -> Result<()> {
        // Keep well under libsql's 999 host-parameter limit.
        const MAX_IDS_PER_STMT: usize = 500;
        for chunk in paper_ids.chunks(MAX_IDS_PER_STMT) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "DELETE FROM papers WHERE id IN ({})",
                placeholders.join(", ")
            );
            let params: Vec<turso::Value> = chunk
                .iter()
                .map(|id| turso::Value::Text(id.to_string()))
                .collect();
            self.exec(&sql, params, "delete papers batch").await?;
        }
        Ok(())
    }

    pub(crate) async fn update_paper_status(
        &self,
        paper_id: &str,
        status: &str,
        error_message: Option<&str>,
        retry_count: Option<u32>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let updated_at = chrono::Utc::now().to_rfc3339();
        let error = error_message.unwrap_or("");
        if let Some(retry) = retry_count {
            let mut stmt = conn
                .prepare_cached(
                    "UPDATE papers SET status = ?1, error_message = ?2, retry_count = ?3, updated_at = ?4 WHERE id = ?5",
                )
                .await
                .db("update status")?;
            stmt.execute([
                turso::Value::Text(status.to_string()),
                turso::Value::Text(error.to_string()),
                turso::Value::Integer(retry as i64),
                turso::Value::Text(updated_at),
                turso::Value::Text(paper_id.to_string()),
            ])
            .await
            .db("update status")?;
        } else {
            // Leave retry_count untouched — it is NOT NULL, so binding NULL here
            // would fail the whole update and strand the paper in 'processing'.
            let mut stmt = conn
                .prepare_cached(
                    "UPDATE papers SET status = ?1, error_message = ?2, updated_at = ?3 WHERE id = ?4",
                )
                .await
                .db("update status")?;
            stmt.execute([
                turso::Value::Text(status.to_string()),
                turso::Value::Text(error.to_string()),
                turso::Value::Text(updated_at),
                turso::Value::Text(paper_id.to_string()),
            ])
            .await
            .db("update status")?;
        }
        Ok(())
    }

    pub(crate) async fn update_paper_cover(&self, paper_id: &str, cover_path: &str) -> Result<()> {
        self.update_paper_column(paper_id, "cover_path", cover_path, "update cover")
            .await
    }

    pub(crate) async fn set_paper_embedding_model(
        &self,
        paper_id: &str,
        embedding_model: &str,
    ) -> Result<()> {
        self.update_paper_column(
            paper_id,
            "embedding_model",
            embedding_model,
            "set embedding model",
        )
        .await
    }

    pub(crate) async fn update_paper(&self, paper: &Paper) -> Result<()> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(
                "UPDATE papers SET
                title = ?1,
                authors = ?2,
                affiliations = ?3,
                venue = ?4,
                doi = ?5,
                abstract_text = ?6,
                keywords = ?7,
                urls = ?8,
                emails = ?9,
                extra = ?10,
                paper_type = ?11,
                published_date = ?12,
                corresponding_author = ?13,
                data_availability = ?14,
                embedding_model = ?15,
                source = ?16,
                cover_path = ?17,
                updated_at = ?18
             WHERE id = ?19",
            )
            .await
            .db("update paper")?;
        stmt.execute([
            turso::Value::Text(paper.title.clone()),
            json_text(&paper.authors)?,
            json_text(&paper.affiliations)?,
            opt_text(&paper.venue),
            opt_text(&paper.doi),
            opt_text(&paper.abstract_text),
            json_text(&paper.keywords)?,
            json_text(&paper.urls)?,
            json_text(&paper.emails)?,
            opt_text(&paper.extra),
            opt_text(&paper.paper_type),
            opt_text(&paper.published_date),
            json_text(&paper.corresponding_author)?,
            opt_text(&paper.data_availability),
            opt_text(&paper.embedding_model),
            turso::Value::Text(paper.source.map(|s| s.to_string()).unwrap_or_default()),
            opt_text(&paper.cover_path),
            turso::Value::Text(chrono::Utc::now().to_rfc3339()),
            turso::Value::Text(paper.id.clone()),
        ])
        .await
        .db("update paper")?;
        Ok(())
    }

    // ========================================================================
    // Bio-entities
    // ========================================================================

    pub(crate) async fn set_paper_entities(
        &self,
        paper_id: &str,
        entities: &crate::paper::BioEntities,
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().await.db("set entities tx")?;
        tx.execute(
            "DELETE FROM paper_entities WHERE paper_id = ?1",
            [turso::Value::Text(paper_id.to_string())],
        )
        .await
        .db("delete entities")?;
        for (kind, value) in entities.pairs() {
            tx.execute(
                "INSERT INTO paper_entities (paper_id, kind, value) VALUES (?1, ?2, ?3)",
                [
                    turso::Value::Text(paper_id.to_string()),
                    turso::Value::Text(kind.to_string()),
                    turso::Value::Text(value.to_string()),
                ],
            )
            .await
            .db("insert entity")?;
        }
        tx.commit().await.db("set entities commit")?;
        Ok(())
    }

    pub(crate) async fn paper_entities(&self, paper_id: &str) -> Result<crate::paper::BioEntities> {
        // rowid ordering preserves first-seen (insertion) order within a kind.
        let rows: Vec<(String, String)> = self
            .query_all(
                "SELECT kind, value FROM paper_entities WHERE paper_id = ?1 ORDER BY rowid",
                vec![turso::Value::Text(paper_id.to_string())],
                "paper entities",
                |row| {
                    let kind = get_text(&row.get_value(0)?).unwrap_or_default();
                    let value = get_text(&row.get_value(1)?).unwrap_or_default();
                    Ok((kind, value))
                },
            )
            .await?;
        let mut entities = crate::paper::BioEntities::default();
        for (kind, value) in rows {
            if !kind.is_empty() && !value.is_empty() {
                entities.insert(&kind, value);
            }
        }
        Ok(entities)
    }

    pub(crate) async fn papers_entities_batch(
        &self,
        paper_ids: &[String],
    ) -> Result<std::collections::HashMap<String, crate::paper::BioEntities>> {
        let mut out = std::collections::HashMap::new();
        if paper_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.read_lock().await;
        // One aggregate query per batch — never a per-id loop.
        for batch in paper_ids.chunks(MAX_QUERY_VARS) {
            let placeholders = placeholders(batch.len());
            let sql = format!(
                "SELECT paper_id, kind, value FROM paper_entities \
                 WHERE paper_id IN ({placeholders}) ORDER BY rowid"
            );
            let params: Vec<turso::Value> = batch
                .iter()
                .map(|id| turso::Value::Text(id.clone()))
                .collect();
            let mut rows = conn.query(&sql, params).await.db("entities batch")?;
            while let Some(row) = rows.next().await.db("entities batch row")? {
                let paper_id = get_text(&row.get_value(0)?).unwrap_or_default();
                let kind = get_text(&row.get_value(1)?).unwrap_or_default();
                let value = get_text(&row.get_value(2)?).unwrap_or_default();
                if kind.is_empty() || value.is_empty() {
                    continue;
                }
                out.entry(paper_id)
                    .or_insert_with(crate::paper::BioEntities::default)
                    .insert(&kind, value);
            }
        }
        Ok(out)
    }

    pub(crate) async fn paper_ids_by_entity(&self, kind: &str, value: &str) -> Result<Vec<String>> {
        self.query_all(
            "SELECT paper_id FROM paper_entities WHERE kind = ?1 AND value = ?2",
            vec![
                turso::Value::Text(kind.to_string()),
                turso::Value::Text(value.to_string()),
            ],
            "paper ids by entity",
            |row| Ok(get_text(&row.get_value(0)?).unwrap_or_default()),
        )
        .await
    }

    pub(crate) async fn paper_ids_by_paper_type(&self, paper_type: &str) -> Result<Vec<String>> {
        self.query_all(
            "SELECT id FROM papers WHERE paper_type = ?1",
            vec![turso::Value::Text(paper_type.to_string())],
            "paper ids by type",
            |row| Ok(get_text(&row.get_value(0)?).unwrap_or_default()),
        )
        .await
    }

    // ========================================================================
    // Status queries
    // ========================================================================

    pub(crate) async fn get_papers_by_status(&self, status: &str) -> Result<Vec<Paper>> {
        self.query_all(
            &format!(
                "SELECT {PAPER_COLUMNS} FROM papers WHERE status = ?1 ORDER BY updated_at DESC"
            ),
            vec![turso::Value::Text(status.to_string())],
            "get by status",
            parse_paper_from_row,
        )
        .await
    }

    pub(crate) async fn get_papers_by_status_with_retry_below(
        &self,
        status: &str,
        max_retry: u32,
    ) -> Result<Vec<Paper>> {
        self.query_all(
            &format!(
                "SELECT {PAPER_COLUMNS} FROM papers WHERE status = ?1 AND retry_count < ?2 ORDER BY updated_at DESC"
            ),
            vec![
                turso::Value::Text(status.to_string()),
                turso::Value::Integer(max_retry as i64),
            ],
            "get by status retry",
            parse_paper_from_row,
        )
        .await
    }
}
