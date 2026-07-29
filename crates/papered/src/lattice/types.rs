//! Type definitions for the Lattice Local API.

use serde::{Deserialize, Serialize};

/// Response from `GET /api/v1/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeStatus {
    pub ok: bool,
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    #[serde(rename = "appVersion")]
    pub app_version: String,
    pub capabilities: Vec<String>,
}

/// A paper returned by `GET /api/v1/search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeSearchPaper {
    pub id: String,
    pub title: String,
    #[serde(rename = "authorsDisplay")]
    pub authors_display: String,
    pub subtitle: String,
    pub year: Option<u32>,
    pub citekey: String,
    #[serde(rename = "paperType")]
    pub paper_type: String,
}

impl LatticeSearchPaper {
    pub fn year_string(&self) -> String {
        self.year
            .map_or_else(|| "N/A".to_string(), |y| y.to_string())
    }
}

/// Response from `GET /api/v1/search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeSearchResponse {
    pub papers: Vec<LatticeSearchPaper>,
}

/// A collection returned by `GET /api/v1/collections`.
///
/// The endpoint responds with a bare JSON array of these objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeCollection {
    pub id: String,
    pub name: String,
    pub path: String,
    pub depth: u32,
}

/// Full paper detail from `GET /api/v1/papers/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticePaperDetail {
    pub id: String,
    pub citekey: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<u32>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub isbn: Option<String>,
    #[serde(rename = "paperType")]
    pub paper_type: String,
    #[serde(rename = "cslItem")]
    pub csl_item: Option<serde_json::Value>,
    /// Filesystem path to the attached PDF, resolved by Lattice from its
    /// security-scoped bookmark. Only present when the detail was requested
    /// with `?include=pdfPath`; `None` if the paper has no PDF or the bookmark
    /// cannot be resolved.
    #[serde(default, rename = "pdfPath")]
    pub pdf_path: Option<String>,
    /// Stored abstract text. Only present with `?include=abstract`; `None` if
    /// the paper has no abstract.
    #[serde(default, rename = "abstract")]
    pub abstract_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim payload from Lattice 2.4.2 (apiVersion 1): the collections
    // endpoint returns a bare array, not a {"collections": [...]} wrapper.
    #[test]
    fn collections_decode_from_bare_array() {
        let body = r#"[
            {"name":"AI4S","depth":0,"path":"AI4S","id":"17210932-3ED3-4D5D-B022-7841A3FBE9D3"},
            {"name":"Evo","depth":0,"path":"Evo","id":"F11BDAD4-082D-4186-A2FF-5FE9226604DA"}
        ]"#;
        let collections: Vec<LatticeCollection> =
            serde_json::from_str(body).expect("bare array decodes");
        assert_eq!(collections.len(), 2);
        assert_eq!(collections[0].name, "AI4S");
        assert_eq!(collections[1].id, "F11BDAD4-082D-4186-A2FF-5FE9226604DA");
    }
}
