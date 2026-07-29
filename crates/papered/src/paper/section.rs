use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

/// Default semantic section types for academic papers.
/// Each section represents a semantic dimension of the paper.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    EnumString,
    Display,
    IntoStaticStr,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[strum(serialize_all = "snake_case")]
pub enum SectionType {
    /// Title, authors, year, venue, keywords, DOI
    Metadata,
    /// Paper abstract / executive summary
    Abstract,
    /// Core contribution and innovation points
    CoreContribution,
    /// Research methodology and technical approach
    Methodology,
    /// Experimental design, datasets, evaluation metrics
    ExperimentalDesign,
    /// Key findings and main results
    KeyFindings,
    /// Related work comparison and positioning
    RelatedWork,
    /// Application scenarios and practical uses
    Application,
    /// Research problem, motivation, and background challenges
    ProblemStatement,
    /// Core insight and key conceptual breakthrough
    KeyInsight,
    /// Limitations, weaknesses, and critical assessment
    Limitations,
}

impl SectionType {
    /// Look up a variant by its canonical label.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

/// A single semantic section extracted from a paper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub section_type: SectionType,
    pub content: String,
    pub content_hash: String,
}

/// All semantic sections for a paper.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaperSections {
    pub sections: Vec<Section>,
    /// Optional hash of the LLM input that produced these sections, used for cache
    /// lookups to skip redundant LLM calls.
    #[serde(default)]
    pub input_hash: Option<String>,
}

/// Slim serializable view of a [`Section`] for API responses (the internal
/// `content_hash` cache key is intentionally not exposed).
#[derive(Debug, Clone, Serialize)]
pub struct SectionView {
    pub section_type: String,
    pub content: String,
}

impl From<&Section> for SectionView {
    fn from(s: &Section) -> Self {
        Self {
            section_type: s.section_type.to_string(),
            content: s.content.clone(),
        }
    }
}

impl PaperSections {
    pub fn new(sections: Vec<Section>) -> Self {
        Self {
            sections,
            input_hash: None,
        }
    }

    /// Slim views of all sections, for API responses.
    #[must_use]
    pub fn to_views(&self) -> Vec<SectionView> {
        self.sections.iter().map(SectionView::from).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn as_str_display_from_str_round_trip() {
        for value in [
            SectionType::Metadata,
            SectionType::Abstract,
            SectionType::CoreContribution,
            SectionType::Methodology,
            SectionType::ExperimentalDesign,
            SectionType::KeyFindings,
            SectionType::RelatedWork,
            SectionType::Application,
            SectionType::ProblemStatement,
            SectionType::KeyInsight,
            SectionType::Limitations,
        ] {
            let label: &str = value.into();
            assert_eq!(SectionType::from_str(label).unwrap(), value);
            assert_eq!(value.to_string(), label);
        }
    }
}
