//! Export functionality for database, MinerU results, sections, figures, and tables.

use crate::error::{PaperedError, Result};
use crate::store::vector::VectorStore;
use crate::util::str_enum::StrLabel;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::fs;
use std::path::Path;

/// YAML frontmatter for full paper markdown export.
#[derive(Serialize)]
struct PaperFrontmatter {
    title: String,
    authors: Vec<String>,
    affiliations: Vec<String>,
    emails: Vec<String>,
    corresponding_author: Vec<String>,
    published_date: Option<String>,
    venue: Option<String>,
    doi: Option<String>,
    keywords: Vec<String>,
    urls: Vec<String>,
    data_availability: Option<String>,
    paper_type: Option<String>,
}

impl PaperFrontmatter {
    fn from_paper(paper: &crate::paper::Paper) -> Self {
        Self {
            title: paper.title.clone(),
            authors: paper.authors.clone(),
            affiliations: paper.affiliations.clone(),
            emails: paper.emails.clone(),
            corresponding_author: paper.corresponding_author.clone(),
            published_date: paper.published_date.clone(),
            venue: paper.venue.clone(),
            doi: paper.doi.clone(),
            keywords: paper.keywords.clone(),
            urls: paper.urls.clone(),
            data_availability: paper.data_availability.clone(),
            paper_type: paper.paper_type.clone(),
        }
    }
}

/// Export format.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ExportFormat {
    Json,
    Csv,
    Markdown,
    Sqlite,
}

/// What to export.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ExportTarget {
    Database,
    Sections,
    FullPapers,
}

/// Export request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRequest {
    pub target: ExportTarget,
    pub format: ExportFormat,
    pub destination: String,
    pub paper_ids: Option<Vec<String>>,
}

/// Export result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    pub files_created: Vec<String>,
    pub papers_exported: usize,
}

/// Perform export based on request.
pub async fn perform_export(
    store: &dyn VectorStore,
    db_path: &Path,
    request: &ExportRequest,
) -> Result<ExportResult> {
    fs::create_dir_all(&request.destination).map_err(PaperedError::Io)?;

    match request.target {
        ExportTarget::Database => export_database(db_path, &request.destination, request.format),
        ExportTarget::Sections => {
            export_sections(
                store,
                &request.destination,
                request.format,
                request.paper_ids.as_deref(),
            )
            .await
        }
        ExportTarget::FullPapers => {
            export_full_papers(
                store,
                &request.destination,
                request.format,
                request.paper_ids.as_deref(),
            )
            .await
        }
    }
}

fn export_database(
    db_path: &Path,
    destination: &str,
    format: ExportFormat,
) -> Result<ExportResult> {
    match format {
        ExportFormat::Sqlite => {
            let dest = Path::new(destination).join("papered_export.db");
            fs::copy(db_path, &dest).map_err(PaperedError::Io)?;
            Ok(ExportResult {
                files_created: vec![dest.to_string_lossy().into_owned()],
                papers_exported: 0,
            })
        }
        _ => Err(PaperedError::invalid_argument(
            "Database export only supports SQLite format",
        )),
    }
}

async fn fetch_papers_for_export(
    store: &dyn VectorStore,
    paper_ids: Option<&[String]>,
) -> Result<Vec<crate::paper::Paper>> {
    match paper_ids {
        Some(ids) => {
            let mut papers = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(p) = store.get_paper(id).await? {
                    papers.push(p);
                }
            }
            Ok(papers)
        }
        None => store.list_papers(100000, 0).await,
    }
}

async fn export_sections(
    store: &dyn VectorStore,
    destination: &str,
    format: ExportFormat,
    paper_ids: Option<&[String]>,
) -> Result<ExportResult> {
    let papers = fetch_papers_for_export(store, paper_ids).await?;

    let mut files = Vec::new();
    let mut exported = 0;

    match format {
        ExportFormat::Json => {
            let mut all_sections: Vec<serde_json::Value> = Vec::with_capacity(papers.len());
            for paper in &papers {
                let sections = store.get_sections(&paper.id).await?;
                let mut map = serde_json::Map::new();
                map.insert(
                    "paper_id".to_string(),
                    serde_json::Value::String(paper.id.clone()),
                );
                map.insert(
                    "title".to_string(),
                    serde_json::Value::String(paper.title.clone()),
                );
                let sections_json: Vec<serde_json::Value> = sections
                    .sections
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "section_type": s.section_type.as_str(),
                            "content": s.content,
                            "content_hash": s.content_hash,
                        })
                    })
                    .collect();
                map.insert(
                    "sections".to_string(),
                    serde_json::Value::Array(sections_json),
                );
                all_sections.push(serde_json::Value::Object(map));
                exported += 1;
            }
            let dest = Path::new(destination).join("sections_export.json");
            let json = serde_json::to_string_pretty(&all_sections).map_err(PaperedError::Json)?;
            fs::write(&dest, json).map_err(PaperedError::Io)?;
            files.push(dest.to_string_lossy().into_owned());
        }
        ExportFormat::Csv => {
            let dest = Path::new(destination).join("sections_export.csv");
            let mut csv = String::from("paper_id,title,section_type,content\n");
            for paper in &papers {
                let sections = store.get_sections(&paper.id).await?;
                for section in &sections.sections {
                    let content_escaped = section.content.replace('"', "\"\"").replace('\n', " ");
                    csv.push_str(&format!(
                        "{},{},{},\"{}\"\n",
                        escape_csv_field(&paper.id),
                        escape_csv_field(&paper.title),
                        section.section_type.as_str(),
                        content_escaped
                    ));
                    exported += 1;
                }
            }
            fs::write(&dest, csv).map_err(PaperedError::Io)?;
            files.push(dest.to_string_lossy().into_owned());
        }
        ExportFormat::Markdown => {
            for paper in &papers {
                let sections = store.get_sections(&paper.id).await?;
                let filename = format!("{}_sections.md", sanitize_filename(&paper.title));
                let dest = Path::new(destination).join(&filename);
                let frontmatter = PaperFrontmatter::from_paper(paper);
                let yaml = yaml_serde::to_string(&frontmatter)
                    .map_err(|e| PaperedError::Unknown(format!("YAML serialization error: {e}")))?;
                let mut md = format!("---\n{}---\n\n# Sections: {}\n\n", yaml, paper.title);
                for section in &sections.sections {
                    let _ = writeln!(
                        md,
                        "## {}\n\n{}\n",
                        section.section_type.as_str(),
                        section.content
                    );
                }
                fs::write(&dest, md).map_err(PaperedError::Io)?;
                files.push(dest.to_string_lossy().into_owned());
                exported += 1;
            }
        }
        _ => {
            return Err(PaperedError::invalid_argument(
                "Sections export supports JSON, CSV, and Markdown only",
            ));
        }
    }

    Ok(ExportResult {
        files_created: files,
        papers_exported: exported,
    })
}

async fn export_full_papers(
    store: &dyn VectorStore,
    destination: &str,
    format: ExportFormat,
    paper_ids: Option<&[String]>,
) -> Result<ExportResult> {
    let papers = fetch_papers_for_export(store, paper_ids).await?;

    let mut files = Vec::new();

    match format {
        ExportFormat::Json => {
            let mut all_papers: Vec<serde_json::Value> = Vec::with_capacity(papers.len());
            for paper in &papers {
                let sections = store.get_sections(&paper.id).await?;
                let paper_json = serde_json::json!({
                    "id": paper.id,
                    "title": paper.title,
                    "authors": paper.authors,
                    "affiliations": paper.affiliations,
                    "emails": paper.emails,
                    "published_date": paper.published_date,
                    "venue": paper.venue,
                    "doi": paper.doi,
                    "abstract": paper.abstract_text,
                    "keywords": paper.keywords,
                    "urls": paper.urls,
                    "extra": paper.extra,
                    "sections": sections.sections.iter().map(|s| serde_json::json!({
                        "section_type": s.section_type.as_str(),
                        "content": s.content,
                    })).collect::<Vec<_>>(),
                });
                all_papers.push(paper_json);
            }
            let dest = Path::new(destination).join("papers_export.json");
            let json = serde_json::to_string_pretty(&all_papers).map_err(PaperedError::Json)?;
            fs::write(&dest, json).map_err(PaperedError::Io)?;
            files.push(dest.to_string_lossy().into_owned());
        }
        ExportFormat::Markdown => {
            for paper in &papers {
                let sections = store.get_sections(&paper.id).await?;
                let filename = format!("{}.md", sanitize_filename(&paper.title));
                let dest = Path::new(destination).join(&filename);
                let frontmatter = PaperFrontmatter::from_paper(paper);
                let yaml = yaml_serde::to_string(&frontmatter)
                    .map_err(|e| PaperedError::Unknown(format!("YAML serialization error: {e}")))?;
                let mut md = format!("---\n{}---\n\n# {}\n\n", yaml, paper.title);
                if let Some(ref abstract_text) = paper.abstract_text {
                    md.push_str("## Abstract\n\n");
                    md.push_str(abstract_text);
                    md.push_str("\n\n");
                }
                md.push_str("## Sections\n\n");
                for section in &sections.sections {
                    let _ = writeln!(
                        md,
                        "### {}\n\n{}\n",
                        section.section_type.as_str(),
                        section.content
                    );
                }
                fs::write(&dest, md).map_err(PaperedError::Io)?;
                files.push(dest.to_string_lossy().into_owned());
            }
        }
        _ => {
            return Err(PaperedError::invalid_argument(
                "Full papers export supports JSON and Markdown only",
            ));
        }
    }

    Ok(ExportResult {
        files_created: files,
        papers_exported: papers.len(),
    })
}

fn escape_csv_field(field: &str) -> String {
    let needs_quotes =
        field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r');
    if needs_quotes {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            ' ' | '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .take(100)
        .collect()
}
