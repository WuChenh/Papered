use std::path::Path;

use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

use crate::config::PdfExtractionConfig;
use crate::error::Result;
use crate::paper::mineru::MinerUClient;
use crate::paper::parser::{
    ExtractedText, extract_docx, extract_image_as_text, extract_latex, extract_markdown,
    extract_pdf_text, extract_plain_text,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DocumentSource {
    Pdf,
    Markdown,
    PlainText,
    Image,
    Latex,
    OfficeDocument,
}

impl std::str::FromStr for DocumentSource {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pdf" => Ok(Self::Pdf),
            "markdown" | "md" => Ok(Self::Markdown),
            "plaintext" | "text" | "txt" => Ok(Self::PlainText),
            "image" | "img" => Ok(Self::Image),
            "latex" | "tex" => Ok(Self::Latex),
            "docx" => Ok(Self::OfficeDocument),
            _ => Err(format!(
                "unknown document type: {s} (expected pdf, markdown, text, image, tex, or docx)"
            )),
        }
    }
}

impl DocumentSource {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|e| e.to_str())?
            .to_lowercase()
            .as_str()
        {
            "pdf" => Some(Self::Pdf),
            "md" | "markdown" => Some(Self::Markdown),
            "txt" | "text" => Some(Self::PlainText),
            "tex" | "latex" => Some(Self::Latex),
            "docx" => Some(Self::OfficeDocument),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" => Some(Self::Image),
            _ => None,
        }
    }
}

pub async fn extract_document_text(
    path: &Path,
    source: DocumentSource,
    mineru: Option<&MinerUClient>,
    paper_data_dir: Option<&Path>,
    pdf_config: &PdfExtractionConfig,
) -> Result<ExtractedText> {
    match source {
        DocumentSource::Pdf => extract_pdf_text(path, mineru, paper_data_dir, pdf_config).await,
        DocumentSource::Markdown => extract_markdown(path).await,
        DocumentSource::PlainText => extract_plain_text(path).await,
        DocumentSource::Image => extract_image_as_text(path).await,
        DocumentSource::Latex => extract_latex(path).await,
        DocumentSource::OfficeDocument => extract_docx(path).await,
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumString, Display, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PaperSource {
    Manual,
    Zotero,
    Lattice,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_source_from_str_accepts_aliases() {
        assert_eq!("md".parse(), Ok(DocumentSource::Markdown));
        assert_eq!("TEXT".parse(), Ok(DocumentSource::PlainText));
        assert_eq!("tex".parse(), Ok(DocumentSource::Latex));
        assert!("exe".parse::<DocumentSource>().is_err());
    }

    #[test]
    fn as_str_display_from_str_round_trip() {
        for value in [
            PaperSource::Manual,
            PaperSource::Zotero,
            PaperSource::Lattice,
        ] {
            let label: &str = value.into();
            assert_eq!(label.parse::<PaperSource>().unwrap(), value);
            assert_eq!(value.to_string(), label);
        }
    }
}
