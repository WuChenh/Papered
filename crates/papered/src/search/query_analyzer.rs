/// Zero-cost heuristic analysis of query complexity for adaptive retrieval tuning.
///
/// Unlike LLM-based analysis, this uses surface features (length, structure, word patterns)
/// to classify queries and recommend retrieval hyperparameters — no API calls needed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryComplexity {
    Simple,
    Normal,
    Complex,
}

#[derive(Debug, Clone)]
pub struct QueryProfile {
    pub char_count: usize,
    pub word_count: usize,
    pub has_comparison: bool,
    pub complexity: QueryComplexity,
}

impl QueryProfile {
    const COMPARISON_SEPARATORS: [&str; 6] = [
        " and ",
        " vs ",
        " versus ",
        " compared ",
        " between ",
        " difference ",
    ];

    pub fn analyze(query: &str) -> Self {
        let query_lower = query.to_lowercase();
        let whitespace_words = query_lower.split_whitespace().count();
        let char_count = query.chars().count();
        let cjk_chars = query.chars().filter(|&c| crate::util::is_cjk(c)).count();
        let estimated_cjk_words = if char_count > 0 { cjk_chars / 3 } else { 0 };
        let word_count = if cjk_chars > char_count / 2 {
            whitespace_words + estimated_cjk_words
        } else {
            whitespace_words
        };
        let has_comparison = Self::COMPARISON_SEPARATORS
            .iter()
            .any(|sep| query_lower.contains(sep));

        let complexity = if word_count <= 4 && !has_comparison && char_count < 60 {
            QueryComplexity::Simple
        } else if word_count > 14 || (has_comparison && word_count > 6) || char_count > 300 {
            QueryComplexity::Complex
        } else {
            QueryComplexity::Normal
        };

        Self {
            char_count,
            word_count,
            has_comparison,
            complexity,
        }
    }

    pub fn recommended_top_k(&self, base_top_k: usize) -> usize {
        match self.complexity {
            QueryComplexity::Simple => (base_top_k / 2).max(2),
            QueryComplexity::Complex => (base_top_k * 2).min(32),
            QueryComplexity::Normal => base_top_k,
        }
    }

    pub fn should_use_enhancement(&self) -> bool {
        matches!(
            self.complexity,
            QueryComplexity::Normal | QueryComplexity::Complex
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_simple_query() {
        let p = QueryProfile::analyze("deep learning");
        assert_eq!(p.complexity, QueryComplexity::Simple);
        assert_eq!(p.word_count, 2);
        assert!(!p.has_comparison);
    }

    #[test]
    fn test_analyze_complex_query() {
        let p = QueryProfile::analyze(
            "What is the difference between transformer and recurrent neural networks?",
        );
        assert_eq!(p.complexity, QueryComplexity::Complex);
        assert!(p.has_comparison);
    }

    #[test]
    fn test_analyze_normal_query() {
        let p =
            QueryProfile::analyze("How does the attention mechanism work in transformer models");
        assert_eq!(p.complexity, QueryComplexity::Normal);
        assert!(!p.has_comparison);
    }

    #[test]
    fn test_recommended_top_k() {
        let simple = QueryProfile::analyze("cat");
        let normal = QueryProfile::analyze("explain neural network architecture in detail");
        let complex = QueryProfile::analyze(
            "Compare and contrast the architectural trade-offs between CNNs and transformers for vision tasks",
        );
        assert_eq!(simple.recommended_top_k(10), 5);
        assert_eq!(normal.recommended_top_k(10), 10);
        assert_eq!(complex.recommended_top_k(10), 20);
        assert_eq!(complex.recommended_top_k(20), 32); // capped at 32
    }

    #[test]
    fn test_should_use_enhancement() {
        let simple = QueryProfile::analyze("hi");
        let normal = QueryProfile::analyze("explain neural network architecture in detail");
        let complex = QueryProfile::analyze(
            "What are the differences between supervised and unsupervised learning?",
        );
        assert!(!simple.should_use_enhancement());
        assert!(normal.should_use_enhancement());
        assert!(complex.should_use_enhancement());
    }

    #[test]
    fn test_cjk_word_estimation() {
        let p = QueryProfile::analyze("深度学习中注意力机制的作用");
        // CJK chars dominate, so word_count uses CJK estimation
        assert!(p.word_count > 0);
        assert!(p.char_count > 0);
    }
}
