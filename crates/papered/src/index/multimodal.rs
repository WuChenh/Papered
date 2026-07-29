//! Multimodal processing for figures and tables extracted by MinerU.
//!
//! Supports generating natural language descriptions of figures/images
//! and embedding them alongside text for multimodal retrieval.

use serde::{Deserialize, Serialize};

/// Extracted figure metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FigureInfo {
    pub id: String,
    pub paper_id: String,
    pub caption: Option<String>,
    pub description: Option<String>,
    pub image_path: Option<String>,
    pub page_number: Option<u32>,
    pub bbox: Option<BoundingBox>,
    /// The figure's label from the paper text (e.g., "1", "S1", "3a").
    /// `None` for figures from the MinerU fallback pipeline (no label metadata).
    pub figure_label: Option<String>,
}

/// Bounding box for figure/table location in PDF.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Parse figures from MinerU markdown output.
/// Looks for `![caption](path)` syntax.
pub fn parse_figures_from_markdown(paper_id: &str, markdown: &str) -> Vec<FigureInfo> {
    let mut figures = Vec::new();
    let re = &*crate::util::MARKDOWN_IMAGE_RE;
    for (i, cap) in re.captures_iter(markdown).enumerate() {
        let caption = cap.get(1).map(|m| m.as_str().to_string());
        let path = cap.get(2).map(|m| m.as_str().to_string());

        figures.push(FigureInfo {
            id: format!("{}_fig{}", paper_id, i + 1),
            paper_id: paper_id.to_string(),
            caption,
            description: None,
            image_path: path,
            page_number: None,
            bbox: None,
            figure_label: None,
        });
    }

    figures
}
