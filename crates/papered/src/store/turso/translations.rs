//! Translation storage operations.

use super::*;
use crate::error::Result;
use crate::store::vector::TranslationInfo;

fn parse_translation_from_row(row: &turso::Row) -> Result<TranslationInfo> {
    Ok(TranslationInfo {
        id: get_int(&row.get_value(0)?).unwrap_or(0),
        paper_id: get_text(&row.get_value(1)?).unwrap_or_default(),
        content_type: get_text(&row.get_value(2)?).unwrap_or_default(),
        content_ref: get_text(&row.get_value(3)?).unwrap_or_default(),
        source_hash: get_text(&row.get_value(4)?).unwrap_or_default(),
        target_language: get_text(&row.get_value(5)?).unwrap_or_default(),
        translated_text: get_text(&row.get_value(6)?).unwrap_or_default(),
        model: get_text(&row.get_value(7)?),
        created_at: get_text(&row.get_value(8)?).unwrap_or_default(),
        updated_at: get_text(&row.get_value(9)?).unwrap_or_default(),
    })
}

impl TursoStore {
    pub(crate) async fn upsert_translation(&self, t: &TranslationInfo) -> Result<()> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(
                "INSERT INTO translations (paper_id, content_type, content_ref, source_hash, target_language, translated_text, model, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now')) \
                 ON CONFLICT(paper_id, content_type, content_ref, target_language) \
                 DO UPDATE SET source_hash = excluded.source_hash, translated_text = excluded.translated_text, \
                 model = excluded.model, updated_at = excluded.updated_at",
            )
            .await
            .db("upsert translation")?;
        stmt.execute([
            turso::Value::Text(t.paper_id.clone()),
            turso::Value::Text(t.content_type.clone()),
            turso::Value::Text(t.content_ref.clone()),
            turso::Value::Text(t.source_hash.clone()),
            turso::Value::Text(t.target_language.clone()),
            turso::Value::Text(t.translated_text.clone()),
            turso::Value::Text(t.model.clone().unwrap_or_default()),
        ])
        .await
        .db("upsert translation")?;
        Ok(())
    }

    pub(crate) async fn get_translations(
        &self,
        paper_id: &str,
        lang: &str,
    ) -> Result<Vec<TranslationInfo>> {
        self.query_all(
            "SELECT id, paper_id, content_type, content_ref, source_hash, target_language, translated_text, model, created_at, updated_at \
             FROM translations WHERE paper_id = ?1 AND target_language = ?2 ORDER BY content_type, content_ref",
            vec![
                turso::Value::Text(paper_id.to_string()),
                turso::Value::Text(lang.to_string()),
            ],
            "get translations",
            parse_translation_from_row,
        )
        .await
    }

    pub(crate) async fn get_translation(
        &self,
        paper_id: &str,
        content_type: &str,
        content_ref: &str,
        lang: &str,
    ) -> Result<Option<TranslationInfo>> {
        self.query_one(
            "SELECT id, paper_id, content_type, content_ref, source_hash, target_language, translated_text, model, created_at, updated_at \
             FROM translations WHERE paper_id = ?1 AND content_type = ?2 AND content_ref = ?3 AND target_language = ?4",
            vec![
                turso::Value::Text(paper_id.to_string()),
                turso::Value::Text(content_type.to_string()),
                turso::Value::Text(content_ref.to_string()),
                turso::Value::Text(lang.to_string()),
            ],
            "get translation",
            parse_translation_from_row,
        )
        .await
    }

    pub(crate) async fn delete_translations(&self, paper_id: &str) -> Result<()> {
        self.exec(
            "DELETE FROM translations WHERE paper_id = ?1",
            vec![turso::Value::Text(paper_id.to_string())],
            "delete translations",
        )
        .await
    }

    pub(crate) async fn search_translations(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TranslationInfo>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let safe_query = sanitize_fts_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read_lock().await;
        let sql = "SELECT t.id, t.paper_id, t.content_type, t.content_ref, t.source_hash, \
             t.target_language, t.translated_text, t.model, t.created_at, t.updated_at \
             FROM translations t \
             WHERE fts_match(t.translated_text, ?1) \
             ORDER BY fts_score(t.translated_text, ?1) DESC \
             LIMIT ?2"
            .to_string();
        let mut rows = conn
            .query(
                &sql,
                [
                    turso::Value::Text(safe_query),
                    turso::Value::Integer(limit as i64),
                ],
            )
            .await
            .db("search translations")?;
        let mut results = Vec::new();
        while let Some(row) = rows.next().await.db("search translations row")? {
            results.push(parse_translation_from_row(&row)?);
        }
        Ok(results)
    }
}
