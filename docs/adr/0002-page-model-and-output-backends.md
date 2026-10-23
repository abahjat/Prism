# ADR 0002: Positioned Page Model and Output Backends (`svg`, `tiny-skia`, `krilla`)

- **Status:** Accepted
- **Date:** 2026-10-23
- **Milestone:** Phase 1 — M0 (Spikes and ADRs)

## Context

Phase 1 unifies all paged document formats (PPTX, PDF, DOCX, TXT) behind a single positioned page representation (`LayoutDocument` / `LayoutPage`) produced by `prism-layout` and consumed by three output backends in `prism-render` (`svg`, `raster`, `pdf`) as well as text extraction and search:

```text
Parsers ──► UDM ──► prism-layout ──► LayoutDocument { pages: Vec<LayoutPage>, .. }
                                         ├──► prism-render::svg    (SVG + selectable HTML text layer)
                                         ├──► prism-render::raster (PNG, multi-page TIFF)
                                         ├──► prism-render::pdf    (Searchable PDF via krilla)
                                         └──► LayoutPage::text() / char_boxes() / LayoutDocument::find()
```

All outputs must be **forensically deterministic** (byte-identical output for the same input and engine version) and enforce resource limits (`LayoutOptions`: max pages, max glyphs per page, max image pixels, max recursion depth, max decompressed stream bytes).

## Page Model Specification (`crates/prism-layout`)

- **Coordinate system:** Points (`1 pt = 1/72 in`), origin `(0, 0)` at top-left, `+x` right, `+y` down. Z-order is slice iteration order in `LayoutPage::items`.
- **Serialization & Determinism:** All model types derive `Serialize` and `Deserialize`. Maps serialized or iterated into output use `BTreeMap` (never `HashMap`) so iteration order is strictly deterministic. Floating-point values in SVG/HTML formatting use fixed precision formatting and normalize `-0.0` to `0.0`.
- **Core types:**
  - `LayoutDocument`: `{ pages: Vec<LayoutPage>, fonts: FontStore, images: ImageStore, font_report: FontReport }`
  - `LayoutPage`: `{ index: u32, size: Size, items: Vec<Item>, label: Option<String> }`
  - `Item`:
    - `Glyphs(GlyphRun)` — positioned glyph run with source text and grapheme clusters
    - `Path(PathItem)` — vector path (`commands: Vec<PathOp>`, `fill: Option<FillPaint>`, `stroke: Option<StrokeStyle>`, `transform: Affine`)
    - `Image(ImageItem)` — raster image reference (`image: ImageId`, `rect: Rect`, `transform: Affine`, `opacity: f32`)
    - `Group(GroupItem)` — scoped group (`transform: Affine`, `clip: Option<ClipShape>`, `opacity: f32`, `children: Vec<Item>`)
    - `Link(LinkItem)` — inert link metadata (`rect: Rect`, `target: String`; exposed as data/hover only, never navigated)
    - `Anchor(AnchorItem)` — named position or target rect (`name: String`, `rect: Rect`) for bookmarks, comments, and future redactions
  - `GlyphRun`:
    - `font: FontId`, `size: f32`, `color: Rgba`, `transform: Affine`
    - `glyphs: Vec<PositionedGlyph>` (`{ glyph_id: u16, x: f32, y: f32, advance: f32 }`, where `(x, y)` is in run space prior to `transform`)
    - `text: String` — UTF-8 source text for this run
    - `clusters: Vec<Cluster>` — `{ text_range: Range<u32>, glyph_range: Range<u32>, advance: f32, x: f32 }`
    - `decorations: Decorations` — `{ underline: Option<LineDecoration>, strikethrough: Option<LineDecoration>, highlight: Option<Rgba> }`
    - `source: SourceRef` — `{ page: u32, block: u32, run: u32 }`
- **Text, Character Boxes, and Search:**
  - `LayoutPage::text(&self) -> String`: reconstructs reading-order text exclusively from `Item::Glyphs` (recursing into `Item::Group`), inserting spaces/newlines based on baseline and horizontal advance gaps when runs belong to distinct lines or blocks.
  - `LayoutPage::char_boxes(&self) -> Vec<CharBox>`: computes per-character (and per-cluster) bounding boxes in page coordinates (`Rect`) from each `Cluster`'s `x`, `advance`, font ascent/descent (or `size`), and `GlyphRun::transform`.
  - `LayoutDocument::find(&self, query: &str, options: SearchOptions) -> Vec<SearchHit>`: supports case-insensitive (`case_sensitive: bool`), diacritic-insensitive (`ignore_diacritics: bool`, via Unicode NFD stripping of combining marks), and whole-word (`whole_word: bool`) matching, returning `SearchHit { page: u32, text: String, rects: Vec<Rect> }` where `rects` are coalesced per line from `char_boxes()`.

## Output Backend Evaluations

### 1. SVG + Viewer HTML Wrapper (`prism-render::svg`)

- Emits one self-contained `<svg>` per `LayoutPage`.
- **Deterministic glyph outlines:** For every unique `(FontId, glyph_id)` used on the page (ordered by `BTreeMap<(FontId, u16), String>`), `skrifa` (or the PDF embedded outline cache) extracts unhinted Bézier path commands at `1000` units-per-em (`d="..."`) into `<defs><path id="g-{font}-{gid}" d="..."/></defs>`. Each `PositionedGlyph` is drawn with `<use href="#g-{font}-{gid}" transform="..."/>`. This requires zero web fonts, works identically on every OS, and complies with `font-src data: blob:` / `default-src 'none'`.
- **Selectable text layer:** An overlaid `<div class="prism-text-layer">` places transparent (`color: transparent`) `<span>` elements aligned to the run/character boxes from `LayoutPage::char_boxes()`, enabling native browser text selection and copy-paste.

### 2. 2D Rasterization: `tiny-skia 0.12.0` vs `vello_cpu 0.3.0` (`prism-render::raster`)

| Criterion | `tiny-skia 0.12.0` | `vello_cpu 0.3.0` |
| :--- | :--- | :--- |
| License | `BSD-3-Clause` | `Apache-2.0 OR MIT` |
| Configurable anti-aliasing | **Yes:** per-draw `Paint::anti_alias: bool` and `Mask::from_path(..., anti_alias: bool)` | Limited (pipeline-level LUT settings; no per-path AA toggle on `RenderContext`) |
| Dimension limits | `u32` dimensions (supports arbitrary DPI up to `LayoutOptions::max_image_pixels`) | `u16` width/height (`RenderContext::new(u16, u16)`) |
| M0 Spike (150 DPI Letter, `1275×1650`) | **`14.9 ms`** (unoptimized build) | `94.6 ms` (unoptimized build) |
| Direct dependency footprint | 7 crates (`tiny-skia-path`, `arrayvec`, `arrayref`, `bytemuck`, `cfg-if`, `log`, `png`) | 12+ crates (`vello_common`, `glifo`, `peniko`, `color`, `kurbo`, `guillotiere`, `euclid`, `fearless_simd`, ...) |

**Decision for Raster Backend:**
- Use **`tiny-skia 0.12.0`** as the primary 2D rasterizer in `prism-render::raster` for rendering `&LayoutPage` to RGBA buffers at arbitrary DPI with configurable anti-aliasing (`RasterOptions { dpi: f32, anti_alias: bool, background: Rgba }`).
- Encode single-page PNGs with `png` (with fixed compression settings and no timestamp chunks) and multi-page TIFFs with `tiff` (Deflate/LZW for color/grayscale pages, CCITT Group 4 / Fax4 or bilevel packing for 1-bit bi-level pages).
- Note: `vello_cpu 0.3.0` remains in the workspace transitively via `hayro 0.8.0` for the PDF raster-fallback path when a malformed/complex PDF page cannot be decomposed into vector primitives.

### 3. PDF Export: `krilla 0.8.2` (`prism-render::pdf`)

- **Feature configuration:** `krilla = { version = "0.8.2", default-features = false, features = ["raster-images"] }`.
  - Crucially, disabling `krilla`'s default `simple-text` feature prevents pulling in `rustybuzz 0.20.1` (`RUSTSEC-2026-0206`) and `ttf-parser 0.25.1` (`RUSTSEC-2026-0192`).
  - Because `prism-layout` already shapes text into `GlyphRun`s with `PositionedGlyph`s and `Cluster`s, `prism-render::pdf` calls `Surface::draw_glyphs(Point, &[KrillaGlyph], Font, &run.text, run.size, false)` directly.
- **Searchability & Subsetting:** `krilla` uses `subsetter` and `write-fonts` to subset embedded fonts and automatically generates `/ToUnicode` CMaps and `/ActualText` mappings from each `KrillaGlyph::text_range`.
- **Determinism:** Verified in the M0 spike that two consecutive runs of `krilla::Document::new_with(SerializeSettings::default())` produce byte-identical PDF output (`19,992` bytes, `pdf1 == pdf2`), with no creation timestamp unless explicitly supplied by the caller.

## Consequences

- **Positive:** One `LayoutPage` representation drives SVG, PNG, multi-page TIFF, searchable PDF export, text extraction, character boxes, and search hits with byte-identical determinism.
- **Negative / Trade-offs:** Converting Type1/CFF PDF fonts that lack TrueType tables into `krilla::text::Font` requires either supplying their raw OpenType/CFF stream (which `krilla` supports via `skrifa`/`subsetter`) or drawing their cached `BezPath` outlines combined with an invisible searchable fallback font run in `krilla`. This is handled cleanly in `prism-render::pdf`.
