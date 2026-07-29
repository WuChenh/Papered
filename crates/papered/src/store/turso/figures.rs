//! Figure storage operations.

use super::*;
use crate::error::Result;
use crate::util::paths::safe_join;

impl TursoStore {
    pub(crate) async fn insert_figures(
        &self,
        paper_id: &str,
        figures: &[crate::index::multimodal::FigureInfo],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().await.db("insert figures tx")?;
        let mut stmt = tx
            .prepare_cached(
                "INSERT OR REPLACE INTO figures (id, paper_id, caption, description, image_path, page_number, bbox, figure_label, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))",
            )
            .await
            .db("insert figure")?;
        for fig in figures {
            stmt.execute([
                turso::Value::Text(fig.id.clone()),
                turso::Value::Text(paper_id.to_string()),
                turso::Value::Text(fig.caption.clone().unwrap_or_default()),
                turso::Value::Text(fig.description.clone().unwrap_or_default()),
                turso::Value::Text(fig.image_path.clone().unwrap_or_default()),
                fig.page_number
                    .map(|p| turso::Value::Integer(p as i64))
                    .unwrap_or(turso::Value::Null),
                turso::Value::Text(
                    fig.bbox
                        .as_ref()
                        .and_then(|b| serde_json::to_string(b).ok())
                        .unwrap_or_default(),
                ),
                turso::Value::Text(fig.figure_label.clone().unwrap_or_default()),
            ])
            .await
            .db("insert figure")?;
        }
        tx.commit().await.db("commit figures")?;
        Ok(())
    }

    pub(crate) async fn get_figures(
        &self,
        paper_id: &str,
    ) -> Result<Vec<crate::index::multimodal::FigureInfo>> {
        self.query_all(
            "SELECT id, paper_id, caption, description, image_path, page_number, bbox, figure_label FROM figures WHERE paper_id = ?1 ORDER BY page_number",
            vec![turso::Value::Text(paper_id.to_string())],
            "get figures",
            |row| {
                let bbox_str = get_text(&row.get_value(6)?);
                let bbox = bbox_str.and_then(|s| serde_json::from_str(&s).ok());
                Ok(crate::index::multimodal::FigureInfo {
                    id: get_text(&row.get_value(0)?).unwrap_or_default(),
                    paper_id: get_text(&row.get_value(1)?).unwrap_or_default(),
                    caption: get_text(&row.get_value(2)?),
                    description: get_text(&row.get_value(3)?),
                    image_path: get_text(&row.get_value(4)?),
                    page_number: get_int(&row.get_value(5)?).map(|p| p as u32),
                    bbox,
                    figure_label: get_text(&row.get_value(7)?),
                })
            },
        )
        .await
    }

    pub(crate) async fn delete_figures(&self, paper_id: &str) -> Result<()> {
        self.exec(
            "DELETE FROM figures WHERE paper_id = ?1",
            vec![turso::Value::Text(paper_id.to_string())],
            "delete figures",
        )
        .await
    }

    pub(crate) async fn update_figure_image_path(
        &self,
        figure_id: &str,
        image_path: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached("UPDATE figures SET image_path = ?1 WHERE id = ?2")
            .await
            .db("update figure image_path")?;
        stmt.execute([
            turso::Value::Text(image_path.to_string()),
            turso::Value::Text(figure_id.to_string()),
        ])
        .await
        .db("update figure image_path")?;
        Ok(())
    }

    pub(crate) async fn figures_with_missing_images(
        &self,
        data_dir: &std::path::Path,
    ) -> Result<Vec<(String, String)>> {
        let candidates: Vec<(String, String, String)> = self
            .query_all(
                "SELECT paper_id, id, image_path FROM figures WHERE image_path IS NOT NULL AND image_path != ''",
                Vec::new(),
                "missing images",
                |row| {
                    Ok((
                        get_text(&row.get_value(0)?).unwrap_or_default(),
                        get_text(&row.get_value(1)?).unwrap_or_default(),
                        get_text(&row.get_value(2)?).unwrap_or_default(),
                    ))
                },
            )
            .await?;

        let data_dir = data_dir.to_path_buf();
        let mut missing = Vec::new();
        for (paper_id, id, path) in candidates {
            let resolved = safe_join(&data_dir, &paper_id, &path).await;
            let is_missing = match resolved {
                Ok(abs) => !tokio::fs::try_exists(&abs).await.unwrap_or(false),
                Err(e) => {
                    tracing::debug!(paper_id = %paper_id, image_path = %path, "Unsafe figure path treated as missing: {e}");
                    true
                }
            };
            if is_missing {
                missing.push((paper_id, id));
            }
        }
        Ok(missing)
    }

    pub(crate) async fn figure_count(&self) -> Result<usize> {
        self.count_query("SELECT COUNT(*) FROM figures", Vec::new(), "figure count")
            .await
    }
}
