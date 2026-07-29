//! Structured bio-entities extracted from papers: species, genes,
//! experimental techniques, and biological pathways.
//!
//! Extracted by the section-extraction LLM pass (no extra LLM call) and
//! stored in the `paper_entities` table — not on the `papers` row — where
//! they back exact-match filtering in the list/search endpoints.

use serde::{Deserialize, Serialize};

/// Bio-entities extracted from a single paper.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BioEntities {
    /// Organism/species names (e.g. "Oryza sativa").
    #[serde(default)]
    pub species: Vec<String>,
    /// Gene/protein symbols or names as written in the paper (e.g. "OsALS").
    #[serde(default)]
    pub genes: Vec<String>,
    /// Experimental techniques and assays (e.g. "RNA-seq", "CRISPR").
    #[serde(default)]
    pub techniques: Vec<String>,
    /// Biological pathways and processes (e.g. "MAPK signaling").
    #[serde(default)]
    pub pathways: Vec<String>,
}

impl BioEntities {
    /// True when no entities were extracted in any category.
    pub fn is_empty(&self) -> bool {
        self.species.is_empty()
            && self.genes.is_empty()
            && self.techniques.is_empty()
            && self.pathways.is_empty()
    }

    /// Iterate `(kind, value)` pairs for storage.
    pub fn pairs(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("species", self.species.as_slice()),
            ("gene", self.genes.as_slice()),
            ("technique", self.techniques.as_slice()),
            ("pathway", self.pathways.as_slice()),
        ]
        .into_iter()
        .flat_map(|(kind, values)| values.iter().map(move |v| (kind, v.as_str())))
    }

    /// Route a stored `(kind, value)` row back into its category.
    /// Unknown kinds are ignored.
    pub(crate) fn insert(&mut self, kind: &str, value: String) {
        match kind {
            "species" => self.species.push(value),
            "gene" => self.genes.push(value),
            "technique" => self.techniques.push(value),
            "pathway" => self.pathways.push(value),
            _ => {}
        }
    }

    /// Merge per-window extraction results; each field is deduplicated
    /// case-insensitively with first-seen spelling preserved.
    pub fn merge(windows: &[BioEntities]) -> BioEntities {
        let mut merged = BioEntities::default();
        for w in windows {
            merged.species.extend(w.species.iter().cloned());
            merged.genes.extend(w.genes.iter().cloned());
            merged.techniques.extend(w.techniques.iter().cloned());
            merged.pathways.extend(w.pathways.iter().cloned());
        }
        merged.species = crate::util::dedup_strings(merged.species, true);
        merged.genes = crate::util::dedup_strings(merged.genes, true);
        merged.techniques = crate::util::dedup_strings(merged.techniques, true);
        merged.pathways = crate::util::dedup_strings(merged.pathways, true);
        merged
    }
}

/// Optional exact-match bio-entity filters for paper listing and search.
/// Multiple conditions combine with AND.
#[derive(Debug, Clone, Default)]
pub struct EntityFilter {
    pub species: Option<String>,
    pub gene: Option<String>,
    pub technique: Option<String>,
    pub pathway: Option<String>,
}

impl EntityFilter {
    /// True when no filter is set.
    pub fn is_empty(&self) -> bool {
        self.species.is_none()
            && self.gene.is_none()
            && self.technique.is_none()
            && self.pathway.is_none()
    }

    /// Iterate the active `(kind, value)` filter conditions.
    pub fn pairs(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("species", self.species.as_deref()),
            ("gene", self.gene.as_deref()),
            ("technique", self.technique.as_deref()),
            ("pathway", self.pathway.as_deref()),
        ]
        .into_iter()
        .filter_map(|(kind, value)| value.map(|v| (kind, v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_case_insensitive_keeps_first_spelling() {
        let items = vec![
            "OsALS".to_string(),
            "osals".to_string(),
            "OSALS".to_string(),
            "TP53".to_string(),
        ];
        assert_eq!(
            crate::util::dedup_strings(items, true),
            vec!["OsALS", "TP53"]
        );
    }

    #[test]
    fn merge_dedups_across_windows() {
        let a = BioEntities {
            species: vec!["Oryza sativa".into()],
            genes: vec!["OsALS".into()],
            ..Default::default()
        };
        let b = BioEntities {
            species: vec!["oryza sativa".into(), "Mus musculus".into()],
            genes: vec!["osals".into()],
            techniques: vec!["RNA-seq".into()],
            ..Default::default()
        };
        let merged = BioEntities::merge(&[a, b]);
        assert_eq!(merged.species, vec!["Oryza sativa", "Mus musculus"]);
        assert_eq!(merged.genes, vec!["OsALS"]);
        assert_eq!(merged.techniques, vec!["RNA-seq"]);
        assert!(merged.pathways.is_empty());
    }

    #[test]
    fn pairs_covers_all_kinds() {
        let e = BioEntities {
            species: vec!["Rice".into()],
            genes: vec!["OsALS".into()],
            techniques: vec!["CRISPR".into()],
            pathways: vec!["MAPK".into()],
        };
        let pairs: Vec<(&str, &str)> = e.pairs().collect();
        assert_eq!(
            pairs,
            vec![
                ("species", "Rice"),
                ("gene", "OsALS"),
                ("technique", "CRISPR"),
                ("pathway", "MAPK"),
            ]
        );
    }

    #[test]
    fn insert_routes_known_kinds_and_ignores_unknown() {
        let mut e = BioEntities::default();
        e.insert("gene", "TP53".into());
        e.insert("bogus", "ignored".into());
        assert_eq!(e.genes, vec!["TP53"]);
        assert!(e.species.is_empty());
    }

    #[test]
    fn entity_filter_pairs_only_active() {
        let f = EntityFilter {
            gene: Some("OsALS".into()),
            ..Default::default()
        };
        assert!(!f.is_empty());
        let pairs: Vec<(&str, &str)> = f.pairs().collect();
        assert_eq!(pairs, vec![("gene", "OsALS")]);
        assert!(EntityFilter::default().is_empty());
    }

    #[test]
    fn serde_round_trip_and_partial_default() {
        let e = BioEntities {
            genes: vec!["OsALS".into()],
            ..Default::default()
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: BioEntities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        // Missing fields default to empty arrays.
        let partial: BioEntities = serde_json::from_str(r#"{"genes":["TP53"]}"#).unwrap();
        assert_eq!(partial.genes, vec!["TP53"]);
        assert!(partial.species.is_empty());
    }
}
