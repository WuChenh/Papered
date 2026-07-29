//! Ratings, comments, and annotation summaries.

use super::query_builder::{MAX_QUERY_VARS, placeholders};
use super::*;
use crate::error::{PaperedError, Result};
use crate::store::vector::{AnnotationSummary, PaperComment};

/// Parse a `paper_comments` row `(id, paper_id, content, created_at)`.
fn parse_comment_from_row(row: &turso::Row) -> Result<PaperComment> {
    Ok(PaperComment {
        id: get_int(&row.get_value(0)?).unwrap_or(0),
        paper_id: get_text(&row.get_value(1)?).unwrap_or_default(),
        content: get_text(&row.get_value(2)?).unwrap_or_default(),
        created_at: get_text(&row.get_value(3)?).unwrap_or_default(),
    })
}

impl TursoStore {
    pub(crate) async fn get_paper_rating(&self, paper_id: &str) -> Result<Option<i64>> {
        self.query_one(
            "SELECT rating FROM paper_ratings WHERE paper_id = ?1",
            vec![turso::Value::Text(paper_id.to_string())],
            "get paper rating",
            |row| Ok(get_int(&row.get_value(0)?).unwrap_or(0)),
        )
        .await
    }

    pub(crate) async fn set_paper_rating(&self, paper_id: &str, rating: i64) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().await.db("set rating tx")?;
        tx.execute(
            "DELETE FROM paper_ratings WHERE paper_id = ?1",
            [turso::Value::Text(paper_id.to_string())],
        )
        .await
        .db("delete rating")?;
        tx.execute(
            "INSERT INTO paper_ratings (paper_id, rating) VALUES (?1, ?2)",
            [
                turso::Value::Text(paper_id.to_string()),
                turso::Value::Integer(rating),
            ],
        )
        .await
        .db("insert rating")?;
        tx.commit().await.db("set rating commit")?;
        Ok(())
    }

    pub(crate) async fn delete_paper_rating(&self, paper_id: &str) -> Result<()> {
        self.exec(
            "DELETE FROM paper_ratings WHERE paper_id = ?1",
            vec![turso::Value::Text(paper_id.to_string())],
            "delete paper rating",
        )
        .await
    }

    pub(crate) async fn list_paper_comments(&self, paper_id: &str) -> Result<Vec<PaperComment>> {
        self.query_all(
            "SELECT id, paper_id, content, created_at FROM paper_comments \
             WHERE paper_id = ?1 ORDER BY id ASC",
            vec![turso::Value::Text(paper_id.to_string())],
            "list paper comments",
            parse_comment_from_row,
        )
        .await
    }

    pub(crate) async fn add_paper_comment(
        &self,
        paper_id: &str,
        content: &str,
    ) -> Result<PaperComment> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "INSERT INTO paper_comments (paper_id, content) VALUES (?1, ?2) \
                 RETURNING id, paper_id, content, created_at",
                [
                    turso::Value::Text(paper_id.to_string()),
                    turso::Value::Text(content.to_string()),
                ],
            )
            .await
            .db("add comment")?;
        match rows.next().await.db("comment readback row")? {
            Some(row) => parse_comment_from_row(&row),
            None => Err(PaperedError::Database(
                "INSERT RETURNING returned no row".to_string(),
            )),
        }
    }

    pub(crate) async fn delete_paper_comment(&self, paper_id: &str, comment_id: i64) -> Result<()> {
        self.exec(
            "DELETE FROM paper_comments WHERE id = ?1 AND paper_id = ?2",
            vec![
                turso::Value::Integer(comment_id),
                turso::Value::Text(paper_id.to_string()),
            ],
            "delete paper comment",
        )
        .await
    }

    pub(crate) async fn annotation_summaries(
        &self,
        paper_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, AnnotationSummary>> {
        let mut out = std::collections::HashMap::new();
        if paper_ids.is_empty() {
            return Ok(out);
        }
        // One aggregate query per batch — never a per-id loop.
        for batch in paper_ids.chunks(MAX_QUERY_VARS) {
            let sql = format!(
                "SELECT p.id, r.rating, COUNT(c.id) \
                 FROM papers p \
                 LEFT JOIN paper_ratings r ON r.paper_id = p.id \
                 LEFT JOIN paper_comments c ON c.paper_id = p.id \
                 WHERE p.id IN ({}) \
                 GROUP BY p.id \
                 HAVING r.rating IS NOT NULL OR COUNT(c.id) > 0",
                placeholders(batch.len())
            );
            let params: Vec<turso::Value> = batch
                .iter()
                .map(|id| turso::Value::Text((*id).to_string()))
                .collect();
            let rows = self
                .query_all(&sql, params, "annotation summaries", |row| {
                    Ok((
                        get_text(&row.get_value(0)?).unwrap_or_default(),
                        get_int(&row.get_value(1)?),
                        get_int(&row.get_value(2)?).unwrap_or(0),
                    ))
                })
                .await?;
            for (id, rating, comment_count) in rows {
                out.insert(
                    id,
                    AnnotationSummary {
                        rating,
                        comment_count,
                    },
                );
            }
        }
        Ok(out)
    }
}
