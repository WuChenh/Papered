/// Validate that a paper identifier is safe to use as a filesystem directory name.
///
/// Rejects empty ids, parent-directory references, and path separators.
/// This is a synchronous guard; for resolving relative paths inside a paper
/// directory use [`safe_join`].
pub fn is_safe_paper_id(paper_id: &str) -> bool {
    !paper_id.is_empty()
        && !paper_id.contains("..")
        && !paper_id.contains('/')
        && !paper_id.contains('\\')
}

/// Validate a paper ID, returning `InvalidArgument` on failure.
pub fn validate_paper_id(paper_id: &str) -> crate::error::Result<()> {
    if is_safe_paper_id(paper_id) {
        Ok(())
    } else {
        Err(crate::PaperedError::invalid_argument(format!(
            "Invalid paper ID: {paper_id:?}"
        )))
    }
}

/// Validate a paper ID and return a pre-formatted error message string.
pub fn validate_paper_id_msg(paper_id: &str) -> Result<(), String> {
    validate_paper_id(paper_id).map_err(|e| format!("Invalid paper ID: {e}"))
}

pub async fn safe_join(
    base: &std::path::Path,
    paper_id: &str,
    rel: &str,
) -> Result<std::path::PathBuf, crate::PaperedError> {
    if !is_safe_paper_id(paper_id) {
        return Err(crate::PaperedError::invalid_argument(
            "Invalid paper_id: path traversal detected",
        ));
    }
    if rel.contains("..") || rel.starts_with('/') || rel.starts_with('\\') || rel.contains('\\') {
        return Err(crate::PaperedError::invalid_argument(format!(
            "Path traversal detected: {rel} is not a safe relative path"
        )));
    }
    let candidate = base.join("papers").join(paper_id).join(rel);
    let base = base.to_path_buf();
    let paper_id = paper_id.to_string();
    let rel = rel.to_string();
    tokio::task::spawn_blocking(move || {
        let paper_dir = base.join("papers").join(&paper_id);

        // Canonicalize the paper directory so containment checks are against the
        // real filesystem path (resolves symlinks).
        let canonical_base = match std::fs::canonicalize(&paper_dir) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("canonicalize paper dir failed for {paper_id}: {e}");
                // The paper directory does not exist. We can still validate purely
                // syntactically by normalizing components and checking containment
                // against the (non-canonical) base.
                let normalized = normalize_components(&candidate);
                if !normalized.starts_with(&paper_dir) {
                    return Err(crate::PaperedError::invalid_argument(format!(
                        "Path traversal detected: {rel} resolves outside paper directory"
                    )));
                }
                return Ok(normalized);
            }
        };

        // If the candidate exists, canonicalize it and verify containment.
        if let Ok(canonical_candidate) = std::fs::canonicalize(&candidate) {
            if !canonical_candidate.starts_with(&canonical_base) {
                return Err(crate::PaperedError::invalid_argument(format!(
                    "Path traversal detected: {rel} resolves outside paper directory"
                )));
            }
            return Ok(canonical_candidate);
        }

        // Candidate does not exist. Canonicalize its parent directory, re-join the
        // file name, and verify containment. This avoids the TOCTU window where a
        // path component is replaced by a symlink between normalization and use.
        let parent = candidate.parent().unwrap_or(&canonical_base);
        let file_name = candidate
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if file_name.is_empty() || file_name == "." || file_name == ".." {
            return Err(crate::PaperedError::invalid_argument(format!(
                "Invalid relative path: {rel}"
            )));
        }
        let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
            crate::PaperedError::invalid_argument(format!(
                "Path traversal detected: {rel} parent cannot be resolved: {e}"
            ))
        })?;
        if !canonical_parent.starts_with(&canonical_base) {
            return Err(crate::PaperedError::invalid_argument(format!(
                "Path traversal detected: {rel} resolves outside paper directory"
            )));
        }
        let resolved = canonical_parent.join(&file_name);
        if !resolved.starts_with(&canonical_base) {
            return Err(crate::PaperedError::invalid_argument(format!(
                "Path traversal detected: {rel} resolves outside paper directory"
            )));
        }
        Ok(resolved)
    })
    .await
    .map_err(|e| crate::PaperedError::io_other(e.to_string()))?
}

/// Normalize a file path as typed or pasted by a user.
///
/// Handles the three shapes real pastes come in:
/// - surrounding single or double quotes (copied from shell output or error
///   messages): `'/tmp/a b.pdf'` -> `/tmp/a b.pdf`
/// - backslash escapes inserted when a file is dragged into a terminal:
///   `/tmp/a\ b.pdf` -> `/tmp/a b.pdf`
/// - a leading `~/` (or bare `~`) expanded to the user's home directory
pub fn normalize_input_path(input: &str) -> std::path::PathBuf {
    let mut s = input.trim();
    // Strip one pair of surrounding matching quotes.
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            s = &s[1..s.len() - 1];
        }
    }
    expand_tilde(&unescape_shell_chars(s))
}

/// Remove the backslash escapes a terminal inserts when a file is dragged
/// into it (`\ `, `\(`, `\)` ...). Only a conservative ASCII set is
/// unescaped, so Windows paths like `C:\Users` pass through untouched.
fn unescape_shell_chars(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\\') {
        return std::borrow::Cow::Borrowed(s);
    }
    const ESCAPABLE: &[u8] = b" \\'\"()[]{};&$!#*?<>|`";
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && ESCAPABLE.contains(&bytes[i + 1]) {
            out.push(bytes[i + 1]);
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // The escapable set is ASCII, so removing backslashes keeps valid UTF-8.
    std::borrow::Cow::Owned(String::from_utf8(out).expect("unescape keeps UTF-8"))
}

/// Expand a leading `~/` (or bare `~`) to the user's home directory so the
/// add-paper form accepts the paths users naturally paste. Anything else is
/// returned unchanged.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    std::path::PathBuf::from(path)
}

/// Normalize a path by collapsing `.` and `..` components without touching the filesystem.
fn normalize_components(path: &std::path::Path) -> std::path::PathBuf {
    path.iter()
        .fold(std::path::PathBuf::new(), |mut acc, component| {
            if component == ".." {
                acc.pop();
            } else if component != "." && !component.is_empty() {
                acc.push(component);
            }
            acc
        })
}

async fn resolve_image_path(
    data_dir: &std::path::Path,
    paper_id: &str,
    image_path: &mut Option<String>,
) {
    if let Some(ref rel) = *image_path {
        match safe_join(data_dir, paper_id, rel).await {
            Ok(abs) => *image_path = Some(abs.to_string_lossy().into_owned()),
            Err(e) => {
                tracing::warn!("Skipping unsafe figure path for {}: {}", paper_id, e);
                *image_path = None;
            }
        }
    }
}

pub async fn resolve_figure_search_results(
    data_dir: &std::path::Path,
    results: &mut [crate::search::FigureSearchResult],
) {
    for r in results {
        resolve_image_path(data_dir, &r.paper.id, &mut r.figure.image_path).await;
    }
}

/// Resolve relative figure image paths stored in `figures.image_path` to
/// absolute paths under `data_dir/{paper_id}`.
pub async fn resolve_figure_paths(
    data_dir: &std::path::Path,
    paper_id: &str,
    figures: &mut [crate::index::multimodal::FigureInfo],
) {
    for fig in figures {
        resolve_image_path(data_dir, paper_id, &mut fig.image_path).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn safe_join_valid_relative_path_resolves() {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let paper_dir = base.join("papers").join("p1");
        std::fs::create_dir_all(&paper_dir).unwrap();
        let file_path = paper_dir.join("fig1.png");
        std::fs::write(&file_path, b"x").unwrap();
        let result = safe_join(&base, "p1", "fig1.png").await.unwrap();
        assert_eq!(result, file_path);
    }

    #[tokio::test]
    async fn safe_join_rejects_parent_in_paper_id() {
        let temp = tempfile::tempdir().unwrap();
        let result = safe_join(temp.path(), "..", "fig.png").await;
        assert!(matches!(
            result,
            Err(crate::PaperedError::InvalidArgument(_))
        ));
    }

    #[tokio::test]
    async fn safe_join_rejects_parent_in_rel() {
        let temp = tempfile::tempdir().unwrap();
        let result = safe_join(temp.path(), "p1", "../other.png").await;
        assert!(matches!(
            result,
            Err(crate::PaperedError::InvalidArgument(_))
        ));
    }

    #[tokio::test]
    async fn safe_join_rejects_absolute_rel() {
        let temp = tempfile::tempdir().unwrap();
        let result = safe_join(temp.path(), "p1", "/etc/passwd").await;
        assert!(matches!(
            result,
            Err(crate::PaperedError::InvalidArgument(_))
        ));
    }

    #[tokio::test]
    async fn safe_join_nonexistent_file_fallback_path() {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let paper_dir = base.join("papers").join("p1");
        std::fs::create_dir_all(&paper_dir).unwrap();
        let result = safe_join(&base, "p1", "missing.png").await.unwrap();
        assert_eq!(result, paper_dir.join("missing.png"));
    }

    #[test]
    fn normalize_input_path_strips_matching_quotes() {
        assert_eq!(
            normalize_input_path("'/tmp/a b.pdf'"),
            std::path::PathBuf::from("/tmp/a b.pdf")
        );
        assert_eq!(
            normalize_input_path("\"/tmp/a b.pdf\""),
            std::path::PathBuf::from("/tmp/a b.pdf")
        );
    }

    #[test]
    fn normalize_input_path_keeps_mismatched_quotes() {
        assert_eq!(
            normalize_input_path("'/tmp/a.pdf\""),
            std::path::PathBuf::from("'/tmp/a.pdf\"")
        );
    }

    #[test]
    fn normalize_input_path_unescapes_terminal_drag_drops() {
        assert_eq!(
            normalize_input_path("/tmp/a\\ b\\ (1).pdf"),
            std::path::PathBuf::from("/tmp/a b (1).pdf")
        );
    }

    #[test]
    fn normalize_input_path_leaves_windows_separators_alone() {
        assert_eq!(
            normalize_input_path("C:\\Users\\me\\paper.pdf"),
            std::path::PathBuf::from("C:\\Users\\me\\paper.pdf")
        );
    }

    #[test]
    fn normalize_input_path_expands_tilde_after_unquoting() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            normalize_input_path("'~/Papers/a\\ b.pdf'"),
            home.join("Papers/a b.pdf")
        );
    }

    #[test]
    fn normalize_input_path_trims_whitespace() {
        assert_eq!(
            normalize_input_path("  /tmp/a.pdf \n"),
            std::path::PathBuf::from("/tmp/a.pdf")
        );
    }

    #[test]
    fn normalize_input_path_leaves_relative_and_tilde_user_untouched() {
        assert_eq!(
            normalize_input_path("papers/x.pdf"),
            std::path::PathBuf::from("papers/x.pdf")
        );
        assert_eq!(
            normalize_input_path("~user/x.pdf"),
            std::path::PathBuf::from("~user/x.pdf")
        );
    }
}
