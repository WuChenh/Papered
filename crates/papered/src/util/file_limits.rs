use std::path::Path;

use crate::paper::mineru::MinerUMode;

/// The backend that will extract an uploaded PDF, which determines its size limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfBackend {
    /// MinerU cloud extraction in the given mode.
    MinerU(MinerUMode),
    /// Local pdf_oxide extraction.
    Local,
}

impl PdfBackend {
    /// Effective backend for the current configuration.
    #[must_use]
    pub fn from_config(config: &crate::AppConfig) -> Self {
        if config.mineru.enabled {
            Self::MinerU(config.mineru.mode)
        } else {
            Self::Local
        }
    }

    /// Maximum accepted PDF size for this backend, in bytes.
    #[must_use]
    pub const fn size_limit_bytes(self) -> u64 {
        match self {
            Self::MinerU(MinerUMode::Lightweight) => 10 * 1024 * 1024,
            Self::MinerU(MinerUMode::Precision) => 200 * 1024 * 1024,
            Self::Local => 1024 * 1024 * 1024,
        }
    }

    /// Human-readable label used in error messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MinerU(MinerUMode::Lightweight) => "lightweight",
            Self::MinerU(MinerUMode::Precision) => "precision",
            Self::Local => "pdf_oxide",
        }
    }
}

/// Reject sizes larger than the limit imposed by the active extraction backend.
pub fn check_size_bytes(size: u64, backend: PdfBackend) -> crate::Result<()> {
    let limit = backend.size_limit_bytes();
    if size > limit {
        Err(crate::PaperedError::invalid_argument(format!(
            "File size {size} bytes exceeds {} limit of {limit} bytes",
            backend.label()
        )))
    } else {
        Ok(())
    }
}

/// Reject files larger than the limit imposed by the active extraction backend.
pub fn check_file_size(path: impl AsRef<Path>, backend: PdfBackend) -> crate::Result<()> {
    let size = std::fs::metadata(path.as_ref())?.len();
    check_size_bytes(size, backend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn size_limit_per_backend() {
        assert_eq!(
            PdfBackend::MinerU(MinerUMode::Lightweight).size_limit_bytes(),
            10 * 1024 * 1024
        );
        assert_eq!(
            PdfBackend::MinerU(MinerUMode::Precision).size_limit_bytes(),
            200 * 1024 * 1024
        );
        assert_eq!(PdfBackend::Local.size_limit_bytes(), 1024 * 1024 * 1024);
    }

    #[test]
    fn from_config_tracks_mineru_enabled() {
        let mut config = crate::AppConfig::default();
        assert_eq!(PdfBackend::from_config(&config), PdfBackend::Local);

        config.mineru.enabled = true;
        config.mineru.mode = MinerUMode::Lightweight;
        assert_eq!(
            PdfBackend::from_config(&config),
            PdfBackend::MinerU(MinerUMode::Lightweight)
        );
    }

    #[test]
    fn check_file_size_accepts_files_within_limit() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&[0u8; 1024]).unwrap();
        assert!(check_file_size(tmp.path(), PdfBackend::MinerU(MinerUMode::Lightweight)).is_ok());
    }

    #[test]
    fn check_file_size_rejects_files_over_limit() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&[0u8; 10 * 1024 * 1024 + 1]).unwrap();
        let err =
            check_file_size(tmp.path(), PdfBackend::MinerU(MinerUMode::Lightweight)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exceeds lightweight limit"), "{msg}");
        assert!(msg.contains("10485761"), "{msg}");
        assert!(msg.contains("10485760"), "{msg}");
    }
}
