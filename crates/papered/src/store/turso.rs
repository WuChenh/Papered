//! Turso-backed unified store — pure-Rust SQLite-compatible engine.
//!
//! Single `.db` file handling all storage needs:
//! - Structured data (papers, sections, chunks, figures, tables, prompts) via standard SQL tables
//! - Full-text search via Turso's Tantivy-backed `USING fts` indexes
//! - Vector search via Turso's built-in `vector32` / `vector_distance_cos` functions

use crate::Prompt;
use crate::error::{PaperedError, Result};
use crate::paper::Paper;
use crate::paper::PaperStatus;
use crate::paper::section::Section;
use std::path::Path;
use std::sync::Arc;

pub mod annotations;
pub mod chunks;
pub mod figures;
pub mod health;
pub mod papers;
pub mod prompts;
pub mod query_builder;
pub mod translations;
pub mod vectors;

pub(crate) const PAPER_COLUMNS: &str = concat!(
    "id, title, authors, affiliations, venue, doi, ",
    "abstract_text, keywords, urls, emails, extra, file_path, ",
    "file_hash, cover_path, status, error_message, retry_count, ",
    "paper_type, published_date, corresponding_author, ",
    "data_availability, embedding_model, source, updated_at"
);

/// Columns of the `chunks` table in canonical order (kept in sync with
/// [`parse_chunk_from_row`]).
pub(crate) const CHUNK_COLUMNS: &str =
    "id, paper_id, parent_id, chunk_type, content, start_pos, end_pos, page_number, metadata";

pub struct TursoStore {
    /// Owned `Database` handle kept alive for the lifetime of the store.
    /// The connection references this database; dropping it would invalidate
    /// active connections. This field is intentionally not read directly.
    _db: Arc<turso::Database>,
    pub(crate) conn: tokio::sync::Mutex<turso::Connection>,
}

impl TursoStore {
    pub async fn new(db_path: &Path) -> Result<Self> {
        let path_str = db_path.to_str().unwrap_or(":memory:");
        let db = turso::Builder::new_local(path_str)
            .experimental_index_method(true)
            .build()
            .await
            .db("turso open")?;

        let conn = db.connect().db("turso connect")?;

        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .db("turso pragma")?;

        Self::init_tables(&conn).await?;
        Self::seed_prompts(&conn).await?;

        Ok(Self {
            _db: Arc::new(db),
            conn: tokio::sync::Mutex::new(conn),
        })
    }

    async fn init_tables(conn: &turso::Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS papers (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                authors TEXT NOT NULL DEFAULT '[]',
                affiliations TEXT NOT NULL DEFAULT '[]',
                venue TEXT,
                doi TEXT,
                abstract_text TEXT,
                keywords TEXT NOT NULL DEFAULT '[]',
                urls TEXT NOT NULL DEFAULT '[]',
                emails TEXT NOT NULL DEFAULT '[]',
                extra TEXT,
                file_path TEXT,
                file_hash TEXT,
                cover_path TEXT,
                status TEXT NOT NULL DEFAULT 'indexed',
                error_message TEXT,
                retry_count INTEGER NOT NULL DEFAULT 0,
                paper_type TEXT,
                published_date TEXT,
                corresponding_author TEXT NOT NULL DEFAULT '[]',
                data_availability TEXT,
                embedding_model TEXT,
                source TEXT,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sections (
                id INTEGER PRIMARY KEY,
                paper_id TEXT NOT NULL,
                section_type TEXT NOT NULL,
                content TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                input_hash TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (paper_id) REFERENCES papers(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_sections_paper_id ON sections(paper_id);
            CREATE INDEX IF NOT EXISTS idx_sections_type ON sections(section_type);
            CREATE INDEX IF NOT EXISTS idx_sections_input_hash ON sections(paper_id, input_hash);
            CREATE INDEX IF NOT EXISTS idx_papers_status ON papers(status);
            CREATE INDEX IF NOT EXISTS idx_papers_file_hash ON papers(file_hash);
            CREATE INDEX IF NOT EXISTS idx_papers_updated_at ON papers(updated_at DESC);

            CREATE TABLE IF NOT EXISTS chunks (
                id TEXT PRIMARY KEY,
                paper_id TEXT NOT NULL,
                parent_id TEXT,
                chunk_type TEXT NOT NULL DEFAULT 'paragraph',
                content TEXT NOT NULL,
                start_pos INTEGER,
                end_pos INTEGER,
                page_number INTEGER,
                metadata TEXT,
                path TEXT,
                level INTEGER,
                depth INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (paper_id) REFERENCES papers(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_paper_id ON chunks(paper_id);
            CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(paper_id, path);

            CREATE TABLE IF NOT EXISTS figures (
                id TEXT PRIMARY KEY,
                paper_id TEXT NOT NULL,
                caption TEXT,
                description TEXT,
                image_path TEXT,
                page_number INTEGER,
                bbox TEXT,
                figure_label TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (paper_id) REFERENCES papers(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_figures_paper_id ON figures(paper_id);
            CREATE INDEX IF NOT EXISTS idx_figures_caption ON figures(caption);

            CREATE TABLE IF NOT EXISTS prompts (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                system_prompt TEXT NOT NULL,
                temperature REAL NOT NULL DEFAULT 0.2,
                is_default INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_prompts_default ON prompts(is_default);

            CREATE TABLE IF NOT EXISTS store_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS paper_entities (
                paper_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (paper_id, kind, value),
                FOREIGN KEY (paper_id) REFERENCES papers(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_paper_entities_kind_value
                ON paper_entities(kind, value);

            CREATE TABLE IF NOT EXISTS paper_ratings (
                paper_id TEXT PRIMARY KEY,
                rating INTEGER NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (paper_id) REFERENCES papers(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS paper_comments (
                id INTEGER PRIMARY KEY,
                paper_id TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (paper_id) REFERENCES papers(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_paper_comments_paper_id
                ON paper_comments(paper_id);

            CREATE TABLE IF NOT EXISTS llm_call_metrics (
                id INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL,
                kind TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER,
                completion_tokens INTEGER,
                latency_ms INTEGER NOT NULL,
                success INTEGER NOT NULL,
                error TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_llm_call_metrics_created_at
                ON llm_call_metrics(created_at);

            CREATE TABLE IF NOT EXISTS vectors (
                id TEXT PRIMARY KEY,
                paper_id TEXT NOT NULL,
                content_type TEXT NOT NULL,
                section_type TEXT NOT NULL,
                chunk_text TEXT,
                vector BLOB NOT NULL,
                FOREIGN KEY (paper_id) REFERENCES papers(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_vectors_paper_id ON vectors(paper_id);
            CREATE INDEX IF NOT EXISTS idx_vectors_content_type ON vectors(content_type);
            CREATE INDEX IF NOT EXISTS idx_vectors_section_type ON vectors(section_type);
            CREATE INDEX IF NOT EXISTS idx_vectors_paper_content
                ON vectors(paper_id, content_type);

            CREATE INDEX IF NOT EXISTS idx_papers_fts ON papers USING fts (title, abstract_text, keywords);
            CREATE INDEX IF NOT EXISTS idx_chunks_fts ON chunks USING fts (content);
            CREATE INDEX IF NOT EXISTS idx_chunks_path_fts ON chunks USING fts (path);

            CREATE TABLE IF NOT EXISTS translations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                paper_id TEXT NOT NULL,
                content_type TEXT NOT NULL,
                content_ref TEXT NOT NULL,
                source_hash TEXT NOT NULL,
                target_language TEXT NOT NULL,
                translated_text TEXT NOT NULL,
                model TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (paper_id) REFERENCES papers(id) ON DELETE CASCADE,
                UNIQUE(paper_id, content_type, content_ref, target_language)
            );
            CREATE INDEX IF NOT EXISTS idx_translations_paper ON translations(paper_id);
            CREATE INDEX IF NOT EXISTS idx_translations_lang ON translations(target_language);
            CREATE INDEX IF NOT EXISTS idx_translations_fts ON translations USING fts (translated_text);
            ",
        )
        .await
        .db("turso init tables")?;
        Ok(())
    }

    async fn seed_prompts(conn: &turso::Connection) -> Result<()> {
        let mut stmt = conn
            .prepare_cached("SELECT COUNT(*) FROM prompts")
            .await
            .db("seed prompts count prepare")?;
        let mut rows = stmt
            .query(Vec::<turso::Value>::new())
            .await
            .db("seed prompts count")?;
        if let Some(row) = rows.next().await.db("seed prompts count row")? {
            let count: usize =
                get_int(&row.get_value(0).db("seed prompts count value")?).unwrap_or(0) as usize;
            if count > 0 {
                return Ok(());
            }
        }
        let seeds: Vec<[&str; 6]> = vec![
            [
                "default",
                "Default research assistant",
                "The standard system prompt for answering questions with citations.",
                "You are a cognitive mirror reflecting the user's thought space. Answer based ONLY on the provided research papers.\n\nCitations: the context is organized into numbered sources (\"### Source 1:\", \"### Source 2:\", ...). Cite every non-trivial claim inline with the matching source number in square brackets, e.g. [1] or [1][3]. When the context shows a \"Section path:\" line, you may also name the section. If the context does not contain enough information, say so clearly instead of guessing. Be concise but thorough.",
                "0.2",
                "1",
            ],
            [
                "concise",
                "Concise summarizer",
                "Short, focused answers with minimal elaboration.",
                "Answer the user's question based ONLY on the provided research papers. Be extremely concise — answer in 1-3 sentences when possible. Cite sources with [number] brackets. If the papers don't contain enough information, say so clearly.",
                "0.1",
                "0",
            ],
            [
                "detailed",
                "Deep dive analyst",
                "Comprehensive analysis with thorough citations and section references.",
                "You are a thorough research analyst. Answer based ONLY on the provided research papers.\n\nCitations: the context is organized into numbered sources (\"### Source 1:\", \"### Source 2:\", ...). For every factual claim, cite the source number in brackets [1]. When the context includes a \"Section path:\" line, reference the specific section. Structure your answer with clear sections when the question warrants depth. If the context lacks sufficient information, state what is missing rather than guessing. Be thorough and precise.",
                "0.3",
                "0",
            ],
        ];
        let mut insert = conn
            .prepare_cached(
                "INSERT INTO prompts (id, name, description, system_prompt, temperature, is_default) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .await
            .db("seed prompts prepare")?;
        for s in &seeds {
            insert
                .execute([
                    turso::Value::Text((*s)[0].to_string()),
                    turso::Value::Text((*s)[1].to_string()),
                    turso::Value::Text((*s)[2].to_string()),
                    turso::Value::Text((*s)[3].to_string()),
                    turso::Value::Real((*s)[4].parse::<f64>().unwrap()),
                    turso::Value::Integer((*s)[5].parse::<i64>().unwrap()),
                ])
                .await
                .db("seed prompts insert")?;
        }
        Ok(())
    }

    // =============================================================================
    // Helpers
    // =============================================================================

    /// `prepare_cached` + `execute`, discarding rows; `ctx` labels errors.
    async fn exec(&self, sql: &str, params: Vec<turso::Value>, ctx: &'static str) -> Result<()> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached(sql).await.db(ctx)?;
        stmt.execute(params).await.db(ctx)?;
        Ok(())
    }

    /// `prepare_cached` + query one row, mapped via `parse`; `None` when empty.
    async fn query_one<T>(
        &self,
        sql: &str,
        params: Vec<turso::Value>,
        ctx: &'static str,
        parse: impl Fn(&turso::Row) -> Result<T>,
    ) -> Result<Option<T>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached(sql).await.db(ctx)?;
        let mut rows = stmt.query(params).await.db(ctx)?;
        match rows.next().await.db(ctx)? {
            Some(row) => Ok(Some(parse(&row)?)),
            None => Ok(None),
        }
    }

    /// `prepare_cached` + query all rows, mapped via `parse`.
    async fn query_all<T>(
        &self,
        sql: &str,
        params: Vec<turso::Value>,
        ctx: &'static str,
        parse: impl Fn(&turso::Row) -> Result<T>,
    ) -> Result<Vec<T>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached(sql).await.db(ctx)?;
        let mut rows = stmt.query(params).await.db(ctx)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.db(ctx)? {
            out.push(parse(&row)?);
        }
        Ok(out)
    }

    /// `SELECT COUNT(*)`-style single integer → `usize` (0 when no row).
    async fn count_query(
        &self,
        sql: &str,
        params: Vec<turso::Value>,
        ctx: &'static str,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached(sql).await.db(ctx)?;
        let mut rows = stmt.query(params).await.db(ctx)?;
        if let Some(row) = rows.next().await.db(ctx)? {
            return Ok(get_int(&row.get_value(0)?).unwrap_or(0) as usize);
        }
        Ok(0)
    }

    pub(crate) fn paper_values(paper: &Paper) -> Result<Vec<turso::Value>> {
        Ok(vec![
            turso::Value::Text(paper.id.clone()),
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
            opt_text(&paper.file_path),
            opt_text(&paper.file_hash),
            opt_text(&paper.cover_path),
            turso::Value::Text(paper.status.to_string()),
            opt_text(&paper.error_message),
            turso::Value::Integer(paper.retry_count as i64),
            opt_text(&paper.paper_type),
            opt_text(&paper.published_date),
            json_text(&paper.corresponding_author)?,
            opt_text(&paper.data_availability),
            opt_text(&paper.embedding_model),
            turso::Value::Text(paper.source.map(|s| s.to_string()).unwrap_or_default()),
            turso::Value::Text(paper.updated_at.to_rfc3339()),
        ])
    }
}

// =============================================================================
// Row parsing
// =============================================================================

pub(crate) fn parse_paper_from_row(row: &turso::Row) -> Result<Paper> {
    let v = |i| row.get_value(i).db("row get");

    Ok(Paper {
        id: get_text(&v(0)?).unwrap_or_default(),
        title: get_text(&v(1)?).unwrap_or_default(),
        authors: parse_json(&v(2)?),
        affiliations: parse_json(&v(3)?),
        venue: get_text(&v(4)?),
        doi: get_text(&v(5)?),
        abstract_text: get_text(&v(6)?),
        keywords: parse_json(&v(7)?),
        urls: parse_json(&v(8)?),
        emails: parse_json(&v(9)?),
        extra: get_text(&v(10)?),
        file_path: get_text(&v(11)?),
        file_hash: get_text(&v(12)?),
        cover_path: get_text(&v(13)?),
        status: get_text(&v(14)?)
            .map(|s| match s.parse() {
                Ok(status) => status,
                Err(err) => {
                    tracing::warn!(status = %s, error = %err, "invalid paper status in row; using default");
                    PaperStatus::default()
                }
            })
            .unwrap_or_default(),
        error_message: get_text(&v(15)?),
        retry_count: get_int(&v(16)?).unwrap_or(0) as u32,
        paper_type: get_text(&v(17)?),
        published_date: get_text(&v(18)?),
        corresponding_author: parse_json(&v(19)?),
        data_availability: get_text(&v(20)?),
        embedding_model: get_text(&v(21)?),
        source: get_text(&v(22)?).and_then(|s| match s.parse() {
            Ok(source) => Some(source),
            Err(err) => {
                tracing::warn!(source = %s, error = %err, "invalid paper source in row; using None");
                None
            }
        }),
        updated_at: get_text(&v(23)?)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(chrono::Utc::now),
        // Bio-entities live in paper_entities, not on the papers row;
        // detail endpoints populate this field separately.
        entities: crate::paper::BioEntities::default(),
    })
}

pub(crate) fn parse_prompt_from_row(row: &turso::Row) -> Result<Prompt> {
    Ok(Prompt {
        id: get_text(&row.get_value(0)?).unwrap_or_default(),
        name: get_text(&row.get_value(1)?).unwrap_or_default(),
        description: get_text(&row.get_value(2)?),
        system_prompt: get_text(&row.get_value(3)?).unwrap_or_default(),
        temperature: get_real(&row.get_value(4)?).unwrap_or(0.2) as f32,
        is_default: get_int(&row.get_value(5)?).unwrap_or(0) != 0,
        created_at: get_text(&row.get_value(6)?)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(chrono::Utc::now),
        updated_at: get_text(&row.get_value(7)?)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(chrono::Utc::now),
    })
}

pub(crate) fn parse_section_from_row(row: &turso::Row, offset: usize) -> Result<Section> {
    use crate::paper::section::SectionType;
    let section_type_str = get_text(&row.get_value(offset)?).unwrap_or_default();
    let section_type = SectionType::from_name(&section_type_str).unwrap_or(SectionType::Metadata);
    Ok(Section {
        section_type,
        content: get_text(&row.get_value(offset + 1)?).unwrap_or_default(),
        content_hash: get_text(&row.get_value(offset + 2)?).unwrap_or_default(),
    })
}

pub(crate) fn get_text(v: &turso::Value) -> Option<String> {
    match v {
        turso::Value::Text(t) if !t.is_empty() => Some(t.clone()),
        _ => None,
    }
}

pub(crate) fn get_int(v: &turso::Value) -> Option<i64> {
    match v {
        turso::Value::Integer(i) => Some(*i),
        _ => None,
    }
}

pub(crate) fn get_real(v: &turso::Value) -> Option<f64> {
    match v {
        turso::Value::Real(f) => Some(*f),
        _ => None,
    }
}

pub(crate) fn parse_json<T: Default + serde::de::DeserializeOwned>(v: &turso::Value) -> T {
    match v {
        turso::Value::Text(t) => serde_json::from_str(t).unwrap_or_default(),
        _ => T::default(),
    }
}

pub(crate) fn vector_to_sql(v: &[f32]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}

pub(crate) fn vector_from_text(text: &str) -> Option<Vec<f32>> {
    let trimmed = text.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    let inner = trimmed[1..trimmed.len() - 1].trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    inner
        .split(',')
        .map(|s| s.trim().parse::<f32>().ok())
        .collect()
}

/// Extension trait to DRY `map_err(|e| PaperedError::Database(...))` boilerplate.
pub(crate) trait DbExt<T> {
    fn db(self, ctx: &'static str) -> Result<T>;
}

impl<T> DbExt<T> for std::result::Result<T, turso::Error> {
    fn db(self, ctx: &'static str) -> Result<T> {
        self.map_err(|e| PaperedError::Database(format!("{ctx}: {e}")))
    }
}

/// Parse a `chunks` row (see [`CHUNK_COLUMNS`]) into a [`crate::chunker::Chunk`].
pub(crate) fn parse_chunk_from_row(row: &turso::Row) -> Result<crate::chunker::Chunk> {
    let metadata = get_text(&row.get_value(8)?).and_then(|s| serde_json::from_str(&s).ok());
    Ok(crate::chunker::Chunk {
        id: get_text(&row.get_value(0)?).unwrap_or_default(),
        paper_id: get_text(&row.get_value(1)?).unwrap_or_default(),
        parent_id: get_text(&row.get_value(2)?),
        chunk_type: crate::chunker::ChunkType::from_name(
            &get_text(&row.get_value(3)?).unwrap_or_default(),
        )
        .unwrap_or(crate::chunker::ChunkType::Paragraph),
        content: get_text(&row.get_value(4)?).unwrap_or_default(),
        start_pos: get_int(&row.get_value(5)?).unwrap_or(0) as usize,
        end_pos: get_int(&row.get_value(6)?).unwrap_or(0) as usize,
        page_number: get_int(&row.get_value(7)?).map(|p| p as u32),
        metadata,
    })
}

/// Encode an optional string as a `Text` value (empty string when `None`).
pub(crate) fn opt_text(value: &Option<String>) -> turso::Value {
    turso::Value::Text(value.clone().unwrap_or_default())
}

/// Serialize a value to JSON and encode it as a `Text` value.
pub(crate) fn json_text<T: serde::Serialize>(value: &T) -> Result<turso::Value> {
    Ok(turso::Value::Text(serde_json::to_string(value)?))
}

pub(crate) fn sanitize_fts_query(query: &str) -> String {
    let s = query
        .replace('"', "")
        .replace(['\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}'], "");

    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '^' | '(' | ')' | ',' | '+' | '~' => ' ',
            _ => c,
        })
        .collect();

    let raw_tokens: Vec<&str> = cleaned.split_whitespace().collect();
    let mut tokens = Vec::new();

    const FTS_RESERVED: &[&str] = &["AND", "OR", "NOT", "NEAR", "COLUMN", "COLUMNS"];

    for token in raw_tokens {
        if FTS_RESERVED.iter().any(|&w| token.eq_ignore_ascii_case(w)) {
            continue;
        }
        let trimmed = if token.starts_with('-') && token.len() > 1 {
            &token[1..]
        } else {
            token
        };
        if !trimmed.is_empty() && !is_stop_word(trimmed) {
            tokens.push(trimmed);
        }
    }

    match tokens.len() {
        0 => String::new(),
        1..=3 => tokens.join(" "),
        4..=6 => tokens.join(" OR "),
        _ => {
            let mut with_len: Vec<(&str, usize)> = tokens.iter().map(|t| (*t, t.len())).collect();
            with_len.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            with_len
                .iter()
                .take(4)
                .map(|(t, _)| *t)
                .collect::<Vec<_>>()
                .join(" AND ")
        }
    }
}

pub(crate) fn is_stop_word(word: &str) -> bool {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
        "from", "as", "is", "was", "are", "be", "been", "being", "have", "has", "had", "do",
        "does", "did", "will", "would", "could", "should", "may", "might", "can", "this", "that",
        "these", "those",
    ];
    STOP_WORDS.iter().any(|&w| word.eq_ignore_ascii_case(w))
}

/// Test database guard: owns a [`tempfile::TempDir`] holding the database
/// file (and its WAL/SHM sidecars). Dropping it removes the whole directory,
/// so tests leave zero residue in `$TMPDIR` — even on panic, where the old
/// `remove_file` cleanup never ran.
#[cfg(test)]
pub(crate) struct TestDb {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

#[cfg(test)]
impl TestDb {
    pub(crate) fn new(name: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(&format!("papered_test_{name}_"))
            .tempdir()
            .expect("create test db tempdir");
        Self {
            path: dir.path().join("test.db"),
            _dir: dir,
        }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn chunk_columns(conn: &turso::Connection) -> Vec<String> {
        let mut rows = conn.query("PRAGMA table_info(chunks)", ()).await.unwrap();
        let mut cols = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            cols.push(get_text(&row.get_value(1).unwrap()).unwrap_or_default());
        }
        cols
    }

    #[tokio::test]
    async fn init_tables_creates_chunk_heading_columns() {
        let db = TestDb::new("init_chunks");
        let store = TursoStore::new(db.path()).await.expect("open db");
        let conn = store.conn.lock().await;
        let cols = chunk_columns(&conn).await;
        for want in ["path", "level", "depth"] {
            assert!(cols.iter().any(|c| c == want), "missing {want}: {cols:?}");
        }
    }
}
