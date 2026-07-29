//! MCP resource handling — `paper://` URIs for paper summary, metadata,
//! extracted sections, and full text.

use papered::paper::format::{
    build_paper_bibliographic_header, build_paper_markdown, build_paper_summary,
};
use papered::paper::section::PaperSections;
use rmcp::ErrorData;
use rmcp::model::{ReadResourceResult, ResourceContents};

use crate::PaperedMcpServer;
use crate::util::McpResultExt;

/// Serialize a value to compact JSON text, logging on failure.
fn json_text(v: &impl serde::Serialize) -> String {
    crate::util::json_text(v, "resource payload")
}

/// Validate a paper ID, returning a structured MCP error on failure.
fn validate(paper_id: &str) -> Result<(), ErrorData> {
    papered::util::paths::validate_paper_id(paper_id)
        .map_err(|e| ErrorData::invalid_params(format!("Invalid paper ID: {e}"), None))
}

/// Build a plain-text metadata rendering: bibliographic metadata and the full
/// abstract, without the extracted sections overview.
fn build_paper_metadata(paper: &papered::paper::Paper, sections: &PaperSections) -> String {
    build_paper_bibliographic_header(paper, sections)
}

/// Resource template definitions matching the old `resources/templates/list` shape.
pub fn resource_templates() -> Vec<rmcp::model::ResourceTemplate> {
    vec![
        rmcp::model::ResourceTemplate::new("paper://{paper_id}/summary", "Paper Summary")
            .with_title("Paper Summary")
            .with_description(
                "Compact summary with title, authors, date, DOI, abstract, and extracted sections overview",
            )
            .with_mime_type("text/plain"),
        rmcp::model::ResourceTemplate::new("paper://{paper_id}/metadata", "Paper Metadata")
            .with_title("Paper Metadata")
            .with_description(
                "Metadata and full abstract only, no extracted sections content",
            )
            .with_mime_type("text/plain"),
        rmcp::model::ResourceTemplate::new(
            "paper://{paper_id}/extracted_sections",
            "Paper Extracted Sections",
        )
        .with_title("Paper Extracted Sections")
        .with_description(
            "All extracted sections (abstract, methodology, findings, etc.) as JSON",
        )
        .with_mime_type("application/json"),
        rmcp::model::ResourceTemplate::new("paper://{paper_id}/full_text", "Paper Full Text")
            .with_title("Paper Full Text")
            .with_description(
                "Full paper export in Markdown format with all extracted sections",
            )
            .with_mime_type("text/markdown"),
    ]
}

/// List all resources (one per paper) for `resources/list`.
pub async fn list_resources(
    server: &PaperedMcpServer,
) -> Result<Vec<rmcp::model::Resource>, ErrorData> {
    const PAGE_SIZE: usize = 100;
    let mut resources: Vec<rmcp::model::Resource> = Vec::new();
    let mut offset = 0;
    loop {
        let papers = server.store.list_papers(PAGE_SIZE, offset).await.mcp()?;
        let fetched = papers.len();
        resources.extend(papers.into_iter().map(|p| {
            rmcp::model::Resource::new(
                format!("paper://{}/summary", p.id),
                format!("Paper Summary: {}", p.title),
            )
            .with_description(format!(
                "Summary of paper: {} ({})",
                p.title,
                p.authors_string()
            ))
            .with_mime_type("text/plain")
        }));
        if fetched < PAGE_SIZE {
            break;
        }
        offset += fetched;
    }
    Ok(resources)
}

/// Read a single resource for `resources/read`.
pub async fn read_resource(
    server: &PaperedMcpServer,
    uri: &str,
) -> Result<ReadResourceResult, ErrorData> {
    let store = &server.store;

    let Some(rest) = uri.strip_prefix("paper://") else {
        return Err(ErrorData::invalid_params(
            format!("Unsupported resource URI scheme: {uri}"),
            None,
        ));
    };

    let (paper_id, resource_type) = if let Some((a, b)) = rest.split_once('/') {
        (a, Some(b))
    } else {
        (rest, None)
    };
    validate(paper_id)?;

    let paper = store
        .get_paper(paper_id)
        .await
        .mcp()?
        .ok_or_else(|| ErrorData::invalid_params(format!("Resource not found: {uri}"), None))?;

    // Bare paper JSON — no sections needed.
    let Some(rt) = resource_type.filter(|s| !s.is_empty()) else {
        return Ok(ReadResourceResult::new(vec![ResourceContents::text(
            json_text(&paper),
            uri,
        )]));
    };

    // All remaining resource types need extracted sections — fetch once.
    let sections = store.get_sections(paper_id).await.mcp()?;

    match rt {
        "summary" => {
            let summary = build_paper_summary(&paper, &sections);
            Ok(ReadResourceResult::new(vec![ResourceContents::text(
                summary, uri,
            )]))
        }
        "metadata" => {
            let metadata = build_paper_metadata(&paper, &sections);
            Ok(ReadResourceResult::new(vec![ResourceContents::text(
                metadata, uri,
            )]))
        }
        "extracted_sections" => {
            let sections_json = serde_json::json!({
                "extracted_sections": sections.to_views(),
            });
            Ok(ReadResourceResult::new(vec![
                ResourceContents::text(json_text(&sections_json), uri)
                    .with_mime_type("application/json"),
            ]))
        }
        "full_text" => {
            let markdown = build_paper_markdown(&paper, &sections);
            Ok(ReadResourceResult::new(vec![
                ResourceContents::text(markdown, uri).with_mime_type("text/markdown"),
            ]))
        }
        other => Err(ErrorData::invalid_params(
            format!("Unknown resource type: {other}"),
            None,
        )),
    }
}
