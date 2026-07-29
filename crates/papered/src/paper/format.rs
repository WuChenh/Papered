use std::fmt::Write;

use crate::paper::Paper;
use crate::paper::section::PaperSections;
use crate::util::str_enum::StrLabel;

/// Resolve the abstract for a paper, preferring `paper.abstract_text`
/// and falling back to the `SectionType::Abstract` section.
pub fn resolve_abstract(paper: &Paper, sections: &PaperSections) -> Option<String> {
    paper.abstract_text.clone().or_else(|| {
        sections
            .sections
            .iter()
            .find(|s| s.section_type == crate::paper::section::SectionType::Abstract)
            .map(|s| s.content.clone())
    })
}

/// Build the shared bibliographic header block (Title, Authors, Date, Venue,
/// DOI, Keywords, URLs, Abstract) used by both `build_paper_summary` and the
/// MCP transport's paper-metadata rendering.
pub fn build_paper_bibliographic_header(paper: &Paper, sections: &PaperSections) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "Title: {}", paper.title);
    let _ = writeln!(s, "Authors: {}", paper.authors_string());
    if let Some(ref date) = paper.formatted_date() {
        let _ = writeln!(s, "Date: {date}");
    }
    if let Some(venue) = &paper.venue {
        let _ = writeln!(s, "Venue: {venue}");
    }
    if let Some(doi) = &paper.doi {
        let _ = writeln!(s, "DOI: {doi}");
    }
    if !paper.keywords.is_empty() {
        let _ = writeln!(s, "Keywords: {}", paper.keywords_string());
    }
    if !paper.urls.is_empty() {
        let _ = writeln!(s, "URLs: {}", paper.urls.join(", "));
    }
    if let Some(abstract_text) = resolve_abstract(paper, sections) {
        s.push_str("Abstract:\n");
        s.push_str(&abstract_text);
        s.push('\n');
    }
    s
}

/// Build a compact plain-text summary of a paper.
pub fn build_paper_summary(paper: &Paper, sections: &PaperSections) -> String {
    let mut s = build_paper_bibliographic_header(paper, sections);
    let non_empty: Vec<&str> = sections
        .sections
        .iter()
        .filter(|sk| !sk.content.is_empty())
        .map(|sk| sk.section_type.as_str())
        .collect();
    if !non_empty.is_empty() {
        let _ = writeln!(s, "Sections: {}", non_empty.join(", "));
    }
    s
}

/// Build a Markdown representation of a paper with all sections.
pub fn build_paper_markdown(paper: &Paper, sections: &PaperSections) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# {}\n", paper.title);
    let _ = writeln!(md, "**Authors**: {}\n", paper.authors_string());
    if let Some(ref date) = paper.formatted_date() {
        let _ = writeln!(md, "**Date**: {date}\n");
    }
    if let Some(ref venue) = paper.venue {
        let _ = writeln!(md, "**Venue**: {venue}\n");
    }
    if let Some(ref doi) = paper.doi {
        let _ = writeln!(md, "**DOI**: {doi}\n");
    }
    if let Some(abstract_text) = resolve_abstract(paper, sections) {
        let _ = writeln!(md, "## Abstract\n\n{abstract_text}\n");
    }
    if !paper.keywords.is_empty() {
        let _ = writeln!(md, "**Keywords**: {}\n", paper.keywords_string());
    }
    if !paper.urls.is_empty() {
        let _ = writeln!(md, "**URLs**: {}\n", paper.urls.join(", "));
    }
    for section in &sections.sections {
        if !section.content.is_empty() {
            let _ = writeln!(
                md,
                "## {}\n\n{}\n",
                section.section_type.as_str(),
                section.content
            );
        }
    }
    md
}
