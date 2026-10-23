# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
### Added
- `prism-word`: complete Phase 2 Word layout and legacy binary `.doc` engine:
  - Rich format-independent Word document model (`WordDocument`, `WordStyleSheet`, `WordNumbering`, `WordTheme`, `WordSettings`, `WordSection`, `WordParagraph`, `WordRun`, `WordTable`, `WordDrawing`, `WordField`, `WordStory`, `WordComment`, `ForensicFacts`), style inheritance and toggle-property XOR evaluation (`prism_word::inheritance`), full WordprocessingML `.docx` reader (`prism_word::docx`), and UDM conversion (`prism_word::udm`).
  - Stateful numbering/list engine (`prism_word::numbering`) supporting multi-level `%1.%2.` templates, `startOverride`, `lvlRestart`, `isLgl`, and Symbol/Wingdings/Webdings PUA bullet mapping.
  - Native two-pass Word layout engine (`prism_word::layout::layout_word_document`) with multi-section and multi-column flow, mirror margins + gutter, headers/footers with section inheritance, footnotes/endnotes, `PAGE`/`NUMPAGES`/`SECTIONPAGES` resolution, custom tab stops + leaders, paragraph borders (`between` merging) + shading, `keepNext`/`keepLines`/widow-orphan control, tables (`AutoFit` + `gridSpan` + `vMerge` + `tblLook` conditional styles + ECMA-376 §17.4.26 border conflict resolution + repeating header rows), floating DrawingML/VML objects with wrap exclusion zones, and tracked-changes `MarkupMode` (`AllMarkup`, `SimpleMarkup`, `NoMarkup`, `Original`) with margin change bars and comment balloons.
  - Pure-Rust WMF/EMF vector metafile interpreter (`prism_word::emf`).
  - Pure-Rust legacy binary `.doc` (`[MS-DOC]`) reader (`prism_word::doc`) parsing `FIB`, `Clx`/`PlcPcd` piece tables (8-bit `CP-1252` and 16-bit `UTF-16LE`), `PlcfBteChpx`/`PlcfBtePapx` FKP pages and `grpprl` SPRMs, inline fields, and tables into `WordDocument`.
  - C ABI `prism_document_set_markup_mode` and `prism_document_forensic_facts`, viewer markup-mode selector (`#prism-markup-select`), fuzz targets (`word_docx`, `word_doc`, `word_layout`), ADRs `0004`–`0006`, `docs/architecture/word.md`, and `docs/word-compat.md`.
- `prism-layout`: new native Rust positioned layout core (`LayoutDocument`, `LayoutPage`, `Item`, `GlyphRun`, `PositionedGlyph`, `Cluster`, `Decorations`, `SourceRef`, `LayoutOptions`, `FontStore`, `ImageStore`, `FontReport`).
- `prism_layout::udm`: fixed-layout converter for PPTX slides (text boxes, shapes, linear gradients, tables, images) and flow-layout paginator for DOCX and TXT documents (section margins `<w:pgMar>`, paragraph shaping, list indentation, table pagination).
- `layout_document` returns `Error::UnsupportedFormat` for documents whose content is embedded HTML or SVG (email, MHT, HTML, EPUB, Markdown, SVG, XPS, DXF) instead of laying out blank pages; `HtmlRenderer` and the WinForms sample fall back to the sanitized HTML path for them.
- `prism_layout::pdf_input`: pure-Rust PDF page interpreter (`hayro-interpret` + `hayro-syntax`) producing positioned `LayoutDocument` pages (`interpret_pdf_bytes`, `interpret_pdf_pages`).
- Bundled SIL OFL-1.1 metric-compatible font suite (`Carlito`, `Caladea`, `Liberation Sans/Serif/Mono`, `Noto Sans/Serif` in `assets/fonts/`), `THIRD_PARTY_NOTICES.md`, data-driven Office/PDF font substitution table, and `.odttf` GUID font deobfuscation (`deobfuscate_odttf`).
- Reading-order text extraction (`LayoutPage::text`), character bounding boxes (`LayoutPage::char_boxes`), and document search (`LayoutDocument::find` with case, diacritic, and whole-word options).
- Rich-text paragraph shaping and line breaking (`TextLayoutEngine`) built on `parley`, `fontique` (hermetic collection), `harfrust`, and `skrifa`.
- `prism_render::svg`: deterministic SVG page renderer (`<defs>` normalized glyph outlines + `<use>` placement) and interactive offline HTML viewer (`prism-viewer.js` / `viewer/src/prism-viewer.ts`) with virtualized pages, zoom, rotation, thumbnails, selectable text layer, and search-hit highlighting.
- `prism_render::raster`: pure-Rust CPU page rasterizer (`tiny-skia`), deterministic PNG encoder (`render_page_rgba`, `render_page_png`), and multi-page TIFF exporter (`export_document_tiff`).
- `prism_render::pdf`: pure-Rust searchable PDF 1.7 exporter (`krilla`, `export_document_pdf`).
- Page-on-demand C ABI (`prism_document_open`, `prism_document_page_count`, `prism_document_page_size`, `prism_document_render_page`, `prism_document_page_text`, `prism_document_search`, `prism_document_export`, `prism_document_font_report`, `prism_buffer_free`, `prism_document_close`), C header `bindings/c/prism.h`, and managed `PrismDocument : IDisposable` wrapper in `Prism.Native`.
- Visual fidelity harness (`crates/prism-tests/tests/fidelity.rs`, `fidelity-baseline.json`, `scripts/make-references.ps1`), performance latency budget tests (`crates/prism-tests/tests/performance_budgets.rs`), and fuzz targets (`layout_doc`, `pdf_interpret`).
- Architecture Decision Records (`docs/adr/0001-text-layout-and-fonts.md`, `0002-page-model-and-output-backends.md`, `0003-pdf-input-interpretation.md`) and architecture guide (`docs/architecture/layout.md`).
- `ContentBlock::Embedded` (HTML, generated HTML, SVG, PDF) with separate extractable text, replacing the `__PDF_DATA__` / `__HTML_RAW__` / `__SVG_RAW__` marker strings.
- `prism_core::text::html_to_text` for indexing markup-based formats.
- HTML sanitizer for untrusted markup (`prism_render::sanitize`, ammonia): scripts, handlers, frames, forms, meta refresh and remote resources are removed, and links are neutralized. Untrusted SVG is displayed via `<img>`.
- Content Security Policy on all rendered HTML (no network, nonce-only scripts).
- Bundled pdf.js 6.4.299 with an offline, lazily rendered continuous-page viewer.
- Position-aware PDF text extraction (text matrix, `TJ` kerning and line changes) and full document-information metadata (lopdf).
- C ABI: `prism_detect_format_ex`, `prism_convert_to_html_ex`, `prism_extract_text`, `prism_extract_metadata`, `prism_last_error`, `prism_version`; .NET helpers throwing `PrismException`.
- Malformed-input robustness test, cargo-fuzz targets, cargo-deny license/advisory gate in CI.
- `docs/ROADMAP.md`.
- `prism_parsers::limits`: bounded decompression (default 64 MiB per stream, or `ParseOptions::max_memory`) for gzip, bzip2, EMZ and OOXML/ODF/EPUB/XPS/VSDX zip entries.

### Security
- lopdf 0.38 → 0.46 (RUSTSEC-2026-0187: stack overflow on deeply nested PDF objects).
- bytes, tar, time, anyhow and crossbeam-epoch updated to patched releases.
- quick-xml 0.31 → 0.42 across all XML parsers (RUSTSEC-2026-0194, RUSTSEC-2026-0195). Entity references are resolved from the new `GeneralRef` events, and attribute values are now unescaped.
- `sevenz-rust` replaced by `sevenz-rust2` (RUSTSEC-2026-0245, RUSTSEC-2026-0246).
- calamine 0.25 → 0.37, bzip2 0.5 → 0.6.
- tokio 1.48 → 1.54 (RUSTSEC `AsyncFd` I/O-safety advisory).
- Decompression bombs are rejected with `Error::MemoryLimitExceeded` instead of exhausting memory.
- Sanitized `<style>` blocks can no longer carry markup: their text is written back with every `<` as the CSS escape `\3c ` (found by the `sanitize_html` fuzz target), and remote `url(...)` and `@import` are stripped as for inline styles.
- Remaining advisories (zip extraction APIs that are not used; unmaintained transitive crates) are listed with reasons in `deny.toml`.

### Fixed
- Text engine: a first-line indent is applied once, by `parley`, so the first line also breaks at the narrower width; it was added twice on the DOCX layout path and could run past the right margin.
- Text engine: paragraphs with more than 65,535 runs no longer panic in `parley` (adjacent spans are merged above 8,192).
- Extracted text of PDF, HTML, email, EPUB, Markdown, SVG, DXF and XPS documents no longer contains base64 or markup.
- DOCX: explicit off values (`<w:b w:val="0"/>`, `<w:u w:val="none"/>`) no longer turn formatting on. Self-closing `w:sz`, `w:rFonts` and theme `w:color` are now applied. Field codes (`w:instrText`) are no longer shown as text. Tracked insertions and deletions are marked (underline / strikethrough). Tabs and line breaks are kept. Page size comes from the section. Image MIME type, pixel size, extent and alt text are read.
- Bindings no longer create a runtime and parser registry per call, no longer panic on interior NUL bytes, and reject null buffers safely.
- EPUB title and author are no longer truncated at the first entity reference (e.g. `AT&amp;T`).
- gzip/bzip2: the compression ratio of an empty stream is reported as 0% instead of infinity.
- WinForms sample no longer limited to 2 MB documents (virtual host instead of `NavigateToString`); navigation away from the document is blocked.

### Changed
- Removed the ImageMagick shell-out: EMF, WMF and EPS now show format metadata only, until native vector rendering lands in Phase 1.
- `Prism.Native` NuGet package license corrected from MIT to `AGPL-3.0-only`, matching the native library it ships.

## [0.5.0] - 2026-01-17
### Changed
- Upgraded codebase to Rust Edition 2024 (requires Rust 1.85+).
- Updated `prism-bindings` to use `#[unsafe(no_mangle)]` per new edition safety requirements.
- Consolidated versioning across all crates (`prism-tests`, `prism-bindings`) to inherit from workspace root.
- Fixed clippy issues in `prism-parsers`.

## [0.1.0] - 2026-01-17
### Added
- Initial release of Prism.
- Core document model and trait definitions.
- Parser implementations for Archive, Office, and Email formats.
- Rust bindings crate `prism-bindings`.
- NuGet package `Prism.Native` including native runtimes for Windows (x64).
