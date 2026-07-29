# Figure Extraction

Figures are extracted through a dual-path architecture that runs in parallel with section extraction. Both paths use the same LLM client configured for `purposes.section`.

## Primary Path — LLM Extraction (on by default)

```
clean text → LLM figure extraction prompt
  → Vec<LlmFigure { label, caption }>
  → PageTextIndex locates each caption in the PDF
  → pdf_oxide renders the surrounding page as a JPEG
  → stores FigureInfo { caption, figure_label, image_path }
```

The LLM is asked to list every figure whose **caption appears in the paper text**, with its **label** (e.g. `"1"`, `"S1"`, `"3a"`, `"graphical_abstract"`) and **caption** text. The prompt (`figure_extraction_prompt.txt`) explicitly instructs the model to include the graphical abstract, and to *omit* figures that are merely cross-referenced in the body (e.g. "see Figure S2") without their caption present — such figures are not actually contained in the document. The LLM call is a focused, short-prompt extraction (separate from the main section extraction prompt), sent to the same model configured for `purposes.section`. Figures and sections are extracted concurrently via `tokio::join!`.

### Figure ID construction

Labels are sanitized via `sanitize_label()` — any character that isn't alphanumeric, hyphen, or underscore is replaced with an underscore, then consecutive underscores are collapsed. The sanitized label is used to build a unique figure ID: `{paper_id}_fig_{sanitized_label}` (e.g. `abc123_fig_1`, `abc123_fig_graphical_abstract`). For labels that sanitize to empty or collide, sequential numeric fallbacks are used.

### Caption-to-page matching

Once the LLM has produced figure labels and captions, the system must locate which PDF page each figure appears on. This is done by `PageTextIndex`, which builds a per-page normalized text index (lowercase, alphanumeric, whitespace collapsed to single spaces) from `pdf_oxide::extract_text()` — keeping the raw extracted text alongside, for the structural fallback below — plus a per-page embedded-image flag from `pdf_oxide::extract_images()`.

The matching needle is the caption body (label prefix stripped case-insensitively via `strip_figure_prefix()`), cut at whole words to ~130 chars. Two tiers:

1. **Label-anchored context matching** — Finds every *whole-word* occurrence of `"figure n"` / `"fig n"` across all pages (whole-word so `"figure 1"` never matches `"figure 10"`), then scores how well the ~40 words *following* each occurrence match the caption needle, using asymmetric character 4-gram containment (fraction of the needle's 4-grams present). A real caption ("Figure 1. Overview of ...") carries the descriptive text right after the label and scores high; a body-text cross-reference ("as shown in Figure 1, the ...") scores low. The best-scoring occurrence wins — so the caption page beats any cross-reference page. Compound labels that already carry a figure word ("Extended Data Fig. 1") are additionally anchored *bare*: the prefixed pattern `"fig extended data fig 1"` would never match the caption prefix "Extended Data Fig. 1 |", and caption-only matching cannot separate such a figure from a main figure with a near-identical caption.

2. **Full-page caption containment** — Used only when the label pattern is absent from every page (graphical abstracts, labels pdf_oxide cannot extract): the fraction of caption 4-grams present on each page, best page wins. Needles shorter than 3 words (e.g. a bare "Graphical abstract" caption) match by exact phrase instead — a 2-word phrase is specific enough, anything shorter is too generic to trust.

Scoring uses **character 4-grams rather than word bigrams** because `extract_text()` frequently splits words with stray spaces ("Overvie w of the DREAM framew ork", "st ate-of-the-art") — word bigrams break at every split, while the character stream still matches almost everywhere around each break.

**Acceptance thresholds.** A match scoring ≥ 0.8 (near-verbatim caption) is accepted on text alone. A match in [0.5, 0.8) additionally requires an embedded image on or next to the matched page (±1, covering full-page-figure layouts) — physical evidence that a figure is actually there. Below 0.5 the match is rejected. Documents with no embedded images on any page (vector-drawn figures, or image extraction unsupported) skip the image gate entirely.

**Structural fallback for paraphrased captions.** When the label occurs on some page but no occurrence is followed by matching caption text, the LLM likely paraphrased or partly invented the caption (observed on Evo 2: "Computing likelihood ratios..." — a string that appears nowhere in the PDF). Before giving up, matching anchors on the caption *block structure* in the raw page text: the label at a line start, immediately followed by a caption delimiter (`|`, `:`, `.`, `—`, `–`) and ≥ 10 words of text. Cross-references never take that shape — "(Fig. 3)" fails the line-start test and "Fig. 3b shows" fails the delimiter test. Because the structural evidence is stronger than the image-presence proxy, these matches are accepted without the medium-band image gate; the needle score only ranks candidate pages.

**Cited-only figures get no image.** When neither label-anchored matching nor the structural fallback finds a caption block (e.g. a supplementary figure cited in the body but not included in the PDF), matching returns `None` and the figure is stored with `image_path: null` — no page is rendered rather than a wrong one. The web UI shows an "Image unavailable" placeholder for such figures.

Once a page is located, `resolve_figure_page()` checks whether the preceding page is nearly empty (< 300 characters), which indicates a full-page-figure layout (image on page N-1, caption on page N). In that case, the preceding page is rendered instead.

### Image rendering

The located page is rendered via `pdf_oxide::render_page_to_image()` at 200 DPI, then resized if either dimension exceeds 2000px (Lanczos3 filter). The rendered JPEG is saved to `{paper_data_dir}/figures/{fig_id}.jpg`.

## Fallback Path — MinerU / pdf_oxide embedded images

When the LLM returns no figures (either `figure_extraction.enabled = false`, the LLM call fails, or the paper genuinely has no figures), the system falls back to images already extracted during text extraction (Step 2):

```
pdf_oxide / MinerU embedded images → FigureInfo { caption: None, figure_label: None, image_path }
```

These are raw raster images extracted from the PDF — they have image files but no assigned labels or captions. An optional vision LLM (configured via `purposes.vision`) can generate natural-language descriptions for them.

## Paths are mutually exclusive

LLM figures and fallback figures **never mix** for the same paper. If the LLM finds any figures, those are used exclusively; otherwise the fallback figures are used. Re-running `reindex` will try LLM extraction again.

## Key differences between the two paths

| | LLM Path | Fallback Path |
|---|---|---|
| `caption` | ✅ full caption text | ❌ `null` |
| `figure_label` | ✅ e.g. `"1"`, `"S1"` | ❌ `null` |
| `description` | `null` (no vision LLM) | ✅ vision LLM (if configured) |
| `image_path` | Rendered page JPEG | PDF embedded image |
| Image source | `{data_dir}/papers/{id}/figures/{id}_fig_{label}.jpg` | `{data_dir}/papers/{id}/images/figN.jpg` |

## API representation

```json
// LLM path figure
{
  "id": "abc123_fig_1",
  "paper_id": "abc123",
  "caption": "Overview of the proposed framework.",
  "figure_label": "1",
  "description": null,
  "image_path": "figures/abc123_fig_1.jpg",
  "page_number": 1,
  "bbox": null
}

// Fallback path figure (pdf_oxide)
{
  "id": "abc123_fig1",
  "paper_id": "abc123",
  "caption": null,
  "figure_label": null,
  "description": "A diagram showing the model architecture with three encoder layers...",
  "image_path": "images/fig1.jpg",
  "page_number": 1,
  "bbox": null
}
```

## Image serving

Figure images are served by `GET /api/v1/papers/{id}/figures/{figure_id}/image`. The endpoint:

1. Reads the figure's `image_path` from the `figures` table (a relative path)
2. Resolves it under `{data_dir}/papers/{id}/` via `safe_join()`, which validates containment to prevent path traversal
3. Returns the image bytes with `Content-Type` matching the file extension (JPEG, PNG, WebP, GIF, BMP) and `Cache-Control: private, no-cache` — reindex rewrites the image under the same URL, so the browser must revalidate rather than serve a stale copy. (Cover endpoints keep `max-age=3600`.)

If `image_path` is `null` (caption matching failed for that figure), the endpoint returns 404.

## Cover images

Covers are rendered from the first PDF page at 150 DPI during indexing, stored as JPEG (`{data_dir}/covers/{paper_id}.jpg`, long side ≤ 2000px) with a thumbnail (`{paper_id}_thumb.jpg`, long side ≤ 200px). The `cover_path` field is written via `update_paper`, which includes it in the SQL UPDATE statement.

List/search views use the thumbnail endpoint (`/api/v1/papers/{id}/cover/thumb`); detail view uses the full cover (`/api/v1/papers/{id}/cover`). If a thumbnail file doesn't exist (upgraded data), the thumb endpoint falls back to the full cover.

To regenerate covers for all existing papers:

```bash
curl -X POST http://127.0.0.1:9321/api/v1/health/regenerate-covers
```

## Configuration

```toml
[figure_extraction]
enabled = true   # default: true. false = skip LLM call, use fallback images only
```

When `enabled = false`, the fallback path is always taken — paper figures will have images but no labels or captions. When `enabled = true` (the default) and the LLM is available, figures get rich metadata. When `enabled = true` but the LLM is misconfigured, figure extraction fails silently and falls back to embedded images.

## Contrast with Section Extraction

| | Section Extraction | Figure Extraction |
|---|---|---|
| Requires LLM | ✅ (no fallback) | Optional (has fallback) |
| LLM client | `purposes.section` | same as sections |
| Runs in parallel | `tokio::join!` with figures | `tokio::join!` with sections |
| On LLM failure | **Indexing fails** | Falls back to embedded images |

Sections have no fallback because text cannot be semantically decomposed without an LLM. Figures have a fallback because the PDF's embedded images are independently useful.

## Relevant source files

| File | Purpose |
|------|---------|
| `crates/papered/src/index/indexer/core.rs:235-273` | Figure extraction orchestration (LLM vs fallback) |
| `crates/papered/src/index/indexer/figures.rs` | `index_llm_figures` — figure ID construction, page rendering, storage |
| `crates/papered/src/index/indexer/figure_extraction_prompt.txt` | LLM prompt for figure label + caption extraction |
| `crates/papered/src/index/indexer/helpers.rs:65-87` | `parse_llm_figures` — JSON response parsing |
| `crates/papered/src/paper/pdf_oxide.rs` | `PageTextIndex` — per-page text + image index, label-anchored caption matching, `resolve_figure_page` |
| `crates/papered/src/store/turso/vectors.rs:1260-1350` | `figures` table — insert, query, delete |
| `crates/papered/src/store/turso/vectors.rs:530-578` | `update_paper` — includes `cover_path` column |
| `crates/papered/src/cover.rs` | Cover generation from PDF first page |
| `crates/papered-daemon/src/api/papers.rs:391-466` | Figure listing and image serving endpoints |
