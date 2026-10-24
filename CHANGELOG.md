# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
### Added
- Phase 4 review and production features:
  - Hidden-content report (`prism_core::review`, `prism_parsers::review::generate_hidden_content_report`): findings with severity, location, source reference and bounded excerpt, plus a coverage list of what each provider inspected. Providers for DOCX/DOC, PPTX/PPT, XLSX/XLS, PDF, EML/MBOX/MSG and ZIP/TAR/7z; other formats report zero-width characters only. Viewer findings panel (`#prism-review-panel`).
  - Container family tree (`prism_core::tree`, `prism_parsers::tree::build_family_tree`, `open_child_bytes`) over archives, email attachments, OLE object pools, OOXML embeddings and PDF embedded files, with depth, byte and member budgets and SHA-256 cycle detection.
  - `prism-review`: annotation sidecar (`AnnotationSidecar`, schema in `docs/schemas/annotation-sidecar.schema.json`) with text anchors re-resolved across engine versions, proposed redactions from a search or regular expression (`propose_pattern_redactions`), destructive redaction of glyphs, image pixels, spreadsheet cells, formulas, comments and metadata (`apply_redactions`, `apply_workbook_redactions`, `scrub_metadata`, `export_redacted_document`), output verification (`verify_redacted_output`), and a content-addressed `FilesystemCache`.
  - `prism-production` (`run_production`): Bates numbering with family ranges, six-position endorsements, slip sheets, single- and multi-page TIFF, PNG and PDF output (PDF/A-2b through krilla's validator), Concordance `.dat`, Opticon `.opt` and CSV load files with reference readers, and a `manifest.json` rewritten after every document so interrupted runs resume.
  - `prism-sandbox` and `prism-worker`: worker-process pool over a stdin/stdout protocol (`PRWK` v1) with a per-job heap limit, wall-clock timeout, Windows Job Objects, and Linux `no_new_privs`, disabled core dumps and an address-space backstop; `InProcessExecutor` for hosts that cannot spawn processes.
  - `prism-cli` (`detect`, `info`, `text`, `render`, `export`, `report`, `tree`, `produce`, `verify-redactions`) and `prism-server` endpoints (`/api/render`, `/api/text`, `/api/report`, `/api/tree`, `/api/export`) that run documents through the worker pool.
  - C ABI `prism_document_hidden_content`, `prism_document_children`, `prism_document_open_child`, `prism_document_load_annotations`, `prism_document_save_annotations`, `prism_document_add_annotation`, `prism_document_remove_annotation`, `prism_document_annotations` and `prism_document_export_redacted`, with `Prism.Native` bindings.
  - ADRs `0010`–`0013`, `docs/architecture/{review,production,sandbox}.md`, `docs/deployment.md`, fuzz targets (`sidecar_parse`, `redaction_apply`, `loadfile_parse`, `container_tree`), and text extraction from PDF Form XObjects.
- `prism-drawingml`: shared ECMA-376 `DrawingML` crate across Word, PowerPoint, and Excel:
  - 12-slot `DrawingTheme` color scheme (`dk1`, `lt1`, `dk2`, `lt2`, `accent1..6`, `hlink`, `folHlink`), font scheme (`majorFont`, `minorFont`), and exact RGB/HSL color transform pipeline (`tint`, `shade`, `lumMod`, `lumOff`, `satMod`, `satOff`, `alpha`).
  - Preset geometry for the 187 `ST_ShapeType` names (`build_preset_geometry_paths`; hand-written approximations, not yet generated from `presetShapeDefinitions.xml`), custom geometry (`custGeom`), and the 17-operator guide formula evaluator.
  - Native `c:chartSpace` and Office 2016+ `cx:chartSpace` (`chartex`) chart parser and vector renderer (`parse_chart_xml`, `render_chart`) drawing bar/column (clustered, stacked, 100% stacked), line, area, pie/doughnut, combo, waterfall, treemap and histogram charts strictly from cached series values (`c:numCache`, `c:strCache`, `cx:data`); scatter, bubble and radar charts are approximations.
- `prism-numfmt`: deterministic, locale-independent ECMA-376 number and date/time formatting engine (`format_cell_value`, `builtin_format_code`, `is_date_format`, `is_concealed_format`) supporting built-in IDs `0..=49`, multi-section format strings, conditional brackets, color tokens, grouping/scaling commas, percentages, scientific notation, continued-fraction rational approximations, `1900` (with Lotus 1-2-3 leap-year bug serial `60` = `1900-02-29`) and `1904` date systems, elapsed time (`[h]:mm:ss`), and `;;;` forensic concealment detection.
- `prism-ppt`: native PowerPoint presentation model (`Presentation`), `.pptx`/`.ppsx`/`.potx` reader (`parse_pptx`) with 3-level `SlideMaster` → `SlideLayout` → `Slide` inheritance, text autofit (`normAutofit` / `spAutoFit`), tables, SmartArt (`diagrams/drawing*.xml` + `data*.xml` fallback), charts, speaker notes, comments, hidden slides (`show="0"`), legacy binary `.ppt` (`[MS-PPT]` + `[MS-ODRAW]`) reader (`parse_ppt`), fixed-coordinate slide layout engine (`layout_presentation`), and `PresentationForensicFacts`.
- `prism-excel`: sparse Excel workbook model (`Workbook`, `BTreeMap<(u32, u32), Cell>`, compressed `ColumnSpan`s) scaling to `1,048,576 × 16,384` (`XFD1048576`) without dense allocation, `.xlsx`/`.xlsm` reader (`parse_xlsx`), legacy BIFF8 `.xls` (`[MS-XLS]`) reader (`parse_xls`), cached-value formula preservation (never recalculating formulas), stored-value conditional formatting evaluator (`evaluate_sheet_conditionals` for `cellIs`, `containsText`, `top10`, `aboveAverage`, `duplicateValues`, `colorScale`, `dataBar`, `iconSet`), paginated print layout (`layout_workbook`), standalone drawing renderer (`render_worksheet_drawing_to_doc`), and `WorkbookForensicFacts`.
- Workbook Grid-Mode C ABI (`prism_workbook_open`, `prism_workbook_sheets`, `prism_workbook_sheet_layout`, `prism_workbook_cells`, `prism_workbook_styles`, `prism_workbook_drawings`, `prism_workbook_render_object`, `prism_workbook_search`, `prism_workbook_close`) in `prism-bindings`, `bindings/c/prism.h`, and `PrismWorkbook : IDisposable` in `Prism.Native`, plus viewer formula inspector bar (`#prism-formula-bar`), sheet tabs (`#prism-sheet-tabs`), and hidden/concealed forensic toggles.
- Phase 3 ADRs (`0007`–`0009`), architecture docs (`docs/architecture/{powerpoint,excel,charts}.md`, `docs/numfmt-compat.md`), fuzz targets (`ppt_parse`, `workbook_parse`, `numfmt`, `chart_render`, run in CI), and `phase3_ppt_excel_tests.rs`.
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
- Number formats: a date or time format applied to a serial Excel cannot show (negative, or after 9999-12-31) panicked on very large values (`numfmt` fuzz target); it now shows `########` as Excel does.
- Redaction: a partly covered image that is flipped or rotated (PDF images usually are) had the wrong pixels filled, leaving the covered pixels in the output under the box.
- Redaction: rectangle-only redactions recorded the removed text one character at a time, so verification checked nothing; the removed text is now recorded as whole stretches.
- Redaction: cell-range redactions in `export_redacted_document` (used by `/api/export`, `prism export` and `prism_document_export_redacted`) were logged as applied but left the cell values in the output. The workbook is now redacted before layout, and cell-range redactions on other documents are refused.
- Redaction: verification also searches UTF-16BE (PDF text strings), and its error messages no longer repeat the redacted text.
- Productions: the manifest was written only at the end, so an interrupted run could not resume; a document is reused only when it still starts at the same Bates number; natives of documents with approved redactions are no longer produced through native-only slip sheets; a workbook whose cell redactions cannot be applied stops the production instead of falling back to the unredacted text.
- Family tree: every member of an archive was decompressed before any limit was checked, so one archive could exhaust memory; members are now charged against the budgets as they are read.
- PDF text extraction: nested Form XObjects could invoke each other exponentially (16 levels deep); invocations are capped per page and a form that invokes itself is skipped.
- Server and CLI cache entries are keyed by filename as well as content, since the filename steers format detection.
- `prism-worker` lookup no longer runs `cargo build` from a release build; the worker's Linux address-space limit no longer equals the heap limit.
- Excel print layout covers the whole used range (it stopped at 5,000 rows × 256 columns without notice), skips pages with nothing on them as Excel does, and reports `max_pages` as an error. A whole-sheet print area or merge (`A1:XFD1048576`) no longer hangs.
- Workbook text extraction (`workbook_to_udm`) keeps every row and column (it stopped at 2,000 × 128), includes the stored value of `;;;` cells and cell comments, and no longer scans the sheet once per row.
- XLSX: entity and character references (`&amp;`, `&lt;`, `&#10;`) in strings, formulas, comments and names were dropped; numbers in the Text (`@`) format showed a literal `@`.
- XLS: cell comments (`NOTE` / `TXO` records) are read.
- Number formats: weekdays were one day late; `1.005` with `0.00` showed `1.00` (Excel rounds on 15 significant digits); `0.##` dropped the decimal point; times are rounded to the displayed seconds; non-ASCII literals in date formats were garbled.
- Charts: a `NaN` or infinite axis bound panicked; sparse caches (blank cells) shifted later points onto the wrong category.
- PowerPoint: XML nested deeper than 128 levels could re-parent later elements; more than `max_pages` slides is an error instead of a silently shortened deck.
- `prism_workbook_cells` no longer panics on a reversed rectangle and evaluates conditional formatting once per sheet; `prism_document_set_markup_mode` no longer turns off Word hidden text.
- Viewer: the hidden-sheets toggle is on by default and re-lays out the pages; sheet tabs no longer jump to the page with the sheet's index.
- Non-ASCII hex colour values no longer panic in `prism-drawingml`.
- Word 2.0 inputs shorter than 32 bytes no longer panic (`word_doc` fuzz target).
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
