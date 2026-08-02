//! Health checks, maintenance operations, and LLM call metrics.

use super::*;
use crate::error::{PaperedError, Result};

impl TursoStore {
    // ========================================================================
    // Health check
    // ========================================================================

    pub(crate) async fn papers_with_missing_files(
        &self,
    ) -> Result<Vec<crate::store::vector::PaperRef>> {
        let candidates: Vec<(String, String, String)> = self
            .query_all(
                "SELECT id, title, file_path FROM papers WHERE file_path IS NOT NULL AND file_path != ''",
                Vec::new(),
                "missing files",
                |row| {
                    Ok((
                        get_text(&row.get_value(0)?).unwrap_or_default(),
                        get_text(&row.get_value(1)?).unwrap_or_default(),
                        get_text(&row.get_value(2)?).unwrap_or_default(),
                    ))
                },
            )
            .await?;

        let mut missing = Vec::new();
        for (id, title, path) in candidates {
            let exists = tokio::fs::try_exists(&path).await.unwrap_or(false);
            if !exists {
                missing.push(crate::store::vector::PaperRef { id, title });
            }
        }
        Ok(missing)
    }

    pub(crate) async fn orphaned_data_directories(
        &self,
        data_dir: &std::path::Path,
    ) -> Result<Vec<String>> {
        use crate::util::paths::is_safe_paper_id;

        let papers_dir = data_dir.join("papers");
        if !tokio::fs::try_exists(&papers_dir).await.unwrap_or(false) {
            return Ok(Vec::new());
        }
        let mut disk_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut read_dir = tokio::fs::read_dir(&papers_dir)
            .await
            .map_err(|e| PaperedError::Indexing(format!("Failed to read papers dir: {e}")))?;
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let file_type = entry.file_type().await;
            if file_type.is_ok_and(|t| t.is_dir()) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_safe_paper_id(&name) {
                    disk_dirs.insert(name);
                } else {
                    tracing::warn!(paper_id = %name, "Skipping unsafe orphaned directory candidate");
                }
            }
        }

        if disk_dirs.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.read_lock().await;
        let mut stmt = conn
            .prepare_cached("SELECT id FROM papers")
            .await
            .db("orphaned dirs")?;
        let mut rows = stmt.query(()).await.db("orphaned dirs")?;
        let mut db_ids = std::collections::HashSet::new();
        while let Some(row) = rows.next().await.db("orphaned dirs row")? {
            if let Some(id) = get_text(&row.get_value(0)?) {
                db_ids.insert(id);
            }
        }

        Ok(disk_dirs
            .into_iter()
            .filter(|id| !db_ids.contains(id))
            .collect())
    }

    // ========================================================================
    // Maintenance
    // ========================================================================

    pub(crate) async fn optimize(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached("OPTIMIZE INDEX").await.db("optimize")?;
        stmt.execute(()).await.db("optimize")?;
        Ok(())
    }

    pub(crate) async fn store_dimension(&self) -> Option<usize> {
        match self.get_meta("embedding_dimension").await {
            Ok(Some(v)) => v.parse().ok(),
            _ => None,
        }
    }

    pub(crate) async fn clear_all_data(&self) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().await.db("clear all tx")?;
        // Child tables are deleted explicitly (papers itself is deleted last);
        // paper_ratings / paper_comments are covered by the papers cascade
        // (PRAGMA foreign_keys = ON).
        tx.execute("DELETE FROM sections", ()).await?;
        tx.execute("DELETE FROM chunks", ()).await?;
        tx.execute("DELETE FROM figures", ()).await?;
        tx.execute("DELETE FROM translations", ()).await?;
        tx.execute("DELETE FROM vectors", ()).await?;
        tx.execute("DELETE FROM paper_entities", ()).await?;
        tx.execute("DELETE FROM papers", ()).await?;
        tx.execute(
            "DELETE FROM store_meta WHERE key IN ('embedding_fingerprint', 'embedding_dimension')",
            (),
        )
        .await?;
        tx.commit().await.db("clear all commit")?;
        Ok(())
    }

    pub(crate) async fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached("INSERT OR REPLACE INTO store_meta (key, value) VALUES (?1, ?2)")
            .await
            .db("set meta")?;
        stmt.execute([
            turso::Value::Text(key.to_string()),
            turso::Value::Text(value.to_string()),
        ])
        .await
        .db("set meta")?;
        Ok(())
    }

    pub(crate) async fn get_meta(&self, key: &str) -> Result<Option<String>> {
        self.query_one(
            "SELECT value FROM store_meta WHERE key = ?1",
            vec![turso::Value::Text(key.to_string())],
            "get meta",
            |row| Ok(get_text(&row.get_value(0)?)),
        )
        .await
        .map(Option::flatten)
    }

    // ========================================================================
    // LLM call metrics
    // ========================================================================

    pub(crate) async fn insert_llm_call_metric(
        &self,
        metric: &crate::llm::metrics::LlmCallMetric,
    ) -> Result<()> {
        let opt_u32 =
            |v: Option<u32>| v.map_or(turso::Value::Null, |n| turso::Value::Integer(i64::from(n)));
        self.exec(
            "INSERT INTO llm_call_metrics \
             (created_at, kind, model, prompt_tokens, completion_tokens, latency_ms, success, error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            vec![
                turso::Value::Text(chrono::Utc::now().to_rfc3339()),
                turso::Value::Text(metric.kind.as_str().to_string()),
                turso::Value::Text(metric.model.clone()),
                opt_u32(metric.usage.prompt_tokens),
                opt_u32(metric.usage.completion_tokens),
                turso::Value::Integer(metric.latency_ms as i64),
                turso::Value::Integer(i64::from(metric.success)),
                metric
                    .error
                    .clone()
                    .map_or(turso::Value::Null, turso::Value::Text),
            ],
            "insert llm call metric",
        )
        .await
    }

    pub(crate) async fn llm_call_metrics_summary(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<crate::llm::metrics::LlmCallMetricGroup>> {
        const SELECT: &str = "SELECT kind, model, COUNT(*), SUM(success = 0), \
             COALESCE(SUM(prompt_tokens), 0), COALESCE(SUM(completion_tokens), 0), \
             AVG(latency_ms) FROM llm_call_metrics";
        const GROUP: &str = " GROUP BY kind, model ORDER BY kind, model";
        let (sql, params) = match since {
            Some(ts) => (
                format!("{SELECT} WHERE created_at >= ?1{GROUP}"),
                vec![turso::Value::Text(ts.to_rfc3339())],
            ),
            None => (format!("{SELECT}{GROUP}"), Vec::new()),
        };
        self.query_all(&sql, params, "llm call metrics summary", |row| {
            Ok(crate::llm::metrics::LlmCallMetricGroup {
                kind: get_text(&row.get_value(0)?).unwrap_or_default(),
                model: get_text(&row.get_value(1)?).unwrap_or_default(),
                calls: get_int(&row.get_value(2)?).unwrap_or(0) as u64,
                failures: get_int(&row.get_value(3)?).unwrap_or(0) as u64,
                prompt_tokens: get_int(&row.get_value(4)?).unwrap_or(0) as u64,
                completion_tokens: get_int(&row.get_value(5)?).unwrap_or(0) as u64,
                avg_latency_ms: get_real(&row.get_value(6)?).unwrap_or(0.0),
            })
        })
        .await
    }
}
