use std::path::{Path, PathBuf};

use futures_util::future;
use papered::AppConfig;
use papered::store::vector::VectorStore;
use papered::util::paths::is_safe_paper_id;

/// A PNG found in a paper's image directory, with its quality verdict
/// (`None` = acceptable, or could not be evaluated).
pub(crate) struct ScannedImage {
    pub(crate) path: PathBuf,
    pub(crate) filename: String,
    pub(crate) reason: Option<String>,
}

/// Scan `data_dir/papers/<paper_id>/images` in a single blocking task: list
/// the directory, stat each PNG, and read its header for dimensions.
pub(crate) async fn scan_paper_images(
    data_dir: &Path,
    paper_id: &str,
    config: &AppConfig,
) -> Vec<ScannedImage> {
    let img_dir = data_dir.join("papers").join(paper_id).join("images");
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        let mut scanned = Vec::new();
        let Ok(entries) = std::fs::read_dir(&img_dir) else {
            return scanned;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("png") {
                continue;
            }
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let reason = std::fs::metadata(&path)
                .ok()
                .and_then(|m| evaluate_image(m.len(), png_dimensions(&path), &config));
            scanned.push(ScannedImage {
                path,
                filename,
                reason,
            });
        }
        scanned
    })
    .await
    .unwrap_or_default()
}

pub(crate) fn evaluate_image(
    file_size: u64,
    dimensions: Option<(u32, u32)>,
    config: &AppConfig,
) -> Option<String> {
    let pdf = &config.pdf_extraction;
    if file_size < pdf.min_image_file_size_bytes {
        Some(format!(
            "too_small_file ({}B < {}B)",
            file_size, pdf.min_image_file_size_bytes
        ))
    } else if file_size > pdf.max_image_file_size_bytes {
        Some(format!(
            "too_large_file ({}B > {}B)",
            file_size, pdf.max_image_file_size_bytes
        ))
    } else if let Some((w, h)) = dimensions {
        let short = w.min(h);
        let long = w.max(h);
        if short <= pdf.min_image_short_side {
            Some(format!(
                "short_side_too_small ({}px <= {}px)",
                short, pdf.min_image_short_side
            ))
        } else if long > pdf.max_image_long_side {
            Some(format!(
                "long_side_too_large ({}px > {}px)",
                long, pdf.max_image_long_side
            ))
        } else {
            None
        }
    } else {
        Some("unreadable_png".to_string())
    }
}

pub(crate) async fn prune_stale_figures_for_paper(
    store: &std::sync::Arc<dyn VectorStore>,
    data_dir: &Path,
    paper_id: &str,
) -> usize {
    if !is_safe_paper_id(paper_id) {
        tracing::warn!(paper_id = %paper_id, "Skipping stale figure prune for unsafe id");
        return 0;
    }
    let figures = match store.get_figures(paper_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("Failed to get figures for {}: {}", paper_id, e);
            return 0;
        }
    };
    let original_count = figures.len();
    let exist_checks: Vec<_> = figures
        .iter()
        .map(|f| {
            let data_dir = data_dir.to_path_buf();
            let paper_id = paper_id.to_string();
            async move {
                let keep = if let Some(ref p) = f.image_path {
                    let path = data_dir.join("papers").join(&paper_id).join(p);
                    tokio::fs::try_exists(&path).await.unwrap_or(false)
                } else {
                    false
                };
                (keep, f.clone())
            }
        })
        .collect();
    let mut valid = Vec::new();
    for (keep, f) in future::join_all(exist_checks).await {
        if keep {
            valid.push(f);
        }
    }
    let removed_count = original_count - valid.len();
    if let Err(e) = store.delete_figures(paper_id).await {
        tracing::warn!(
            "Failed to delete stale figure records for {}: {}",
            paper_id,
            e
        );
        return 0;
    }
    if !valid.is_empty()
        && let Err(e) = store.insert_figures(paper_id, &valid).await
    {
        tracing::warn!("Failed to re-insert valid figures for {}: {}", paper_id, e);
    }
    removed_count
}

/// Read width/height from a PNG file header (IHDR chunk).
fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 24];
    file.read_exact(&mut buf).ok()?;
    let w = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
    let h = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
    Some((w, h))
}
