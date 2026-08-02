//! Image optimization utilities for reducing storage size of extracted figures.

use crate::config::PdfExtractionConfig;
use crate::error::{PaperedError, Result};
use crate::store::vector::VectorStore;
use std::path::Path;
use std::sync::Arc;

/// Determine the MIME content type for an image file based on its extension.
pub fn image_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// Output format requested by configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizedImageFormat {
    Jpeg,
    WebP,
    Png,
    /// Automatically choose between JPEG and WebP per-image.
    /// - Images with transparency -> WebP.
    /// - Images with a small color palette (diagrams, text) -> WebP lossless.
    /// - Everything else (photos, scans) -> JPEG.
    Auto,
}

/// Parse an image format string from configuration.
pub fn parse_image_format(s: &str) -> OptimizedImageFormat {
    match s.to_lowercase().as_str() {
        "webp" => OptimizedImageFormat::WebP,
        "png" => OptimizedImageFormat::Png,
        "auto" => OptimizedImageFormat::Auto,
        _ => OptimizedImageFormat::Jpeg,
    }
}

/// Preferred file extension for an optimized image format.
pub fn format_extension(format: image::ImageFormat) -> &'static str {
    match format {
        image::ImageFormat::WebP => "webp",
        image::ImageFormat::Png => "png",
        _ => "jpg",
    }
}

impl OptimizedImageFormat {
    /// Extension to use when the actual format cannot be determined (e.g.
    /// optimization failed before classification).
    pub fn default_extension(self) -> &'static str {
        match self {
            OptimizedImageFormat::WebP => "webp",
            OptimizedImageFormat::Png => "png",
            OptimizedImageFormat::Jpeg | OptimizedImageFormat::Auto => "jpg",
        }
    }
}

/// Heuristic that decides whether an image is better stored as lossless WebP
/// or JPEG.
///
/// Currently the image crate's lossless WebP encoder is not competitive with
/// JPEG for most paper figures, including diagrams. The only clear win for
/// WebP is transparency, which JPEG cannot represent. Therefore "auto" defaults
/// to JPEG and only picks WebP when the image has an alpha channel.
fn choose_auto_format(img: &image::DynamicImage) -> image::ImageFormat {
    if img.color().has_alpha() {
        image::ImageFormat::WebP
    } else {
        image::ImageFormat::Jpeg
    }
}

/// Read width/height from a PNG file header (IHDR chunk).
/// PNG layout: 8-byte signature, then 4-byte length, 4-byte "IHDR",
/// then 4-byte width, 4-byte height (all big-endian).
pub fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 24];
    file.read_exact(&mut buf).ok()?;
    let w = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
    let h = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
    Some((w, h))
}

/// Shared long-side cap (px) for stored page images: covers and extracted
/// figures are both capped at 2000 px on the long side.
pub const MAX_IMAGE_LONG_SIDE: u32 = 2000;

/// Resize an image so its longest side is at most `max_long_side`, preserving
/// aspect ratio with Lanczos3 filtering. Returns the image unchanged when it
/// already fits. Shared by cover generation, figure rendering, and image
/// optimization.
pub fn resize_to_longest_side(
    img: &image::DynamicImage,
    max_long_side: u32,
) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w <= max_long_side && h <= max_long_side {
        return img.clone();
    }
    let (new_w, new_h) = if w > h {
        (max_long_side, (h * max_long_side / w).max(1))
    } else {
        ((w * max_long_side / h).max(1), max_long_side)
    };
    img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3)
}

/// Resize and re-encode an image to reduce storage size.
///
/// - Decodes the source image.
/// - Resizes so that the longest side is at most `max_long_side`, preserving aspect ratio.
/// - Re-encodes as JPEG/WebP/PNG with the specified quality.
/// - Returns the number of bytes written to `dst`.
///
/// Quality is interpreted per format:
/// - JPEG: 0–100 (higher = better quality, larger file).
/// - WebP: 0–100.
/// - PNG: quality is ignored; the image is saved with the fastest/compact settings.
pub fn optimize_image(
    src: &Path,
    dst: &Path,
    max_long_side: u32,
    quality: u8,
    format: OptimizedImageFormat,
) -> Result<(u64, image::ImageFormat)> {
    if max_long_side == 0 {
        return Err(PaperedError::config(
            "image_max_long_side must be greater than 0",
        ));
    }

    let mut img = image::open(src).map_err(|e| {
        PaperedError::Indexing(format!(
            "Failed to open image {} for optimization: {e}",
            src.display()
        ))
    })?;

    let actual_format = match format {
        OptimizedImageFormat::Auto => choose_auto_format(&img),
        OptimizedImageFormat::Jpeg => image::ImageFormat::Jpeg,
        OptimizedImageFormat::WebP => image::ImageFormat::WebP,
        OptimizedImageFormat::Png => image::ImageFormat::Png,
    };

    img = resize_to_longest_side(&img, max_long_side);

    // Ensure color type is compatible with the target format.
    let img = match actual_format {
        image::ImageFormat::Jpeg | image::ImageFormat::WebP => {
            // JPEG does not support alpha; WebP supports it, but converting to RGB
            // produces smaller files and avoids edge-case encoder issues.
            img.to_rgb8().into()
        }
        image::ImageFormat::Png => img,
        _ => img,
    };

    let quality = quality.clamp(1, 100);

    let mut out = std::fs::File::create(dst).map_err(|e| {
        PaperedError::io_other(format!(
            "Failed to create optimized image {}: {e}",
            dst.display()
        ))
    })?;

    match actual_format {
        image::ImageFormat::Jpeg => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            img.write_with_encoder(encoder).map_err(|e| {
                PaperedError::Indexing(format!("Failed to write JPEG image {}: {e}", dst.display()))
            })?;
        }
        image::ImageFormat::Png => {
            let encoder = image::codecs::png::PngEncoder::new(&mut out);
            img.write_with_encoder(encoder).map_err(|e| {
                PaperedError::Indexing(format!("Failed to write PNG image {}: {e}", dst.display()))
            })?;
        }
        image::ImageFormat::WebP => {
            // The image crate's WebP encoder is lossless-only, so quality is
            // ignored. For lossy WebP a separate codec would be needed.
            let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
            img.write_with_encoder(encoder).map_err(|e| {
                PaperedError::Indexing(format!("Failed to write WebP image {}: {e}", dst.display()))
            })?;
        }
        _ => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            img.write_with_encoder(encoder).map_err(|e| {
                PaperedError::Indexing(format!("Failed to write JPEG image {}: {e}", dst.display()))
            })?;
        }
    };

    let size = std::fs::metadata(dst)
        .map(|m| m.len())
        .map_err(|e| PaperedError::io_other(e.to_string()))?;
    Ok((size, actual_format))
}

/// Move an optimized image into `{dest_dir}` as `{stem}.{ext}`, falling back
/// to copying the original file when optimization failed. Returns the file
/// name and the relative path (`{rel_dir}/{file_name}`) used for markdown
/// references.
///
/// `optimize_result` is the (already invoked, possibly already awaited)
/// result of [`optimize_image`] writing to `{dest_dir}/{stem}.tmp`, which
/// keeps this helper usable from both sync and async callers. When
/// `fallback_copy_fatal` is true, a failure of the fallback copy is returned
/// as an error; otherwise it is logged and the names are returned with the
/// destination file missing.
pub fn place_optimized_image(
    optimize_result: Result<(u64, image::ImageFormat)>,
    src: &Path,
    dest_dir: &Path,
    rel_dir: &str,
    stem: &str,
    default_format: OptimizedImageFormat,
    fallback_copy_fatal: bool,
) -> Result<(String, String)> {
    let tmp_path = dest_dir.join(format!("{stem}.tmp"));
    match optimize_result {
        Ok((_, actual_format)) => {
            let ext = format_extension(actual_format);
            let fname = format!("{stem}.{ext}");
            let dest = dest_dir.join(&fname);
            if let Err(e) = std::fs::rename(&tmp_path, &dest) {
                tracing::warn!("Failed to rename optimized image: {e}");
            }
            Ok((fname.clone(), format!("{rel_dir}/{fname}")))
        }
        Err(e) => {
            let ext = default_format.default_extension();
            let fname = format!("{stem}.{ext}");
            let dest = dest_dir.join(&fname);
            tracing::warn!("Failed to optimize image {fname}: {e}; falling back to original");
            let _ = std::fs::remove_file(&tmp_path);
            match std::fs::copy(src, &dest) {
                Ok(_) => {}
                Err(copy_err) if fallback_copy_fatal => {
                    return Err(PaperedError::pdf_parse_with_source(
                        format!("Failed to copy image {fname}: {copy_err}"),
                        copy_err,
                    ));
                }
                Err(copy_err) => {
                    tracing::warn!(
                        "Failed to copy original image {} to {}: {}",
                        src.display(),
                        dest.display(),
                        copy_err
                    );
                }
            }
            Ok((fname.clone(), format!("{rel_dir}/{fname}")))
        }
    }
}

/// Statistics returned by [`optimize_existing_images`].
///
/// Fields are atomic — the struct can be shared across tasks via `Arc`
/// without a wrapping `Mutex`.
#[derive(Debug, Default)]
pub struct ImageOptimizationStats {
    papers_scanned: std::sync::atomic::AtomicUsize,
    images_processed: std::sync::atomic::AtomicUsize,
    images_skipped: std::sync::atomic::AtomicUsize,
    images_failed: std::sync::atomic::AtomicUsize,
    bytes_before: std::sync::atomic::AtomicU64,
    bytes_after: std::sync::atomic::AtomicU64,
}

impl ImageOptimizationStats {
    pub fn add_paper(&self) {
        self.papers_scanned
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn add_processed(&self, before: u64, after: u64) {
        self.images_processed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.bytes_before
            .fetch_add(before, std::sync::atomic::Ordering::Relaxed);
        self.bytes_after
            .fetch_add(after, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn add_skipped(&self) {
        self.images_skipped
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn add_failed(&self) {
        self.images_failed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    /// Collect the current values into a plain snapshot for the return type.
    pub fn snapshot(&self) -> ImageOptimizationSnapshot {
        ImageOptimizationSnapshot {
            papers_scanned: self
                .papers_scanned
                .load(std::sync::atomic::Ordering::Relaxed),
            images_processed: self
                .images_processed
                .load(std::sync::atomic::Ordering::Relaxed),
            images_skipped: self
                .images_skipped
                .load(std::sync::atomic::Ordering::Relaxed),
            images_failed: self
                .images_failed
                .load(std::sync::atomic::Ordering::Relaxed),
            bytes_before: self.bytes_before.load(std::sync::atomic::Ordering::Relaxed),
            bytes_after: self.bytes_after.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// Non-atomic snapshot of [`ImageOptimizationStats`] for public API returns.
#[derive(Debug, Clone, Default)]
pub struct ImageOptimizationSnapshot {
    pub papers_scanned: usize,
    pub images_processed: usize,
    pub images_skipped: usize,
    pub images_failed: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

/// Re-optimize all existing figure images under `data_dir/papers` according to
/// `pdf_config`. When `dry_run` is true, sizes are estimated but no files or
/// database records are changed.
///
/// Returns the accumulated statistics.
pub async fn optimize_existing_images(
    data_dir: &Path,
    store: Arc<dyn VectorStore>,
    pdf_config: &PdfExtractionConfig,
    dry_run: bool,
) -> Result<ImageOptimizationSnapshot> {
    if !pdf_config.extract_images {
        return Err(PaperedError::config(
            "Cannot optimize images when extract_images is false",
        ));
    }

    let papers_dir = data_dir.join("papers");
    if !papers_dir.exists() {
        return Ok(ImageOptimizationSnapshot::default());
    }

    let out_format = parse_image_format(&pdf_config.output_format);
    let stats = Arc::new(ImageOptimizationStats::default());
    let progress_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let mut read_dir = tokio::fs::read_dir(&papers_dir).await.map_err(|e| {
        PaperedError::io_other(format!(
            "Failed to read papers directory {}: {e}",
            papers_dir.display()
        ))
    })?;

    let mut paper_dir_entries = Vec::new();
    while let Some(entry) = read_dir.next_entry().await? {
        paper_dir_entries.push(entry);
    }

    tracing::info!(
        "Found {} paper directories to scan",
        paper_dir_entries.len()
    );

    let mut paper_tasks = tokio::task::JoinSet::new();
    for entry in paper_dir_entries {
        let paper_dir = entry.path();
        let is_dir = match entry.file_type().await {
            Ok(ft) => ft.is_dir(),
            Err(e) => {
                tracing::warn!(path = %paper_dir.display(), "Failed to read entry file type: {e}");
                continue;
            }
        };
        if !is_dir {
            continue;
        }
        let paper_id = match paper_dir.file_name().and_then(|n| n.to_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };

        let figures = match store.get_figures(&paper_id).await {
            Ok(figs) => figs,
            Err(e) => {
                tracing::warn!(paper_id = %paper_id, error = %e, "Failed to load figures");
                continue;
            }
        };
        if figures.is_empty() {
            continue;
        }

        stats.add_paper();

        let stats = stats.clone();
        let paper_dir = paper_dir.clone();
        let max_long_side = pdf_config.output_max_long_side;
        let quality = pdf_config.output_quality;
        let format = out_format;
        let store = store.clone();
        let progress_counter = progress_counter.clone();
        paper_tasks.spawn(async move {
            optimize_paper_images(
                &paper_id,
                &paper_dir,
                figures,
                max_long_side,
                quality,
                format,
                dry_run,
                store,
                stats,
                progress_counter,
            )
            .await;
        });
    }

    while let Some(res) = paper_tasks.join_next().await {
        if let Err(e) = res {
            tracing::warn!("Paper optimization task panicked or failed: {e}");
        }
    }

    let final_stats = Arc::try_unwrap(stats)
        .map(|s| s.snapshot())
        .unwrap_or_else(|s| s.snapshot());
    Ok(final_stats)
}

#[allow(clippy::too_many_arguments)]
async fn optimize_paper_images(
    paper_id: &str,
    paper_dir: &Path,
    figures: Vec<crate::index::multimodal::FigureInfo>,
    max_long_side: u32,
    quality: u8,
    format: OptimizedImageFormat,
    dry_run: bool,
    store: Arc<dyn VectorStore>,
    stats: Arc<ImageOptimizationStats>,
    progress_counter: Arc<std::sync::atomic::AtomicUsize>,
) {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(2));
    let mut image_tasks = tokio::task::JoinSet::new();

    for fig in figures {
        let Some(ref rel_path) = fig.image_path else {
            stats.add_skipped();
            continue;
        };
        if rel_path.is_empty() {
            stats.add_skipped();
            continue;
        }

        let src = paper_dir.join(rel_path);
        let exists = match tokio::fs::try_exists(&src).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(paper_id = %paper_id, figure_id = %fig.id, path = %src.display(), "Failed to check image existence: {e}");
                stats.add_skipped();
                continue;
            }
        };
        if !exists {
            stats.add_skipped();
            continue;
        }

        let fig = fig.clone();
        let stats = stats.clone();
        let paper_id = paper_id.to_string();
        let rel_path = rel_path.clone();
        let store = store.clone();
        let progress_counter = progress_counter.clone();
        let permit = semaphore.clone().acquire_owned().await.ok();
        image_tasks.spawn(async move {
            let _permit = permit;
            optimize_single_existing_image(
                &paper_id,
                &fig,
                &src,
                &rel_path,
                max_long_side,
                quality,
                format,
                dry_run,
                store,
                stats,
                progress_counter,
            )
            .await;
        });
    }

    while let Some(res) = image_tasks.join_next().await {
        if let Err(e) = res {
            tracing::warn!(paper_id = %paper_id, "Image optimization task panicked or failed: {e}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn optimize_single_existing_image(
    paper_id: &str,
    fig: &crate::index::multimodal::FigureInfo,
    src: &Path,
    rel_path: &str,
    max_long_side: u32,
    quality: u8,
    format: OptimizedImageFormat,
    dry_run: bool,
    store: Arc<dyn VectorStore>,
    stats: Arc<ImageOptimizationStats>,
    progress_counter: Arc<std::sync::atomic::AtomicUsize>,
) {
    let src_meta = match tokio::fs::metadata(src).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(paper_id = %paper_id, figure_id = %fig.id, "Failed to read metadata: {e}");
            stats.add_failed();
            return;
        }
    };
    let src_size = src_meta.len();

    if dry_run {
        let tmp = match tempfile::NamedTempFile::with_suffix(".tmp") {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(paper_id = %paper_id, figure_id = %fig.id, "Failed to create temp file: {e}");
                stats.add_failed();
                return;
            }
        };
        let src = src.to_path_buf();
        let dst = tmp.path().to_path_buf();
        let estimate = tokio::task::spawn_blocking(move || {
            optimize_image(&src, &dst, max_long_side, quality, format)
        })
        .await;

        match estimate {
            Ok(Ok((estimated_size, _actual_format))) => {
                stats.add_processed(src_size, estimated_size);
                let count = progress_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if count.is_multiple_of(100) {
                    tracing::info!("processed {count} images (dry-run)...");
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(paper_id = %paper_id, figure_id = %fig.id, "Dry-run optimization failed: {e}");
                stats.add_failed();
            }
            Err(e) => {
                tracing::warn!(paper_id = %paper_id, figure_id = %fig.id, "Image optimization task failed: {e}");
                stats.add_failed();
            }
        }
        return;
    }

    let src_buf = src.to_path_buf();
    let tmp_path = src.with_extension("tmp");
    let dst_buf = tmp_path.clone();
    let optimize_result = tokio::task::spawn_blocking(move || {
        optimize_image(&src_buf, &dst_buf, max_long_side, quality, format)
    })
    .await;

    match optimize_result {
        Ok(Ok((dest_size, actual_format))) => {
            stats.add_processed(src_size, dest_size);
            let count = progress_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if count.is_multiple_of(100) {
                tracing::info!("processed {count} images...");
            }

            let ext = format_extension(actual_format);
            let dest_rel = std::path::Path::new(rel_path).with_extension(ext);
            let dest = src.with_extension(ext);
            let same_path = src == dest;
            if let Err(e) = tokio::fs::rename(&tmp_path, &dest).await {
                tracing::warn!(paper_id = %paper_id, figure_id = %fig.id, "Failed to rename optimized image: {e}");
            }

            if !same_path {
                let new_rel = dest_rel.to_string_lossy().into_owned();
                if let Err(e) = store.update_figure_image_path(&fig.id, &new_rel).await {
                    tracing::warn!(
                        paper_id = %paper_id,
                        figure_id = %fig.id,
                        "Failed to update figure image_path: {e}"
                    );
                }
                if tokio::fs::try_exists(src).await.unwrap_or(false) {
                    let _ = tokio::fs::remove_file(src).await;
                }
            }
        }
        Ok(Err(e)) => {
            tracing::warn!(paper_id = %paper_id, figure_id = %fig.id, "Optimization failed: {e}");
            let _ = tokio::fs::remove_file(&tmp_path).await;
            stats.add_failed();
        }
        Err(e) => {
            tracing::warn!(paper_id = %paper_id, figure_id = %fig.id, "Image optimization task failed: {e}");
            let _ = tokio::fs::remove_file(&tmp_path).await;
            stats.add_failed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_image_format() {
        assert_eq!(parse_image_format("jpeg"), OptimizedImageFormat::Jpeg);
        assert_eq!(parse_image_format("JPEG"), OptimizedImageFormat::Jpeg);
        assert_eq!(parse_image_format("webp"), OptimizedImageFormat::WebP);
        assert_eq!(parse_image_format("png"), OptimizedImageFormat::Png);
        assert_eq!(parse_image_format("auto"), OptimizedImageFormat::Auto);
        assert_eq!(parse_image_format("unknown"), OptimizedImageFormat::Jpeg);
    }

    #[test]
    fn test_format_extension() {
        assert_eq!(format_extension(image::ImageFormat::Jpeg), "jpg");
        assert_eq!(format_extension(image::ImageFormat::WebP), "webp");
        assert_eq!(format_extension(image::ImageFormat::Png), "png");
    }

    #[test]
    fn test_optimize_image_resize() {
        // Create a 200x100 RGB image.
        let img = image::RgbImage::from_pixel(200, 100, image::Rgb([0, 128, 255]));
        let src = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        img.save(src.path()).unwrap();

        let dst = tempfile::NamedTempFile::with_suffix(".jpg").unwrap();
        let (written, actual_format) =
            optimize_image(src.path(), dst.path(), 100, 85, OptimizedImageFormat::Jpeg)
                .expect("optimization should succeed");
        assert!(written > 0);
        assert_eq!(actual_format, image::ImageFormat::Jpeg);

        let optimized = image::open(dst.path()).unwrap();
        assert_eq!(optimized.width(), 100);
        assert_eq!(optimized.height(), 50);
    }
}
