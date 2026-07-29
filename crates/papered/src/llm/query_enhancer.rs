//! Unified query enhancement for retrieval.
//!
//! Combines query rewriting and Hypothetical Document Embeddings (HyDE) into a
//! single LLM call that produces both a search-optimized query and a short
//! hypothetical academic paragraph. The layer owns a single cache keyed by the
//! original query.

use crate::error::Result;
use crate::index::indexer::helpers;
use crate::llm::cache::BoundedCache;
use crate::llm::metrics::MetricsSink;
use crate::llm::rate_limiter::RateLimiter;
use crate::llm::transformer::TextTransformer;
use std::sync::Mutex;

/// Configuration for the unified query enhancement layer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct QueryEnhancerConfig {
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: usize,
}

const fn default_temperature() -> f32 {
    0.3
}

const fn default_max_output_tokens() -> usize {
    16384
}

impl Default for QueryEnhancerConfig {
    fn default() -> Self {
        Self {
            temperature: default_temperature(),
            max_output_tokens: default_max_output_tokens(),
        }
    }
}

/// Number of cached enhancement results.
const ENHANCEMENT_CACHE_CAPACITY: usize = 64;

const ENHANCEMENT_SYSTEM_PROMPT: &str = r#"You are a query enhancement engine for an academic paper retrieval system.
Given a user query, produce a JSON object with exactly two fields:
- "rewritten_query": a search-optimized version of the query. Preserve all key terms, named entities, genes, species, and abbreviations. Expand abbreviations only when directly implied. Do not add information not present in the original query.
- "hypothetical_document": a short paragraph (2-4 sentences) that would appear in an academic paper and directly answers the query. Be specific, technical, and include concrete details like methods, metrics, findings, genes, proteins, or organisms. Do not mention that this is hypothetical. Do not include citations or references.

Return ONLY the JSON object, nothing else.

Examples:

Input: rice blast genes
Output: {
  "rewritten_query": "genes and molecular mechanisms involved in rice blast disease resistance (Magnaporthe oryzae)",
  "hypothetical_document": "Genome-wide association studies in rice (Oryza sativa) have identified multiple quantitative trait loci associated with resistance to Magnaporthe oryzae, including Pi-ta, Pi9, and Pikh, which encode nucleotide-binding leucine-rich repeat receptors that recognize pathogen effectors."
}

Input: transformer protein structure
Output: {
  "rewritten_query": "transformer-based deep learning methods for protein structure prediction and contact map estimation",
  "hypothetical_document": "Transformer architectures such as AlphaFold2 and ESMFold process multiple sequence alignments to predict inter-residue distances and angles, achieving high accuracy on CASP benchmarks for de novo protein structure prediction."
}

Input: CRISPR rice yield
Output: {
  "rewritten_query": "CRISPR-Cas9 genome editing applications for improving rice (Oryza sativa) grain yield and agronomic traits",
  "hypothetical_document": "CRISPR-Cas9 targeted mutagenesis has been applied to rice yield-related genes such as GS3, IPA1, and DEP1, with field trials reporting increased grain number, thousand-grain weight, and overall harvest index under diverse agronomic conditions."
}"#;

/// Result of enhancing a single query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnhancementResult {
    pub rewritten_query: String,
    pub hypothetical_document: String,
}

pub struct QueryEnhancer {
    transformer: TextTransformer,
    cache: Mutex<BoundedCache<String, EnhancementResult>>,
}

impl QueryEnhancer {
    /// Build a query enhancer backed by the given model endpoint.
    pub fn with_rate_limiter(
        endpoint: &crate::config::ModelEndpoint,
        temperature: f32,
        max_tokens: usize,
        rate_limiter: Option<RateLimiter>,
    ) -> crate::error::Result<Self> {
        let transformer =
            TextTransformer::with_rate_limiter(endpoint, temperature, max_tokens, rate_limiter)?;
        Ok(Self {
            transformer,
            cache: Mutex::new(BoundedCache::new(ENHANCEMENT_CACHE_CAPACITY)),
        })
    }

    /// Attach a metrics sink to the underlying LLM client.
    pub fn set_metrics(&mut self, sink: MetricsSink) {
        self.transformer.set_metrics(sink);
    }

    /// Enhance a user query into a more search-friendly form plus a hypothetical
    /// document that answers the query.
    pub async fn enhance(&self, query: &str) -> Result<EnhancementResult> {
        {
            let cache = self.cache.lock().unwrap();
            if let Some(cached) = cache.get(query) {
                tracing::debug!("query enhancement cache hit");
                return Ok(cached.clone());
            }
        }

        tracing::debug!("query enhancement cache miss");
        let raw = self
            .transformer
            .transform(ENHANCEMENT_SYSTEM_PROMPT, query)
            .await?;
        let result = parse_enhancement_result(query, &raw);
        {
            let mut cache = self.cache.lock().unwrap();
            cache.put(query.to_string(), result.clone());
        }
        Ok(result)
    }
}

fn parse_enhancement_result(query: &str, raw: &str) -> EnhancementResult {
    let parsed = match helpers::try_parse_llm_json::<serde_json::Value>(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "enhancement response was not valid JSON, using fallback; raw_len={}, error={}",
                raw.len(),
                e
            );
            return EnhancementResult {
                rewritten_query: query.to_string(),
                hypothetical_document: query.to_string(),
            };
        }
    };

    let rewritten = parsed
        .get("rewritten_query")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            tracing::warn!("enhancement response missing rewritten_query, using original");
            query.to_string()
        });

    let hypothetical = parsed
        .get("hypothetical_document")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            tracing::warn!(
                "enhancement response missing hypothetical_document, using original query"
            );
            query.to_string()
        });

    let result = EnhancementResult {
        rewritten_query: rewritten,
        hypothetical_document: hypothetical,
    };

    tracing::info!(
        "query enhanced: rewritten_len={} hypothetical_len={}",
        result.rewritten_query.len(),
        result.hypothetical_document.len()
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_and_miss() {
        let mut cache = BoundedCache::new(ENHANCEMENT_CACHE_CAPACITY);
        assert!(cache.get("rice blast genes").is_none());

        cache.put(
            "rice blast genes".to_string(),
            EnhancementResult {
                rewritten_query:
                    "genes and molecular mechanisms involved in rice blast disease resistance"
                        .to_string(),
                hypothetical_document: "Genome-wide association studies in rice...".to_string(),
            },
        );
        assert_eq!(
            cache.get("rice blast genes"),
            Some(&EnhancementResult {
                rewritten_query:
                    "genes and molecular mechanisms involved in rice blast disease resistance"
                        .to_string(),
                hypothetical_document: "Genome-wide association studies in rice...".to_string(),
            })
        );
    }

    #[test]
    fn cache_evicts_oldest_when_full() {
        let mut cache = BoundedCache::new(3);
        cache.put(
            "a".to_string(),
            EnhancementResult {
                rewritten_query: "A".to_string(),
                hypothetical_document: "a-doc".to_string(),
            },
        );
        cache.put(
            "b".to_string(),
            EnhancementResult {
                rewritten_query: "B".to_string(),
                hypothetical_document: "b-doc".to_string(),
            },
        );
        cache.put(
            "c".to_string(),
            EnhancementResult {
                rewritten_query: "C".to_string(),
                hypothetical_document: "c-doc".to_string(),
            },
        );

        assert_eq!(
            cache.get("a").as_ref().map(|r| &r.rewritten_query),
            Some(&"A".to_string())
        );

        cache.put(
            "d".to_string(),
            EnhancementResult {
                rewritten_query: "D".to_string(),
                hypothetical_document: "d-doc".to_string(),
            },
        );
        assert!(cache.get("a").is_none());
        assert_eq!(
            cache.get("b").as_ref().map(|r| &r.rewritten_query),
            Some(&"B".to_string())
        );
        assert_eq!(
            cache.get("c").as_ref().map(|r| &r.rewritten_query),
            Some(&"C".to_string())
        );
        assert_eq!(
            cache.get("d").as_ref().map(|r| &r.rewritten_query),
            Some(&"D".to_string())
        );
    }

    #[test]
    fn parses_valid_json() {
        let raw = r#"{"rewritten_query": "rewritten text", "hypothetical_document": "hypothetical text"}"#;
        let result = parse_enhancement_result("original", raw);
        assert_eq!(result.rewritten_query, "rewritten text");
        assert_eq!(result.hypothetical_document, "hypothetical text");
    }

    #[test]
    fn parses_fenced_json() {
        let raw = "```json\n{\"rewritten_query\": \"rewritten\", \"hypothetical_document\": \"hypo\"}\n```";
        let result = parse_enhancement_result("original", raw);
        assert_eq!(result.rewritten_query, "rewritten");
        assert_eq!(result.hypothetical_document, "hypo");
    }

    #[test]
    fn falls_back_on_missing_fields() {
        let raw = r#"{"hypothetical_document": "hypo"}"#;
        let result = parse_enhancement_result("original", raw);
        assert_eq!(result.rewritten_query, "original");
        assert_eq!(result.hypothetical_document, "hypo");
    }

    #[test]
    fn falls_back_on_empty_fields() {
        let raw = r#"{"rewritten_query": "", "hypothetical_document": ""}"#;
        let result = parse_enhancement_result("original", raw);
        assert_eq!(result.rewritten_query, "original");
        assert_eq!(result.hypothetical_document, "original");
    }

    #[test]
    fn falls_back_on_invalid_json() {
        let result = parse_enhancement_result("original", "not json");
        assert_eq!(result.rewritten_query, "original");
        assert_eq!(result.hypothetical_document, "original");
    }
}
