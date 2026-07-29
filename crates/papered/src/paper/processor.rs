//! Windowing of long paper text before LLM section extraction.
//!
//! Papers that exceed the model context window are split into windows that
//! each fit, so multi-pass extraction never silently drops the tail of a long
//! paper. Windows prefer paragraph (`\n\n`) boundaries and fall back to
//! newline / hard char cuts.
//!
//! References/bibliography stripping is delegated to the LLM prompt; the LLM
//! is instructed to ignore citation-heavy sections when extracting sections.

/// Split `text` into windows that each fit within `max_chars`, preferring
/// paragraph (`\n\n`) boundaries and falling back to newline / hard char cuts.
///
/// Never drops content: the concatenation of the returned windows covers the
/// whole input (modulo the `\n\n` separators between windows). Used to drive
/// multi-pass LLM extraction for papers that exceed the model context window,
/// so the tail of a long paper is no longer silently discarded.
///
/// Returns a single-element vec when the input already fits. Always returns at
/// least one element.
pub fn split_text_windows(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 || text.is_empty() {
        return vec![text.to_string()];
    }
    // Fast path: byte length <= max_chars implies char count <= max_chars.
    if text.len() <= max_chars || text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }

    let mut windows: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;

    for para in text.split("\n\n") {
        let para_chars = para.chars().count();

        if para_chars > max_chars {
            if !current.is_empty() {
                windows.push(std::mem::take(&mut current));
                current_chars = 0;
            }
            windows.extend(hard_split(para, max_chars));
            continue;
        }

        let join = if current.is_empty() { 0 } else { 2 }; // the "\n\n" rejoin separator
        if current_chars + join + para_chars > max_chars && !current.is_empty() {
            windows.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        if !current.is_empty() {
            current.push_str("\n\n");
            current_chars += 2;
        }
        current.push_str(para);
        current_chars += para_chars;
    }

    if !current.is_empty() {
        windows.push(current);
    }
    if windows.is_empty() {
        windows.push(text.to_string());
    }
    windows
}

/// Hard-split an oversized fragment into chunks of at most `max_chars`,
/// backtracking to the last newline within a chunk when doing so keeps at
/// least half of the budget (avoids tiny fragments).
fn hard_split(s: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + max_chars).min(chars.len());
        let mut cut = end;
        if end < chars.len() {
            let min_keep = start + max_chars / 2;
            for k in (start..end).rev() {
                if chars[k] == '\n' && k >= min_keep {
                    cut = k + 1; // keep the newline with the current chunk
                    break;
                }
            }
        }
        out.push(chars[start..cut].iter().collect());
        start = cut;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_returns_single_window_when_it_fits() {
        let w = split_text_windows("short text", 100);
        assert_eq!(w, vec!["short text".to_string()]);
    }

    #[test]
    fn split_breaks_on_paragraph_boundaries() {
        let para = "a".repeat(60);
        let text = [para.clone(), para.clone(), para.clone()].join("\n\n");
        let windows = split_text_windows(&text, 130);
        assert!(windows.len() >= 2);
        for w in &windows {
            assert!(
                w.chars().count() <= 130,
                "window too large: {}",
                w.chars().count()
            );
        }
        // No paragraph content lost.
        let joined = windows.join("\n\n");
        assert_eq!(joined.matches('a').count(), 180);
    }

    #[test]
    fn split_hard_splits_oversized_paragraph_without_loss() {
        let text = "x".repeat(250); // no paragraph/newline boundaries
        let windows = split_text_windows(&text, 100);
        assert!(windows.len() >= 3);
        for w in &windows {
            assert!(w.chars().count() <= 100);
        }
        let total: usize = windows.iter().map(|w| w.chars().count()).sum();
        assert_eq!(total, 250);
    }

    #[test]
    fn split_handles_multibyte_chars() {
        let text = "中".repeat(250);
        let windows = split_text_windows(&text, 100);
        assert!(windows.len() >= 3);
        for w in &windows {
            assert!(w.chars().count() <= 100);
        }
        let total: usize = windows.iter().map(|w| w.chars().count()).sum();
        assert_eq!(total, 250);
    }
}
