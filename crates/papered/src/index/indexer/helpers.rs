//! Helper functions for the indexing pipeline.

use crate::error::PaperedError;
use crate::json_repair;
use crate::paper::Paper;
use crate::paper::parser::PaperMetadata;
use crate::paper::section::{PaperSections, Section, SectionType};

/// Maximum characters for text previews stored in vectors.
pub const PREVIEW_MAX_CHARS: usize = 500;
/// Maximum characters for section preview excerpts.
pub const SECTION_PREVIEW_CHARS: usize = 50;

/// Attempt to parse an LLM response as JSON, with extraction and repair fallbacks.
pub fn try_parse_llm_json<T: serde::de::DeserializeOwned>(raw: &str) -> crate::error::Result<T> {
    let value = json_repair::parse_llm_json(raw)
        .ok_or_else(|| PaperedError::JsonRepair("All JSON parsing attempts failed".into()))?;
    serde_json::from_value(value).map_err(|e| PaperedError::JsonRepair(e.to_string()))
}

/// Extract a JSON array of strings at `key`, filtering and deduplicating values.
/// Returns `None` when the key is absent or not an array.
fn json_string_array(parsed: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let values = parsed.get(key).and_then(|v| v.as_array())?;
    Some(crate::util::dedup_strings(
        values
            .iter()
            .filter_map(|v| v.as_str().and_then(crate::util::filter_non_empty_string))
            .collect(),
        false,
    ))
}

/// Extract a bio-entity string array at `key`: trims values, drops empty and
/// non-string items, and deduplicates case-insensitively (first spelling
/// wins). Returns an empty vec when the key is missing or not an array, so a
/// malformed entity field can never fail extraction.
fn json_entity_array(parsed: &serde_json::Value, key: &str) -> Vec<String> {
    let Some(values) = parsed.get(key).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    crate::util::dedup_strings(
        values
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        true,
    )
}

/// Parse the four structured bio-entity arrays (species, genes, techniques,
/// pathways) from an LLM extraction response.
pub fn parse_bio_entities(parsed: &serde_json::Value) -> crate::paper::BioEntities {
    crate::paper::BioEntities {
        species: json_entity_array(parsed, "species"),
        genes: json_entity_array(parsed, "genes"),
        techniques: json_entity_array(parsed, "techniques"),
        pathways: json_entity_array(parsed, "pathways"),
    }
}

pub fn parse_llm_figures(parsed: &serde_json::Value) -> Vec<crate::paper::parser::LlmFigure> {
    let Some(figs) = parsed.get("figures").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    figs.iter()
        .filter_map(|fig| {
            let label = fig
                .get("label")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let caption = fig
                .get("caption")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            // Both label and caption required; missing either = malformed entry.
            let label = label?;
            let caption = caption?;
            Some(crate::paper::parser::LlmFigure { label, caption })
        })
        .collect()
}

pub fn parse_llm_metadata(parsed: &serde_json::Value, _paper: &Paper) -> PaperMetadata {
    let llm_title = parsed
        .get("title")
        .and_then(|v| v.as_str())
        .and_then(crate::util::filter_non_empty_string);

    let llm_paper_type = parsed
        .get("paper_type")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let mut meta = PaperMetadata {
        title: llm_title,
        paper_type: llm_paper_type,
        entities: parse_bio_entities(parsed),
        ..Default::default()
    };

    if let Some(authors) = json_string_array(parsed, "authors") {
        meta.authors = authors;
    }

    if let Some(affiliations) = json_string_array(parsed, "affiliations") {
        meta.affiliations = affiliations;
    }

    if let Some(emails) = json_string_array(parsed, "emails") {
        meta.emails = emails;
    }

    meta.corresponding_author = parse_string_or_array(parsed.get("corresponding_author"));

    meta.published_date = parsed
        .get("published_date")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            // Fall back to a valid year value if the LLM only returned `year`.
            parsed.get("year").and_then(|v| {
                v.as_u64()
                    .filter(|y| (1900..=2100).contains(y))
                    .map(|y| y.to_string())
            })
        });

    meta.doi = parsed
        .get("doi")
        .and_then(|v| v.as_str())
        .and_then(crate::util::filter_non_empty_string);

    meta.venue = parsed
        .get("venue")
        .and_then(|v| v.as_str())
        .and_then(crate::util::filter_non_empty_string);

    if let Some(keywords) = json_string_array(parsed, "keywords") {
        meta.keywords = keywords;
    }

    if let Some(urls) = json_string_array(parsed, "urls") {
        meta.urls = urls;
    }

    meta.data_availability = parsed
        .get("data_availability")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string());

    if let Some(extra_val) = parsed.get("extra")
        && !extra_val.is_null()
        && extra_val.is_object()
        && let Ok(extra_str) = serde_json::to_string(extra_val)
        && !extra_str.is_empty()
        && extra_str != "{}"
    {
        meta.extra = Some(extra_str);
    }

    meta
}

pub fn build_metadata_section(meta: &PaperMetadata, paper: &Paper) -> Section {
    let meta_content = format!(
        "Title: {}\nAuthors: {}\nDate: {}\nVenue: {}\nKeywords: {}\nAffiliations: {}\nEmails: {}\nExtra: {}",
        meta.title.as_deref().unwrap_or(&paper.title),
        meta.authors
            .iter()
            .map(std::string::String::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        meta.published_date.as_deref().unwrap_or(""),
        meta.venue.as_deref().unwrap_or(""),
        meta.keywords
            .iter()
            .map(std::string::String::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        meta.affiliations.join(", "),
        meta.emails.join(", "),
        meta.extra.as_deref().unwrap_or(""),
    );
    let meta_hash = crate::util::sha256_hex(meta_content.as_bytes());
    Section {
        section_type: SectionType::Metadata,
        content: meta_content,
        content_hash: meta_hash,
    }
}

pub fn build_sections_from_json(parsed: &serde_json::Value, meta: &PaperMetadata) -> PaperSections {
    let mut sections = PaperSections::default();

    let section_mappings = vec![
        ("abstract", SectionType::Abstract),
        ("problem_statement", SectionType::ProblemStatement),
        ("core_contribution", SectionType::CoreContribution),
        ("key_insight", SectionType::KeyInsight),
        ("methodology", SectionType::Methodology),
        ("experimental_design", SectionType::ExperimentalDesign),
        ("key_findings", SectionType::KeyFindings),
        ("related_work", SectionType::RelatedWork),
        ("application", SectionType::Application),
        ("limitations", SectionType::Limitations),
    ];

    for (json_key, section_type) in section_mappings {
        if let Some(content) = parsed.get(json_key).and_then(|v| v.as_str())
            && !content.trim().is_empty()
        {
            let hash = crate::util::sha256_hex(content.as_bytes());
            sections.sections.push(Section {
                section_type,
                content: content.trim().to_string(),
                content_hash: hash,
            });
        }
    }

    tracing::info!(
        "LLM extraction: title={:?}, authors={}, paper_type={:?}, sections={}, keywords={}, urls={}",
        meta.title
            .as_deref()
            .map(|s| crate::util::truncate_chars(s, SECTION_PREVIEW_CHARS).into_owned()),
        meta.authors.len(),
        meta.paper_type,
        sections.sections.len(),
        meta.keywords.len(),
        meta.urls.len(),
    );

    sections
}

/// Overwrite `target` with a clone of `value` when `value` is not blank.
fn overlay_nonempty_str(target: &mut String, value: &str) {
    if !value.trim().is_empty() {
        *target = value.to_string();
    }
}

/// Overwrite `target` with `src` when `src` holds a non-blank string.
fn overlay_nonempty_opt(target: &mut Option<String>, src: &Option<String>) {
    if let Some(value) = src
        && !value.trim().is_empty()
    {
        *target = Some(value.clone());
    }
}

/// Overwrite `target` with a clone of `src` when `src` is non-empty.
fn overlay_nonempty_vec(target: &mut Vec<String>, src: &[String]) {
    if !src.is_empty() {
        *target = src.to_vec();
    }
}

/// Overlay `meta` fields onto `paper`, only when the meta field is non-empty.
pub fn apply_metadata(paper: &mut Paper, meta: &PaperMetadata) {
    if let Some(ref title) = meta.title {
        overlay_nonempty_str(&mut paper.title, title);
    }
    overlay_nonempty_vec(&mut paper.authors, &meta.authors);
    overlay_nonempty_vec(&mut paper.affiliations, &meta.affiliations);
    overlay_nonempty_vec(&mut paper.emails, &meta.emails);
    overlay_nonempty_vec(&mut paper.keywords, &meta.keywords);
    overlay_nonempty_vec(&mut paper.urls, &meta.urls);
    overlay_nonempty_opt(&mut paper.extra, &meta.extra);
    overlay_nonempty_opt(&mut paper.abstract_text, &meta.abstract_text);
    overlay_nonempty_opt(&mut paper.doi, &meta.doi);
    overlay_nonempty_opt(&mut paper.published_date, &meta.published_date);
    overlay_nonempty_opt(&mut paper.venue, &meta.venue);
    overlay_nonempty_opt(&mut paper.paper_type, &meta.paper_type);
    if !meta.entities.is_empty() {
        paper.entities = meta.entities.clone();
    }
}

/// Parse a JSON value that may be either a JSON array of strings or a single
/// comma/semicolon-separated string into a `Vec<String>`.
pub fn parse_string_or_array(val: Option<&serde_json::Value>) -> Vec<String> {
    let Some(val) = val else { return Vec::new() };
    match val {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Vec::new();
            }
            // Try comma first, then semicolon
            let parts: Vec<&str> = if trimmed.contains(',') {
                trimmed.split(',').collect()
            } else if trimmed.contains(';') {
                trimmed.split(';').collect()
            } else {
                return vec![trimmed.to_string()];
            };
            parts
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
        _ => Vec::new(),
    }
}

const EXTRACTION_PROMPT_TEMPLATE: &str = include_str!("extraction_prompt.txt");

pub fn build_extraction_prompt(paper: &Paper, truncated_text: &str) -> String {
    let year_str = paper.year_string();
    EXTRACTION_PROMPT_TEMPLATE
        .replace("{title}", &paper.title)
        .replace("{authors}", &paper.authors_string())
        .replace("{year}", &year_str)
        .replace("{truncated_text}", truncated_text)
}

/// Build a merge prompt for LLM-based consolidation of per-window section
/// fragments into coherent, non-redundant final sections.
pub fn build_merge_prompt(paper: &Paper, section_lists: &[Vec<Section>]) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!(
        "You are merging {n} independent window extractions of the paper \"{title}\" by {authors}.\n\
         Each window covers a different portion of the paper; its extraction may overlap with others.\n\
         Merge the fragments for each section type into a SINGLE coherent, non-redundant summary.\n\
         Eliminate duplicate information. Preserve all unique facts, findings, and arguments.\n\
         Each merged section must be 100–500 words.\n\n\
         Return a single JSON object whose keys are section types and values are the merged text.\n\
         Example: {{\"abstract\":\"...\",\"core_contribution\":\"...\"}}\n\n",
        n = section_lists.len(),
        title = paper.title,
        authors = paper.authors_string(),
    ));

    use indexmap::IndexMap;
    let mut fragments_by_type: IndexMap<SectionType, Vec<&str>> = IndexMap::new();
    for list in section_lists {
        for s in list {
            let trimmed = s.content.trim();
            if !trimmed.is_empty() {
                fragments_by_type
                    .entry(s.section_type)
                    .or_default()
                    .push(trimmed);
            }
        }
    }

    for (st, fragments) in &fragments_by_type {
        let snake: &'static str = st.into();
        if fragments.len() <= 1 {
            prompt.push_str(&format!(
                "--- {snake} (single fragment, polish only) ---\n{frag}\n\n",
                snake = snake,
                frag = fragments.first().unwrap_or(&""),
            ));
        } else {
            prompt.push_str(&format!(
                "--- {snake} ({n} fragments to merge) ---\n",
                snake = snake,
                n = fragments.len(),
            ));
            for (j, frag) in fragments.iter().enumerate() {
                prompt.push_str(&format!("[Window {j}]:\n{frag}\n\n"));
            }
        }
    }

    prompt.push_str("Respond with JSON only:");
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bio_entities_parse_normal_arrays() {
        let parsed = serde_json::json!({
            "species": ["Oryza sativa", " Mus musculus "],
            "genes": ["OsALS", "TP53"],
            "techniques": ["RNA-seq", "CRISPR"],
            "pathways": ["MAPK signaling"]
        });
        let e = parse_bio_entities(&parsed);
        assert_eq!(e.species, vec!["Oryza sativa", "Mus musculus"]);
        assert_eq!(e.genes, vec!["OsALS", "TP53"]);
        assert_eq!(e.techniques, vec!["RNA-seq", "CRISPR"]);
        assert_eq!(e.pathways, vec!["MAPK signaling"]);
    }

    #[test]
    fn bio_entities_missing_fields_yield_empty() {
        let parsed = serde_json::json!({"title": "Some paper"});
        let e = parse_bio_entities(&parsed);
        assert!(e.is_empty());
    }

    #[test]
    fn bio_entities_non_array_values_yield_empty() {
        let parsed = serde_json::json!({
            "species": "Oryza sativa",
            "genes": 42,
            "techniques": null
        });
        let e = parse_bio_entities(&parsed);
        assert!(e.is_empty());
    }

    #[test]
    fn bio_entities_skip_non_string_and_null_items() {
        let parsed = serde_json::json!({
            "genes": ["OsALS", 123, null, true, "", "  ", "TP53"],
            "species": ["Rice", {"bad": "object"}]
        });
        let e = parse_bio_entities(&parsed);
        assert_eq!(e.genes, vec!["OsALS", "TP53"]);
        assert_eq!(e.species, vec!["Rice"]);
    }

    #[test]
    fn bio_entities_dedup_case_insensitive_keeps_first_spelling() {
        let parsed = serde_json::json!({
            "genes": ["OsALS", "osals", "OSALS"],
            "species": ["Oryza sativa", "Oryza Sativa"]
        });
        let e = parse_bio_entities(&parsed);
        assert_eq!(e.genes, vec!["OsALS"]);
        assert_eq!(e.species, vec!["Oryza sativa"]);
    }

    #[test]
    fn parse_llm_metadata_carries_entities() {
        let parsed = serde_json::json!({
            "paper_type": "research_article",
            "genes": ["OsALS"]
        });
        let paper = Paper::new("t");
        let meta = parse_llm_metadata(&parsed, &paper);
        assert_eq!(meta.paper_type.as_deref(), Some("research_article"));
        assert_eq!(meta.entities.genes, vec!["OsALS"]);
    }
}
