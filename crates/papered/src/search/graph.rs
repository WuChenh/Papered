//! Paper relatedness graph — keyword overlap + shared-entity similarity.
//!
//! Builds an undirected graph over the library where each edge weight reflects
//! how much two papers overlap in their extracted keywords and biological
//! entities (genes, species, techniques, pathways). The daemon serves this as
//! JSON for the interactive network / timeline view, which is rendered with
//! vanilla JS (no chart dependency).
//!
//! Keyword similarity blends exact-phrase Jaccard with token-level Jaccard so
//! that LLM-extracted keywords phrased differently still connect ("chromatin
//! accessibility design" ↔ "chromatin accessibility"). Entity similarity uses
//! the overlap coefficient alongside Jaccard so a focused paper is not diluted
//! when compared against a broad one (e.g. a genome-wide model paper listing
//! dozens of species).

use crate::paper::Paper;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::LazyLock;

/// Weight given to exact-keyword Jaccard similarity in the combined edge weight.
const KEYWORD_EXACT_WEIGHT: f32 = 0.3;
/// Weight given to token-level keyword Jaccard similarity in the combined edge weight.
const KEYWORD_TOKEN_WEIGHT: f32 = 0.2;
/// Weight given to shared-entity similarity in the combined edge weight.
const ENTITY_WEIGHT: f32 = 0.5;

/// Generic academic/ML filler words excluded from token-level keyword
/// comparison. Domain terms (enhancer, chromatin, motif, …) are deliberately
/// kept so they contribute signal.
const KEYWORD_STOPWORDS: &[&str] = &[
    "analysis",
    "analyses",
    "approach",
    "approaches",
    "application",
    "applications",
    "assay",
    "assays",
    "based",
    "benchmark",
    "benchmarks",
    "biological",
    "cancer",
    "cell",
    "cells",
    "characterization",
    "classification",
    "clinical",
    "clustering",
    "comparison",
    "comprehensive",
    "computational",
    "data",
    "dataset",
    "datasets",
    "deep",
    "detection",
    "disease",
    "diseases",
    "editing",
    "efficient",
    "evaluation",
    "experimental",
    "feature",
    "features",
    "framework",
    "frameworks",
    "functional",
    "gene",
    "genes",
    "generation",
    "genome",
    "genomes",
    "genomic",
    "genomics",
    "high",
    "human",
    "identification",
    "improved",
    "inference",
    "insights",
    "integrative",
    "language",
    "large",
    "learning",
    "machine",
    "method",
    "methods",
    "methodology",
    "model",
    "models",
    "modeling",
    "modelling",
    "multi",
    "network",
    "networks",
    "neural",
    "new",
    "novel",
    "optimization",
    "overview",
    "performance",
    "pipeline",
    "platform",
    "platforms",
    "prediction",
    "predictions",
    "profiling",
    "protein",
    "proteins",
    "protocol",
    "protocols",
    "quantitative",
    "regulation",
    "regulatory",
    "resource",
    "resources",
    "reveals",
    "review",
    "reviews",
    "rna",
    "robust",
    "scale",
    "scalable",
    "screening",
    "selection",
    "sensitivity",
    "sequencing",
    "single",
    "specificity",
    "statistical",
    "strategy",
    "strategies",
    "study",
    "studies",
    "survey",
    "systematic",
    "technology",
    "technologies",
    "technique",
    "techniques",
    "tool",
    "tools",
    "towards",
    "toward",
    "training",
    "transfer",
    "unified",
    "unsupervised",
    "using",
    "validation",
    "via",
    "workflow",
    "workflows",
    "whole",
    "zero-shot",
];

static STOPWORD_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| KEYWORD_STOPWORDS.iter().copied().collect());

/// A node in the paper relatedness graph.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub title: String,
    pub venue: Option<String>,
    pub published_date: Option<String>,
    /// Four-digit publication year parsed from `published_date`, for timeline binning.
    pub year: Option<i32>,
    pub keywords: Vec<String>,
}

/// A weighted edge between two papers.
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    /// Combined similarity in `[0, 1]` (exact-keyword + token-keyword + entity
    /// similarity, weighted).
    pub weight: f32,
    pub shared_keywords: Vec<String>,
    pub shared_entities: Vec<String>,
}

/// The full relatedness graph payload.
#[derive(Debug, Clone, Serialize)]
pub struct PaperGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// Precomputed normalized similarity sets for one paper, built once and reused
/// across all pairwise comparisons.
struct PaperSets {
    /// Exact keyword phrases, lowercased.
    keywords: HashSet<String>,
    /// Meaningful keyword tokens (stopword-filtered, stemmed), lowercased.
    tokens: HashSet<String>,
    /// All bio-entities (genes/species/techniques/pathways), lowercased.
    entities: HashSet<String>,
}

impl PaperSets {
    fn from_paper(p: &Paper) -> Self {
        Self {
            keywords: keyword_set(p),
            tokens: token_set(p),
            entities: entity_set(p),
        }
    }
}

/// Lowercase a string for case-insensitive set comparison.
fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Extract the publication year from a `published_date` string (e.g. "2023-05-01"
/// or "2023"). Returns `None` when no 4-digit year prefix is present.
fn parse_year(published_date: &str) -> Option<i32> {
    let digits: String = published_date.chars().take(4).collect();
    if digits.len() == 4 {
        digits.parse::<i32>().ok()
    } else {
        None
    }
}

/// Jaccard similarity `|a ∩ b| / |a ∪ b|` over two normalized sets.
/// Returns `0.0` when both sets are empty (no signal, not a perfect match).
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    }
}

/// Entity similarity: Jaccard, boosted by the overlap coefficient
/// `|a ∩ b| / min(|a|, |b|)` when at least two entities are shared. A broad
/// paper (dozens of species/genes) would otherwise dilute a real subset
/// overlap with a focused paper to near-zero under pure Jaccard.
fn entity_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    let jac = inter as f32 / union as f32;
    if inter >= 2 {
        let overlap = inter as f32 / a.len().min(b.len()) as f32;
        jac.max(overlap)
    } else {
        jac
    }
}

/// Collect a paper's keywords into a normalized set (exact-phrase matching).
fn keyword_set(p: &Paper) -> HashSet<String> {
    p.keywords
        .iter()
        .map(|k| norm(k))
        .filter(|k| !k.is_empty())
        .collect()
}

/// Crude plural stem: strip a single trailing `s` from tokens longer than 3
/// chars so "enhancers"/"enhancer" and "motifs"/"motif" compare equal.
fn stem(token: &str) -> &str {
    if token.len() > 3 && token.ends_with('s') {
        &token[..token.len() - 1]
    } else {
        token
    }
}

/// Collect a paper's keywords into a set of meaningful tokens for fuzzy
/// matching. Each keyword is split on whitespace, lowercased, stemmed, and
/// filtered through [`KEYWORD_STOPWORDS`]; short (<3 char) and purely numeric
/// tokens are dropped. Hyphenated terms ("starr-seq") are kept whole.
fn token_set(p: &Paper) -> HashSet<String> {
    p.keywords
        .iter()
        .flat_map(|k| k.split_whitespace())
        .map(|t| t.trim().to_lowercase())
        .filter(|t| {
            t.len() >= 3
                && !t.chars().all(|c| c.is_ascii_digit())
                && !STOPWORD_SET.contains(stem(t))
        })
        .map(|t| stem(&t).to_string())
        .collect()
}

/// Collect all of a paper's biological entities into one normalized set.
fn entity_set(p: &Paper) -> HashSet<String> {
    let e = &p.entities;
    e.genes
        .iter()
        .chain(e.species.iter())
        .chain(e.techniques.iter())
        .chain(e.pathways.iter())
        .map(|v| norm(v))
        .filter(|v| !v.is_empty())
        .collect()
}

/// Build a [`GraphNode`] from a paper.
fn node_from_paper(p: &Paper) -> GraphNode {
    GraphNode {
        id: p.id.clone(),
        title: p.title.clone(),
        venue: p.venue.clone(),
        year: p.published_date.as_deref().and_then(parse_year),
        published_date: p.published_date.clone(),
        keywords: p.keywords.clone(),
    }
}

/// Compute the relatedness edge between two papers from their precomputed
/// normalized sets, or `None` when they share nothing.
/// `shared_keywords` / `shared_entities` are returned in the display casing of
/// the first paper that lists them.
fn edge_between(a: &Paper, b: &Paper, sa: &PaperSets, sb: &PaperSets) -> Option<GraphEdge> {
    let exact_sim = jaccard(&sa.keywords, &sb.keywords);
    let token_sim = jaccard(&sa.tokens, &sb.tokens);
    let ent_sim = entity_similarity(&sa.entities, &sb.entities);
    let weight = KEYWORD_EXACT_WEIGHT * exact_sim
        + KEYWORD_TOKEN_WEIGHT * token_sim
        + ENTITY_WEIGHT * ent_sim;
    if weight <= 0.0 {
        return None;
    }

    // Recover display casing from paper `a` (fallback to `b` for items only it has).
    let shared_keywords: Vec<String> = a
        .keywords
        .iter()
        .filter(|k| sb.keywords.contains(&norm(k)))
        .cloned()
        .collect();
    let shared_entities: Vec<String> = {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for v in a
            .entities
            .genes
            .iter()
            .chain(a.entities.species.iter())
            .chain(a.entities.techniques.iter())
            .chain(a.entities.pathways.iter())
            .chain(b.entities.genes.iter())
            .chain(b.entities.species.iter())
            .chain(b.entities.techniques.iter())
            .chain(b.entities.pathways.iter())
        {
            let n = norm(v);
            if sa.entities.contains(&n) && sb.entities.contains(&n) && seen.insert(n) {
                out.push(v.clone());
            }
        }
        out
    };

    Some(GraphEdge {
        source: a.id.clone(),
        target: b.id.clone(),
        weight,
        shared_keywords,
        shared_entities,
    })
}

/// Compute the relatedness edge between two papers, building their comparison
/// sets on the fly. Public wrapper for one-off comparisons (e.g. the search
/// engine's graph-focus expansion); the batch builder precomputes sets instead.
pub fn relatedness_edge(a: &Paper, b: &Paper) -> Option<GraphEdge> {
    edge_between(a, b, &PaperSets::from_paper(a), &PaperSets::from_paper(b))
}

/// Build the relatedness graph over a set of papers.
///
/// `max_edges_per_node` caps how many of the strongest edges each node keeps so
/// the rendered network stays readable for large libraries. Edges are symmetric;
/// the cap is applied per node after sorting by weight descending.
pub fn build_paper_graph(papers: &[Paper], max_edges_per_node: usize) -> PaperGraph {
    let nodes: Vec<GraphNode> = papers.iter().map(node_from_paper).collect();

    // Precompute each paper's normalized keyword/token/entity sets once — the
    // pairwise loop is O(n²), and rebuilding the sets per pair would
    // quadratically re-allocate (~500k pairs at limit=1000).
    let sets: Vec<PaperSets> = papers.iter().map(PaperSets::from_paper).collect();

    let mut edges: Vec<GraphEdge> = Vec::new();
    for i in 0..papers.len() {
        for j in (i + 1)..papers.len() {
            if let Some(e) = edge_between(&papers[i], &papers[j], &sets[i], &sets[j]) {
                edges.push(e);
            }
        }
    }

    // Strongest edges first so the per-node cap keeps the most relevant links.
    edges.sort_by(|a, b| b.weight.total_cmp(&a.weight));

    if max_edges_per_node > 0 {
        let mut degree: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        edges.retain(|e| {
            let ds = *degree.get(&e.source).unwrap_or(&0);
            let dt = *degree.get(&e.target).unwrap_or(&0);
            if ds >= max_edges_per_node || dt >= max_edges_per_node {
                false
            } else {
                *degree.entry(e.source.clone()).or_insert(0) += 1;
                *degree.entry(e.target.clone()).or_insert(0) += 1;
                true
            }
        });
    }

    PaperGraph { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(id: &str, keywords: &[&str], genes: &[&str]) -> Paper {
        let mut p = Paper::new(format!("Paper {id}"));
        p.id = id.to_string();
        p.keywords = keywords.iter().map(|s| s.to_string()).collect();
        p.entities.genes = genes.iter().map(|s| s.to_string()).collect();
        p
    }

    #[test]
    fn jaccard_basic() {
        let a: HashSet<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["b", "c", "d"].iter().map(|s| s.to_string()).collect();
        // |∩|=2, |∪|=4 → 0.5
        assert!((jaccard(&a, &b) - 0.5).abs() < 1e-6);
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(jaccard(&empty, &empty), 0.0);
    }

    #[test]
    fn parse_year_variants() {
        assert_eq!(parse_year("2023-05-01"), Some(2023));
        assert_eq!(parse_year("1999"), Some(1999));
        assert_eq!(parse_year("unknown"), None);
        assert_eq!(parse_year(""), None);
    }

    #[test]
    fn graph_links_papers_with_shared_keyword() {
        let papers = vec![
            paper("p1", &["CRISPR", "gene editing"], &["BRCA1"]),
            paper("p2", &["crispr", "delivery"], &["BRCA1"]),
            paper("p3", &["unrelated"], &[]),
        ];
        let g = build_paper_graph(&papers, 10);
        assert_eq!(g.nodes.len(), 3);
        // p1–p2 share "crispr" (case-insensitive) and "BRCA1".
        assert_eq!(g.edges.len(), 1);
        let e = &g.edges[0];
        assert!(
            e.shared_keywords
                .iter()
                .any(|k| k.eq_ignore_ascii_case("crispr"))
        );
        assert!(e.shared_entities.iter().any(|v| v == "BRCA1"));
        assert!(e.weight > 0.0);
    }

    #[test]
    fn graph_no_edges_when_nothing_shared() {
        let papers = vec![paper("p1", &["a"], &[]), paper("p2", &["b"], &["x"])];
        let g = build_paper_graph(&papers, 10);
        assert!(g.edges.is_empty());
    }

    #[test]
    fn per_node_edge_cap_applies() {
        // Four papers all share one keyword → 6 candidate edges. With a cap of
        // 1 edge per node the result is a perfect matching (hub–a and b–c),
        // i.e. 2 edges, and no node exceeds degree 1.
        let papers = vec![
            paper("hub", &["shared"], &[]),
            paper("a", &["shared"], &[]),
            paper("b", &["shared"], &[]),
            paper("c", &["shared"], &[]),
        ];
        let g = build_paper_graph(&papers, 1);
        assert_eq!(g.edges.len(), 2);
        let mut degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for e in &g.edges {
            *degree.entry(e.source.as_str()).or_insert(0) += 1;
            *degree.entry(e.target.as_str()).or_insert(0) += 1;
        }
        assert!(degree.values().all(|d| *d <= 1));
    }

    #[test]
    fn token_matching_links_paraphrased_keywords() {
        // No exact keyword match, but "chromatin accessibility design" and
        // "chromatin accessibility" share the tokens {chromatin, accessibility}.
        let papers = vec![
            paper(
                "p1",
                &["chromatin accessibility design", "sparse autoencoder"],
                &[],
            ),
            paper(
                "p2",
                &["chromatin accessibility", "motif implantation"],
                &[],
            ),
        ];
        let g = build_paper_graph(&papers, 10);
        assert_eq!(
            g.edges.len(),
            1,
            "token overlap must link paraphrased keywords"
        );
        assert!(g.edges[0].weight > 0.0);
    }

    #[test]
    fn stem_matches_plurals() {
        // "synthetic enhancers" vs "enhancer design" — "enhancers" stems to
        // "enhancer" and matches.
        let papers = vec![
            paper("p1", &["synthetic enhancers"], &[]),
            paper("p2", &["enhancer design"], &[]),
        ];
        let g = build_paper_graph(&papers, 10);
        assert_eq!(g.edges.len(), 1, "plural stem must link enhancers/enhancer");
    }

    #[test]
    fn stopwords_do_not_link_unrelated_papers() {
        // Both keywords reduce to stopword-only token sets ("deep"/"learning"
        // and "model" are stopwords), so no token signal and no edge.
        let papers = vec![
            paper("p1", &["deep learning"], &[]),
            paper("p2", &["prediction model"], &[]),
        ];
        let g = build_paper_graph(&papers, 10);
        assert!(g.edges.is_empty(), "stopwords alone must not create edges");
    }

    #[test]
    fn entity_overlap_boosts_broad_vs_focused_paper() {
        // A broad paper with many species and a focused paper whose species are
        // a subset. Pure Jaccard is tiny (2 shared / large union); the overlap
        // coefficient surfaces the real subset relationship.
        let mut broad = paper("broad", &["foundation model"], &[]);
        broad.entities.species = (0..30).map(|i| format!("species{i}")).collect();
        broad
            .entities
            .species
            .push("Drosophila melanogaster".into());
        broad.entities.species.push("Mus musculus".into());
        let mut focused = paper("focused", &["enhancer design"], &[]);
        focused.entities.species = vec!["Drosophila melanogaster".into(), "Mus musculus".into()];
        let papers = vec![broad, focused];
        let g = build_paper_graph(&papers, 10);
        assert_eq!(
            g.edges.len(),
            1,
            "subset entity overlap must link broad/focused papers"
        );
        // Overlap coefficient = 2/2 = 1.0 → weight ≈ ENTITY_WEIGHT.
        assert!(g.edges[0].weight >= ENTITY_WEIGHT * 0.9);
    }

    #[test]
    fn single_shared_entity_uses_jaccard_not_overlap() {
        // With only one shared entity the overlap coefficient is not applied
        // (it would over-weight a single generic match like "Homo sapiens").
        // Keywords are token-disjoint so only the entity leg contributes.
        let mut a = paper("a", &["alpha beta"], &[]);
        a.entities.species = vec!["Homo sapiens".into()];
        let mut b = paper("b", &["gamma delta"], &[]);
        b.entities.species = vec!["Homo sapiens".into()];
        let papers = vec![a, b];
        let g = build_paper_graph(&papers, 10);
        assert_eq!(g.edges.len(), 1);
        // Entity Jaccard = 1/1 = 1.0 (both sets size 1) → weight = ENTITY_WEIGHT.
        assert!((g.edges[0].weight - ENTITY_WEIGHT).abs() < 1e-6);
    }
}
