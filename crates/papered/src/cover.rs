use image::DynamicImage;
use image::RgbaImage;
use pdf_oxide::rendering::{RenderOptions, render_page};
use std::path::Path;

use crate::error::Result;
use crate::util::image::{MAX_IMAGE_LONG_SIDE, resize_to_longest_side};

/// Thumbnail long side in pixels. 600 px keeps covers sharp on Retina (@2x)
/// displays for card widths up to ~300 pt.
const THUMB_LONG_SIDE: u32 = 600;

pub fn generate_cover(pdf_path: &Path, paper_id: &str, data_dir: &Path) -> Result<Option<String>> {
    let covers_dir = data_dir.join("covers");
    std::fs::create_dir_all(&covers_dir)?;

    let cover_rel = format!("covers/{paper_id}.jpg");
    let thumb_rel = format!("covers/{paper_id}_thumb.jpg");
    let cover_path = data_dir.join(&cover_rel);
    let thumb_path = data_dir.join(&thumb_rel);

    if cover_path.exists() && thumb_path.exists() {
        return Ok(Some(cover_rel));
    }

    let doc = pdf_oxide::PdfDocument::open(pdf_path).map_err(|e| {
        crate::PaperedError::pdf_parse_with_source(
            format!("Failed to open PDF for cover generation: {e}"),
            e,
        )
    })?;

    let page_count = doc.page_count().unwrap_or(0);
    if page_count == 0 {
        return Ok(None);
    }

    let opts = RenderOptions::with_dpi(150).as_raw();
    let rendered = render_page(&doc, 0, &opts).map_err(|e| {
        crate::PaperedError::pdf_parse_with_source(format!("Failed to render PDF cover: {e}"), e)
    })?;

    let img =
        RgbaImage::from_raw(rendered.width, rendered.height, rendered.data).ok_or_else(|| {
            crate::PaperedError::io_other("Failed to construct image from rendered PDF page")
        })?;
    let dynamic = DynamicImage::ImageRgba8(img);

    // Resize cover to max 2000px long side
    let cover_img = resize_to_longest_side(&dynamic, MAX_IMAGE_LONG_SIDE);
    save_jpeg(&cover_img, &cover_path)?;

    // Generate thumbnail at max 600px long side
    let thumb_img = resize_to_longest_side(&dynamic, THUMB_LONG_SIDE);
    save_jpeg(&thumb_img, &thumb_path)?;

    tracing::info!(
        paper_id = %paper_id,
        "Generated cover {} ({}x{}) and thumbnail ({}x{})",
        cover_path.display(),
        cover_img.width(),
        cover_img.height(),
        thumb_img.width(),
        thumb_img.height(),
    );

    Ok(Some(cover_rel))
}

fn save_jpeg(img: &DynamicImage, path: &Path) -> Result<()> {
    let rgb = img.to_rgb8();
    rgb.save(path).map_err(|e| {
        crate::PaperedError::io_other(format!("Failed to save JPEG {}: {e}", path.display()))
    })
}
