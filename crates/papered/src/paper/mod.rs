pub mod entities;
pub mod format;
pub mod mineru;
pub mod parser;
pub mod pdf_oxide;
pub mod processor;
pub mod section;
pub mod source;
pub mod status;

pub use entities::{BioEntities, EntityFilter};
pub use source::PaperSource;
pub use status::PaperStatus;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paper {
    /// Unique identifier for the paper (UUID v4).
    pub id: String,
    /// Title of the paper.
    pub title: String,
    /// List of author names.
    pub authors: Vec<String>,
    pub affiliations: Vec<String>,
    pub emails: Vec<String>,
    /// Names of corresponding authors.
    pub corresponding_author: Vec<String>,
    /// Granular publication date (e.g. "2024-03-15", "2024-03", "2024").
    pub published_date: Option<String>,
    /// Publication venue (journal, conference, etc.).
    pub venue: Option<String>,
    /// Digital Object Identifier.
    pub doi: Option<String>,
    /// Paper abstract. Only populated in metadata-only import scenarios.
    /// After full indexing (section extraction), this field is set to `None`
    /// and the abstract is stored as a `SectionType::Abstract` section instead.
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    /// URLs extracted from the paper (project pages, code repos, datasets, etc.).
    pub urls: Vec<String>,
    /// Data and code availability statement.
    pub data_availability: Option<String>,
    /// Additional metadata not covered by dedicated fields (e.g. publisher, page_count, ORCIDs, ISBN).
    /// Stored as a JSON object string for flexibility.
    pub extra: Option<String>,
    /// Filesystem path to the original document.
    pub file_path: Option<String>,
    pub file_hash: Option<String>,
    pub cover_path: Option<String>,
    /// Indexing status: indexed, processing, or failed.
    pub status: PaperStatus,
    pub error_message: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// Number of times indexing has been retried. Used to prevent infinite
    /// retry loops for papers with permanent failures (e.g., corrupted PDFs).
    pub retry_count: u32,
    /// Paper type classification from LLM extraction
    /// (research_article, review, survey, meta_analysis, tutorial, etc.).
    pub paper_type: Option<String>,
    /// Embedding model fingerprint used to generate this paper's vectors.
    /// Format: "{provider}/{model}@{dimensions}d".
    pub embedding_model: Option<String>,
    /// Import source: manual, zotero, or lattice.
    pub source: Option<PaperSource>,
    /// Bio-entities (species, genes, techniques, pathways) extracted during
    /// indexing. Not stored on the `papers` row — persisted in the
    /// `paper_entities` table and populated by detail endpoints.
    #[serde(default)]
    pub entities: BioEntities,
}

impl Paper {
    pub fn new(title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            authors: Vec::new(),
            affiliations: Vec::new(),
            emails: Vec::new(),
            corresponding_author: Vec::new(),
            published_date: None,
            venue: None,
            doi: None,
            abstract_text: None,
            keywords: Vec::new(),
            urls: Vec::new(),
            data_availability: None,
            extra: None,
            file_path: None,
            file_hash: None,
            cover_path: None,
            status: PaperStatus::Indexed,
            error_message: None,
            updated_at: now,
            retry_count: 0,
            paper_type: None,
            embedding_model: None,
            source: None,
            entities: BioEntities::default(),
        }
    }

    pub fn authors_string(&self) -> String {
        self.authors.join(", ")
    }

    pub fn keywords_string(&self) -> String {
        self.keywords.join(", ")
    }

    pub fn year_string(&self) -> String {
        self.published_date
            .as_deref()
            .and_then(|d| d.split('-').next())
            .filter(|y| y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()))
            .map_or_else(|| "N/A".to_string(), |y| y.to_string())
    }

    /// Format `published_date` according to its precision level.
    ///
    /// - `"2024-03-15"` → `"2024-03-15"`
    /// - `"2024-3"` → `"2024-3"`
    /// - `"2024"` → `"2024"`
    /// - Malformed or empty strings return `None`.
    pub fn formatted_date(&self) -> Option<String> {
        let d = self.published_date.as_deref()?.trim();
        if d.is_empty() {
            return None;
        }
        let parts: Vec<&str> = d.split('-').collect();
        if parts.len() > 3
            || parts[0].len() != 4
            || !parts[0].chars().all(|c| c.is_ascii_digit())
            || parts
                .iter()
                .skip(1)
                .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
        Some(d.to_string())
    }

    /// Build labeled metadata parts from a list of field names.
    ///
    /// The `title_label` closure receives the paper title and should return
    /// the formatted title string (e.g. `"Paper: {title}"` or `"Source 1: {title}"`).
    pub fn build_meta_parts(
        &self,
        fields: &[&str],
        title_label: impl Fn(&str) -> String,
    ) -> Vec<String> {
        let mut parts = Vec::new();
        for field in fields {
            match *field {
                "title" => {
                    if !self.title.is_empty() {
                        parts.push(title_label(&self.title));
                    }
                }
                "authors" => {
                    let s = self.authors_string();
                    if !s.is_empty() {
                        parts.push(format!("by {s}"));
                    }
                }
                "published_date" => {
                    if let Some(d) = self.formatted_date() {
                        parts.push(d);
                    }
                }
                "venue" => {
                    if let Some(ref v) = self.venue {
                        parts.push(format!("Venue: {v}"));
                    }
                }
                "affiliations" if !self.affiliations.is_empty() => {
                    parts.push(format!("Affiliations: {}", self.affiliations.join("; ")));
                }
                "emails" if !self.emails.is_empty() => {
                    parts.push(format!("Emails: {}", self.emails.join("; ")));
                }
                "doi" => {
                    if let Some(ref d) = self.doi {
                        parts.push(format!("DOI: {d}"));
                    }
                }
                "keywords" if !self.keywords.is_empty() => {
                    parts.push(format!("Keywords: {}", self.keywords_string()));
                }
                "extra" => {
                    if let Some(ref e) = self.extra {
                        parts.push(format!("Extra: {e}"));
                    }
                }
                _ => {}
            }
        }
        parts
    }
}

/// Paginated paper list response shared between daemon and CLI.
#[derive(Debug, Serialize, Deserialize)]
pub struct ListPapersResponse {
    pub papers: Vec<Paper>,
    pub total: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperSearchResult {
    pub paper: Paper,
    pub score: f32,
    pub matched_sections: Vec<MatchedSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedSection {
    pub section_type: String,
    pub content_snippet: String,
    pub score: f32,
}

/// All metadata fields available for RAG context headers, in display order.
pub const ALL_META_FIELDS: [&str; 9] = [
    "title",
    "authors",
    "published_date",
    "venue",
    "affiliations",
    "emails",
    "doi",
    "keywords",
    "extra",
];

/// Public-facing metadata fields (excludes internal `extra` blob).
pub const PUBLIC_META_FIELDS: [&str; 8] = [
    "title",
    "authors",
    "published_date",
    "venue",
    "affiliations",
    "emails",
    "doi",
    "keywords",
];

/// Default metadata fields for RAG context headers in user config.
pub fn default_include_meta_fields() -> Vec<String> {
    vec![
        "title".to_string(),
        "authors".to_string(),
        "published_date".to_string(),
        "venue".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&PaperStatus::Indexed).unwrap(),
            "\"indexed\""
        );
        assert_eq!(
            serde_json::to_string(&PaperStatus::Processing).unwrap(),
            "\"processing\""
        );
        assert_eq!(
            serde_json::to_string(&PaperStatus::Failed).unwrap(),
            "\"failed\""
        );
    }

    #[test]
    fn status_round_trips() {
        for status in [
            PaperStatus::Indexed,
            PaperStatus::Processing,
            PaperStatus::Failed,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: PaperStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn source_serializes_to_lowercase() {
        assert_eq!(
            serde_json::to_string(&PaperSource::Zotero).unwrap(),
            "\"zotero\""
        );
        assert_eq!(
            serde_json::to_string(&PaperSource::Lattice).unwrap(),
            "\"lattice\""
        );
        assert_eq!(
            serde_json::to_string(&PaperSource::Manual).unwrap(),
            "\"manual\""
        );
    }

    #[test]
    fn source_round_trips() {
        for source in [
            PaperSource::Manual,
            PaperSource::Zotero,
            PaperSource::Lattice,
        ] {
            let json = serde_json::to_string(&source).unwrap();
            let parsed: PaperSource = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, source);
        }
    }

    #[test]
    fn status_default_is_indexed() {
        assert_eq!(PaperStatus::default(), PaperStatus::Indexed);
    }

    #[test]
    fn status_rejects_unknown_variant() {
        let result: std::result::Result<PaperStatus, _> = serde_json::from_str("\"unknown\"");
        assert!(result.is_err());
    }

    #[test]
    fn source_rejects_unknown_variant() {
        let result: std::result::Result<PaperSource, _> = serde_json::from_str("\"unknown\"");
        assert!(result.is_err());
    }
}
