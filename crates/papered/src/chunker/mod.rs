//! Semantic chunking with hierarchical document structure.
//!
//! Splits documents into chunks based on Markdown headings and semantic boundaries,
//! maintaining parent-child relationships for parent document retrieval.

use serde::{Deserialize, Serialize};

/// Type of document chunk.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::EnumString,
    strum::Display,
    strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum ChunkType {
    Chapter,
    Section,
    Paragraph,
    Figure,
    Table,
}

impl ChunkType {
    /// Look up a variant by its canonical label.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

/// A single chunk of a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub paper_id: String,
    pub parent_id: Option<String>,
    pub chunk_type: ChunkType,
    pub content: String,
    pub start_pos: usize,
    pub end_pos: usize,
    pub page_number: Option<u32>,
    pub metadata: Option<serde_json::Value>,
}

impl Chunk {
    pub fn new(
        paper_id: impl Into<String>,
        id: impl Into<String>,
        chunk_type: ChunkType,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            paper_id: paper_id.into(),
            parent_id: None,
            chunk_type,
            content: content.into(),
            start_pos: 0,
            end_pos: 0,
            page_number: None,
            metadata: None,
        }
    }
}

/// A tree of chunks for a document.
pub type ChunkTree = Vec<Chunk>;

/// Flush the current paragraph buffer into the chunk tree.
fn flush_paragraph(
    tree: &mut ChunkTree,
    buffer: &mut String,
    paper_id: &str,
    counter: &mut usize,
    parent_id: &Option<String>,
) {
    if !buffer.trim().is_empty() {
        *counter += 1;
        let pid = format!("{paper_id}_p{counter}");
        let mut chunk = Chunk::new(paper_id, pid.clone(), ChunkType::Paragraph, buffer.trim());
        chunk.parent_id = parent_id.clone();
        tree.push(chunk);
        buffer.clear();
    }
}

/// Chunk a Markdown document into a hierarchical tree.
pub fn chunk_markdown(paper_id: &str, markdown: &str) -> ChunkTree {
    let mut tree = Vec::new();
    let mut current_section_id: Option<String> = None;
    let mut current_chapter_id: Option<String> = None;
    let mut section_counter = 0;
    let mut paragraph_counter = 0;

    let lines: Vec<&str> = markdown.lines().collect();
    let mut buffer = String::new();
    let mut in_table = false;
    let mut table_buffer = String::new();

    for line in &lines {
        let trimmed = line.trim();

        // Table detection
        if trimmed.starts_with('|') {
            if !in_table {
                in_table = true;
                flush_paragraph(
                    &mut tree,
                    &mut buffer,
                    paper_id,
                    &mut paragraph_counter,
                    &current_section_id,
                );
            }
            table_buffer.push_str(line);
            table_buffer.push('\n');
            continue;
        } else if in_table {
            in_table = false;
            section_counter += 1;
            let tid = format!("{paper_id}_t{section_counter}");
            let mut chunk = Chunk::new(paper_id, tid, ChunkType::Table, table_buffer.trim());
            chunk.parent_id = current_section_id.clone();
            tree.push(chunk);
            table_buffer.clear();
        }

        // Heading detection
        if let Some(level) = heading_level(trimmed) {
            flush_paragraph(
                &mut tree,
                &mut buffer,
                paper_id,
                &mut paragraph_counter,
                &current_section_id,
            );

            section_counter += 1;
            let sid = format!("{paper_id}_s{section_counter}");
            let heading_text = trimmed.trim_start_matches('#').trim().to_string();

            let mut chunk = Chunk::new(
                paper_id,
                sid.clone(),
                ChunkType::Section,
                heading_text.clone(),
            );

            if level == 1 {
                chunk.chunk_type = ChunkType::Chapter;
                chunk.parent_id = None;
                current_chapter_id = Some(sid.clone());
                current_section_id = Some(sid.clone());
            } else if level == 2 {
                chunk.parent_id = current_chapter_id.clone();
                current_section_id = Some(sid.clone());
            } else {
                chunk.parent_id = current_section_id.clone();
            }

            tree.push(chunk);
            continue;
        }

        buffer.push_str(line);
        buffer.push('\n');
    }

    // Flush remaining buffer
    flush_paragraph(
        &mut tree,
        &mut buffer,
        paper_id,
        &mut paragraph_counter,
        &current_section_id,
    );

    // Flush remaining table
    if in_table && !table_buffer.trim().is_empty() {
        section_counter += 1;
        let tid = format!("{paper_id}_t{section_counter}");
        let mut chunk = Chunk::new(paper_id, tid, ChunkType::Table, table_buffer.trim());
        chunk.parent_id = current_section_id;
        tree.push(chunk);
    }

    tree
}

/// Create fixed-size overlapping chunks as fallback for unstructured text.
/// Uses char-based indexing to safely handle multi-byte UTF-8 characters.
///
/// Returns an error if `chunk_size` is zero or `overlap` is not smaller
/// than `chunk_size`.
pub fn chunk_fixed_size(
    paper_id: &str,
    text: &str,
    chunk_size: usize,
    overlap: usize,
) -> crate::error::Result<ChunkTree> {
    if chunk_size == 0 {
        return Err(crate::error::PaperedError::invalid_argument(
            "chunk_size must be > 0",
        ));
    }
    if overlap >= chunk_size {
        return Err(crate::error::PaperedError::invalid_argument(
            "overlap must be < chunk_size",
        ));
    }
    let mut tree = Vec::new();
    let char_count = text.chars().count();
    let mut start = 0;
    let mut counter = 0;
    let mut byte_start = 0;
    let mut byte_start_char_idx = 0;

    while start < char_count {
        let end = (start + chunk_size).min(char_count);

        // Advance byte_start to the start-th character
        if start > byte_start_char_idx {
            let skip = start - byte_start_char_idx;
            if let Some((offset, _)) = text[byte_start..].char_indices().nth(skip) {
                byte_start += offset;
            } else {
                byte_start = text.len();
            }
            byte_start_char_idx = start;
        }

        // Find byte_end by scanning forward from byte_start
        let byte_end = text[byte_start..]
            .char_indices()
            .nth(end - start)
            .map(|(offset, _)| byte_start + offset)
            .unwrap_or(text.len());

        // Try to break at sentence boundary
        let chunk_end_byte = find_sentence_boundary(text, byte_end);

        let content = text[byte_start..chunk_end_byte].trim();
        if !content.is_empty() {
            counter += 1;
            let id = format!("{paper_id}_c{counter}");
            tree.push(Chunk::new(paper_id, id, ChunkType::Paragraph, content));
        }

        if chunk_end_byte >= text.len() {
            break;
        }

        // Find char index corresponding to chunk_end_byte
        let chunk_end_char = if chunk_end_byte <= byte_end {
            end
        } else {
            end + text[byte_end..]
                .char_indices()
                .position(|(i, _)| i >= chunk_end_byte - byte_end)
                .unwrap_or(char_count.saturating_sub(end))
        };

        let next_start = chunk_end_char.saturating_sub(overlap);
        if next_start <= start {
            start += 1; // Prevent infinite loop: advance by at least 1 char
        } else {
            start = next_start;
        }
    }

    Ok(tree)
}

fn heading_level(line: &str) -> Option<usize> {
    let mut level = 0;
    for ch in line.chars() {
        if ch == '#' {
            level += 1;
            if level > 6 {
                return None;
            }
        } else if ch.is_whitespace() {
            if level > 0 {
                return Some(level);
            }
            return None;
        } else {
            return None;
        }
    }
    None
}

fn find_sentence_boundary(text: &str, target_end_byte: usize) -> usize {
    if target_end_byte >= text.len() {
        return text.len();
    }

    // Search forward from target_end_byte for sentence-ending punctuation
    let search_end = (target_end_byte + 100).min(text.len());
    let search_text = &text[target_end_byte..search_end];

    for (idx, ch) in search_text.char_indices() {
        let next_idx = idx + ch.len_utf8();
        match ch {
            '.' | '!' | '?' => {
                if search_text[next_idx..].starts_with(|c: char| c.is_ascii_whitespace()) {
                    return target_end_byte + next_idx;
                }
            }
            '。' | '！' | '？' => {
                return target_end_byte + next_idx;
            }
            _ => {}
        }
    }

    // Fallback: break at newline
    if let Some(nl_idx) = search_text.find('\n') {
        return target_end_byte + nl_idx;
    }

    target_end_byte
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_markdown_headings_and_paragraphs() {
        let md = "# Chapter 1\n\nFirst paragraph.\n\n## Section 1.1\n\nSecond paragraph.\n";
        let tree = chunk_markdown("paper1", md);
        let types: Vec<_> = tree.iter().map(|c| c.chunk_type).collect();
        assert_eq!(
            types,
            vec![
                ChunkType::Chapter,
                ChunkType::Paragraph,
                ChunkType::Section,
                ChunkType::Paragraph,
            ]
        );
        assert_eq!(tree[0].content, "Chapter 1");
        assert_eq!(tree[1].parent_id, Some(tree[0].id.clone()));
    }

    #[test]
    fn test_chunk_markdown_table() {
        let md = "# Intro\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
        let tree = chunk_markdown("paper1", md);
        let has_table = tree.iter().any(|c| c.chunk_type == ChunkType::Table);
        assert!(has_table);
    }

    #[test]
    fn test_chunk_markdown_empty() {
        let tree = chunk_markdown("paper1", "");
        assert!(tree.is_empty());
    }

    #[test]
    fn test_chunk_fixed_size_basic() {
        let text = "This is a simple sentence. Here is another one. And a third.";
        let tree = chunk_fixed_size("paper1", text, 20, 5).unwrap();
        assert!(!tree.is_empty());
        for chunk in &tree {
            assert!(!chunk.content.is_empty());
            assert_eq!(chunk.chunk_type, ChunkType::Paragraph);
        }
    }

    #[test]
    fn test_chunk_fixed_size_invalid_args() {
        assert!(chunk_fixed_size("paper1", "text", 0, 0).is_err());
        assert!(chunk_fixed_size("paper1", "text", 10, 10).is_err());
    }

    #[test]
    fn test_chunk_fixed_size_empty() {
        let tree = chunk_fixed_size("paper1", "", 10, 2).unwrap();
        assert!(tree.is_empty());
    }

    #[test]
    fn test_heading_level_various() {
        assert_eq!(heading_level("# H1"), Some(1));
        assert_eq!(heading_level("### H3"), Some(3));
        assert_eq!(heading_level("####### Too many"), None);
        assert_eq!(heading_level("Not a heading"), None);
        assert_eq!(heading_level(""), None);
    }
}
