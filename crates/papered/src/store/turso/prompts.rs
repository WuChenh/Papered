//! Prompt CRUD operations.

use super::*;
use crate::Prompt;
use crate::error::Result;

impl TursoStore {
    pub(crate) async fn list_prompts(&self) -> Result<Vec<Prompt>> {
        self.query_all(
            "SELECT id, name, description, system_prompt, temperature, is_default, created_at, updated_at FROM prompts ORDER BY updated_at DESC",
            Vec::new(),
            "list prompts",
            parse_prompt_from_row,
        )
        .await
    }

    pub(crate) async fn get_prompt(&self, prompt_id: &str) -> Result<Option<Prompt>> {
        self.query_one(
            "SELECT id, name, description, system_prompt, temperature, is_default, created_at, updated_at FROM prompts WHERE id = ?1",
            vec![turso::Value::Text(prompt_id.to_string())],
            "get prompt",
            parse_prompt_from_row,
        )
        .await
    }

    pub(crate) async fn get_default_prompt(&self) -> Result<Option<Prompt>> {
        self.query_one(
            "SELECT id, name, description, system_prompt, temperature, is_default, created_at, updated_at FROM prompts WHERE is_default = 1 LIMIT 1",
            Vec::new(),
            "get default prompt",
            parse_prompt_from_row,
        )
        .await
    }

    pub(crate) async fn insert_prompt(&self, prompt: &Prompt) -> Result<()> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(
                "INSERT OR REPLACE INTO prompts (id, name, description, system_prompt, temperature, is_default, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .await
            .db("insert prompt")?;
        stmt.execute([
            turso::Value::Text(prompt.id.clone()),
            turso::Value::Text(prompt.name.clone()),
            turso::Value::Text(prompt.description.clone().unwrap_or_default()),
            turso::Value::Text(prompt.system_prompt.clone()),
            turso::Value::Real(prompt.temperature as f64),
            turso::Value::Integer(if prompt.is_default { 1 } else { 0 }),
            turso::Value::Text(prompt.created_at.to_rfc3339()),
            turso::Value::Text(prompt.updated_at.to_rfc3339()),
        ])
        .await
        .db("insert prompt")?;
        Ok(())
    }

    pub(crate) async fn update_prompt(&self, prompt: &Prompt) -> Result<()> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(
                "UPDATE prompts SET
                    name = ?1,
                    description = ?2,
                    system_prompt = ?3,
                    temperature = ?4,
                    updated_at = ?5
                 WHERE id = ?6",
            )
            .await
            .db("update prompt")?;
        stmt.execute([
            turso::Value::Text(prompt.name.clone()),
            turso::Value::Text(prompt.description.clone().unwrap_or_default()),
            turso::Value::Text(prompt.system_prompt.clone()),
            turso::Value::Real(prompt.temperature as f64),
            turso::Value::Text(chrono::Utc::now().to_rfc3339()),
            turso::Value::Text(prompt.id.clone()),
        ])
        .await
        .db("update prompt")?;
        Ok(())
    }

    pub(crate) async fn delete_prompt(&self, prompt_id: &str) -> Result<()> {
        self.exec(
            "DELETE FROM prompts WHERE id = ?1",
            vec![turso::Value::Text(prompt_id.to_string())],
            "delete prompt",
        )
        .await
    }

    pub(crate) async fn set_default_prompt(&self, prompt_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().await.db("set default prompt tx")?;
        tx.execute("UPDATE prompts SET is_default = 0", ())
            .await
            .db("clear default prompt")?;
        tx.execute(
            "UPDATE prompts SET is_default = 1 WHERE id = ?1",
            [turso::Value::Text(prompt_id.to_string())],
        )
        .await
        .db("set default prompt")?;
        tx.commit().await.db("set default prompt commit")?;
        Ok(())
    }
}
