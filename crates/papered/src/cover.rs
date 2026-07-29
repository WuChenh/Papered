use image::DynamicImage;
use image::RgbaImage;
use image::imageops::FilterType;
use pdf_oxide::rendering::{RenderOptions, render_page};
use std::path::Path;

use crate::error::Result;

const MAX_LONG_SIDE: u32 = 2000;
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
    let cover_img = resize(&dynamic, MAX_LONG_SIDE);
    save_jpeg(&cover_img, &cover_path)?;

    // Generate thumbnail at max 600px long side
    let thumb_img = resize(&dynamic, THUMB_LONG_SIDE);
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

fn resize(img: &DynamicImage, max_long: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w <= max_long && h <= max_long {
        return img.clone();
    }
    let (new_w, new_h) = if w > h {
        (max_long, (h * max_long / w).max(1))
    } else {
        ((w * max_long / h).max(1), max_long)
    };
    img.resize_exact(new_w, new_h, FilterType::Lanczos3)
}

fn save_jpeg(img: &DynamicImage, path: &Path) -> Result<()> {
    let rgb = img.to_rgb8();
    rgb.save(path).map_err(|e| {
        crate::PaperedError::io_other(format!("Failed to save JPEG {}: {e}", path.display()))
    })
}

pub fn delete_cover(data_dir: &Path, paper_id: &str) {
    for name in &[format!("{paper_id}.jpg"), format!("{paper_id}_thumb.jpg")] {
        let path = data_dir.join("covers").join(name);
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                paper_id = %paper_id,
                "Failed to delete cover {}: {e}",
                path.display()
            );
        }
    }
}
