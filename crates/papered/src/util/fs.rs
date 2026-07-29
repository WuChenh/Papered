use std::path::Path;

fn dir_size_bytes(path: &Path) -> crate::Result<u64> {
    fn walk(path: &Path, visited: &mut std::collections::HashSet<u64>) -> crate::Result<u64> {
        let mut total = 0u64;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let key = meta.dev() ^ meta.ino();
                    if !visited.insert(key) {
                        continue;
                    }
                }
                total += walk(&entry.path(), visited)?;
            }
        }
        Ok(total)
    }

    if !path.exists() {
        return Ok(0);
    }
    let mut visited = std::collections::HashSet::new();
    walk(path, &mut visited)
}

pub fn dir_size(path: impl AsRef<Path>) -> crate::Result<u64> {
    dir_size_bytes(path.as_ref())
}

pub fn dir_size_mb(path: &Path) -> u64 {
    dir_size_bytes(path).unwrap_or(0) / (1024 * 1024)
}

pub fn human_readable_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let exp = ((bytes as f64).log2() / 1024f64.log2()).min(UNITS.len() as f64 - 1.0) as usize;
    let value = bytes as f64 / 1024f64.powi(exp as i32);
    format!("{:.2} {}", value, UNITS[exp])
}

/// Total size of a file or directory tree in bytes (0 if unreadable).
pub fn path_size(path: &Path) -> u64 {
    if path.is_dir() {
        dir_size(path).unwrap_or(0)
    } else {
        std::fs::metadata(path).map_or(0, |m| m.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_readable_size_formats_zero() {
        assert_eq!(human_readable_size(0), "0 B");
    }

    #[test]
    fn human_readable_size_formats_kb() {
        assert_eq!(human_readable_size(1024), "1.00 KB");
    }

    #[test]
    fn dir_size_counts_files() {
        let temp = tempfile::tempdir().unwrap();
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();
        assert_eq!(dir_size(temp.path()).unwrap(), 11);
    }

    #[test]
    fn dir_size_mb_counts_files() {
        let temp = tempfile::tempdir().unwrap();
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();
        assert_eq!(dir_size_mb(temp.path()), 0);
    }
}
