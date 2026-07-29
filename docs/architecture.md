# Architecture

## Processing Pipeline

See the [mermaid diagrams in API.md](API.md#architecture) for the overall architecture and processing pipeline.

## The 10 Extracted Sections

Each paper is decomposed into independently embedded semantic sections:

| Section | What it captures |
|---------|------------------|
| `problem_statement` | The gap, motivation, unanswered question |
| `core_contribution` | What this paper actually brings to the table |
| `key_insight` | The deepest conceptual breakthrough |
| `methodology` | Methods, models, algorithms, technical approaches |
| `experimental_design` | Datasets, evaluation metrics, experimental setup |
| `key_findings` | Primary results, performance numbers, conclusions |
| `related_work` | Positioning against prior work |
| `application` | Real-world deployments and practical uses |
| `limitations` | Weaknesses, unresolved issues, critical assessment |
| `metadata` | Title, authors, venue, published date, DOI |

Each section is independently embedded via a vision-language model (Qwen3-VL-Embedding by default). This enables targeted queries — search `methodology` for techniques, `key_insight` for conceptual breakthroughs, `application` for implementations.

## Figure Extraction

Full technical details in [`docs/figure-extraction.md`](figure-extraction.md).

## Relatedness Graph

The interactive network/timeline view (`/api/v1/graph`) shows how papers in the
library relate to each other. Each paper is a **node**; an undirected **edge**
connects two papers when they share keywords or biological entities, weighted by
how much they overlap.

The source of truth is `crates/papered/src/search/graph.rs`.

### Similarity components

An edge weight is a weighted sum of three signals, each in `[0, 1]`:

```
weight = 0.3 × exact_keyword_sim + 0.2 × token_keyword_sim + 0.5 × entity_sim
```

If `weight ≤ 0` the edge is omitted entirely.

#### 1. Exact-keyword similarity (weight 0.3)

Jaccard index over the set of LLM-extracted keyword phrases, lowercased:

```
exact_keyword_sim = |A ∩ B| / |A ∪ B|
```

Two papers that both list `"CRISPR"` and `"gene editing"` as keywords score
`2/2 = 1.0` on this leg. Phrasing differences (`"deep learning"` vs
`"machine learning"`) score zero here — that is what the token leg is for.

#### 2. Token-keyword similarity (weight 0.2)

Each keyword phrase is split into individual tokens, lowercased, stemmed (trailing
`s` stripped from tokens longer than 3 characters so *enhancers*/*enhancer* and
*motifs*/*motif* match), and filtered through a stopword list of generic
academic/ML filler words (`deep`, `learning`, `model`, `analysis`, `framework`,
`gene`, `protein`, `cell`, `rna`, …). Domain terms (`chromatin`, `enhancer`,
`motif`, `transcription`, `drosophila`, `starr-seq`, …) are deliberately kept.

```
token_keyword_sim = Jaccard over the two papers' filtered token sets
```

This catches paraphrased keywords: `"chromatin accessibility design"` and
`"chromatin accessibility"` share tokens `{chromatin, accessibility}` even
though the full phrases do not match exactly.

#### 3. Entity similarity (weight 0.5)

All bio-entities (genes, species, techniques, pathways) are merged into one
normalized set per paper. The similarity starts as Jaccard but is **boosted by
the overlap coefficient** when at least two entities are shared:

```
entity_sim = max(jaccard, overlap_coefficient)   when |A ∩ B| ≥ 2
           = jaccard                              otherwise

overlap_coefficient = |A ∩ B| / min(|A|, |B|)
```

This addresses a real problem: a broad paper (e.g. a genome-wide model listing
30 species) and a focused paper (2 species, both a subset of the broad paper's)
would score near zero under pure Jaccard (`2/32 ≈ 0.06`) even though the
focused paper's entities are fully contained in the broad paper's. The overlap
coefficient gives `2/2 = 1.0`, correctly surfacing the subset relationship.

When only one entity is shared the overlap coefficient is not applied — a
single generic match like *Homo sapiens* should not dominate the edge weight.

### Degree cap

After all pairwise edges are computed, they are sorted by weight descending and
a per-node cap (`max_edges_per_node`, default 4) keeps only the strongest edges
per paper. This prevents hub nodes from dominating the rendered network and
keeps the graph readable for large libraries.

### Data loading

`list_papers_filtered` reads only the `papers` table; bio-entities live in the
separate `paper_entities` table. `paper_graph()` in `engine.rs` batch-loads
entities via a single `SELECT paper_id, kind, value FROM paper_entities WHERE
paper_id IN (...)` query after listing papers, so the entity-similarity leg has
real data to work with.

## Storage

- **Turso** — Pure-Rust SQLite-compatible engine. Unified storage for vector embeddings (via `vector32()` and `vector_distance_cos()`), full-text search (via Tantivy-backed `USING fts` indexes), and all metadata (papers, sections, chunks, figures, prompts). Single `.db` file with WAL mode and foreign-key cascades.
- **Schema** — Created in full on first open by `init_tables`; there is no migration layer (pre-1.0, no backward compatibility). The `paper_entities(paper_id, kind, value)` table stores structured bio-entities extracted during indexing, with a `(kind, value)` index for exact-match filtering. The `translations(paper_id, content_type, content_ref, target_language, source_hash, translated_text, ...)` table stores cached translations (unique per content item + language) with an FTS5 index for searching translated text.
- **Data directory** — `~/Library/Application Support/papered/` (configurable via `data_dir`)

## Module Overview

```
crates/papered/src/
├── lib.rs              # Crate root, re-exports, Prompt type
├── error.rs            # Error types + ergonomic constructors
├── config.rs           # TOML config, endpoint management, validation
├── client.rs           # Daemon HTTP client (shared by CLI binary)
├── retrieval.rs        # Two-tier context assembly
├── json_repair.rs      # JSON repair utilities for malformed LLM responses
├── test_support.rs     # Shared test fixtures (cfg(test) only)
├── util.rs             # Shared utilities: IndexJob, token estimation,
│   │                   # truncate_chars, build_extra_json, sha256, MIME
│   ├── file_limits.rs  # File size limits for uploads
│   ├── fs.rs           # Filesystem helpers
│   ├── image.rs        # Image optimization (resize/re-encode extracted figures)
│   ├── macos.rs        # macOS defaults read / port discovery
│   ├── paths.rs        # Paper-ID safety validation, path resolution
│   └── str_enum.rs     # str_enum! macro for string-serialized enums
├── paper/              # Paper model, sections, parser, MinerU, pdf_oxide, DocumentSource
│   ├── mod.rs
│   ├── section.rs      # The 10 extracted section types
│   ├── status.rs       # PaperStatus enum (str_enum!)
│   ├── source.rs       # Paper source types (file, Lattice, Zotero)
│   ├── format.rs       # Paper formatting helpers
│   ├── parser.rs       # Multi-format parser dispatch (pdf, md, txt, tex, image, docx)
│   ├── pdf_oxide.rs    # Built-in zero-dependency PDF extractor + quality scoring
│   ├── mineru.rs       # MinerU API client (MinerUConfig, MinerUMode::label/base_url)
│   └── processor.rs    # Windowing of long paper text before LLM section extraction
├── chunker/            # Semantic chunking with hierarchy + fixed-size fallback
├── llm/                # All LLM-related modules
│   ├── mod.rs
│   ├── client.rs       # Generic LLM client
│   ├── provider.rs     # Provider enum
│   ├── transformer.rs  # Generic text transformer using an LLM client
│   ├── config_macros.rs # gen_llm_feature_config! shared config macro
│   ├── embed/          # VL embedding client (batcher, LRU cache)
│   ├── reranker.rs     # Neural cross-encoder reranker (single RerankInput type)
│   ├── query_enhancer.rs # Unified query rewriting + HyDE layer
│   ├── rag.rs          # RAG pipeline
│   ├── translation.rs  # Academic text translation (single + batch, content-type aware)
│   └── rate_limiter.rs # Concurrency + RPM + TPM limiting
├── index/              # All indexing-related modules
│   ├── mod.rs
│   ├── indexer.rs      # Module root (re-exports Indexer)
│   │   ├── core.rs     # Indexer struct + main ingestion pipeline
│   │   ├── figures.rs  # Figure/table multimodal indexing + LLM description
│   │   ├── images.rs   # Standalone image indexing
│   │   ├── reindex.rs  # Reindex, sections-only reindex, re-embed
│   │   └── helpers.rs  # JSON parsing, metadata extraction, prompt building
│   ├── export.rs       # Markdown/JSON/SQLite export
│   ├── multimodal.rs   # Figure/table parsing from markdown
│   └── (multimodal embedding merged into llm/embed/client.rs)
├── search/             # Search engine + query complexity analyzer
│   ├── mod.rs
│   ├── engine.rs       # SearchEngine
│   ├── method.rs       # SearchMethod enum (semantic/fulltext/hybrid)
│   └── query_analyzer.rs # Heuristic query complexity for adaptive retrieval
├── store/              # Storage layer
│   ├── mod.rs
│   ├── vector.rs       # VectorStore trait + record types
│   └── turso.rs        # TursoStore: papers, chunks, sections, prompts,
│       │               # figures, health, store_meta ops
│       ├── query_builder.rs # SQL query construction helpers
│       └── vectors.rs  # Vector ops + VectorStore trait impl
├── cli/                # CLI model-switching subcommands (used by main.rs)
├── lattice/            # Lattice reference manager sync (client, types, syncer)
├── zotero/             # Zotero desktop sync (client, types, source, syncer)
├── sync/               # Shared sync types (SyncReport)
└── main.rs             # CLI binary entry point
```
