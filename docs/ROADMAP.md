# Prism Roadmap

Goal: an open-source, embeddable native-document viewer for forensic and
eDiscovery review, comparable to Oracle Outside In, with Word, PowerPoint and
Excel shown as they look in Office, all built on a **native Rust rendering
engine**.

## Guiding constraints

- **Pure Rust core, permissive dependencies.** Prism is dual-licensed (AGPL-3.0 +
  commercial), so every dependency must be permissive or weak-copyleft.
  `deny.toml` enforces this in CI. No OnlyOffice, MuPDF, Ghostscript or other
  AGPL/GPL code. LibreOffice and Microsoft Office may be used *offline as
  reference oracles* for fidelity testing, never shipped.
- **Forensically sound viewing.** Never execute document content and never make
  network requests. Show stored values (no field updates or formula
  recalculation). Surface hidden content instead of silently dropping it. Same
  input + same engine version = same output.
- **The host must survive the document.** Malformed input may produce an error,
  never a crash or hang in the host application.

## Phase 0 – Foundations (complete)

- [x] Typed `ContentBlock::Embedded` replaces the `__PDF_DATA__` /
      `__HTML_RAW__` / `__SVG_RAW__` marker strings, so extracted text and
      search indexes no longer contain markup or base64.
- [x] Untrusted HTML (email, web, MHT, EPUB, Markdown) is sanitized (ammonia):
      scripts, handlers, frames, forms and remote resources removed; links
      neutralized. Untrusted SVG is displayed through `<img>`.
- [x] Rendered HTML carries a strict Content Security Policy (no network, nonce
      scripts only).
- [x] pdf.js is bundled (no CDN), so the viewer works air-gapped. Position-aware
      PDF text extraction via lopdf.
- [x] DOCX fixes: toggle properties (`<w:b w:val="0"/>` etc.), image MIME types
      and extents, section page size.
- [x] C ABI: shared runtime/registry, filename hints, text/metadata
      extraction, last-error reporting. WinForms sample no longer limited to 2 MB.
- [x] CI: cargo-deny license and advisory gate, malformed-input robustness test,
      cargo-fuzz targets.
- [x] README claims aligned with the current state.
- [x] Security updates: lopdf 0.46 (stack overflow on nested PDFs), bytes, tar,
      time, anyhow, crossbeam-epoch.
- [x] **quick-xml 0.31 → 0.42.** Migrated all Office, EPUB and XPS XML parsers
      to quick-xml 0.42, resolving RUSTSEC-2026-0194 and RUSTSEC-2026-0195.
- [x] Replaced unmaintained `sevenz-rust` with `sevenz-rust2` (resolving
      RUSTSEC-2026-0245 and RUSTSEC-2026-0246).
- [x] Decompression limits for gzip/bzip2/zip entries (`limits::read_bounded`,
      honoring `ParseOptions::max_memory` and defaulting to 64 MiB).
- [x] Removed the ImageMagick shell-out (`image/converter.rs`); pure-Rust
      decoders only.

## Phase 1 – Native layout core (`prism-layout`) (complete)

A single **positioned page model** that every paged format lays out into, and
every output is produced from:

```
Parsers ──► Flow/fixed model ──► prism-layout ──► LayoutPage[] ──┬─► SVG/HTML pages + text layer (viewer)
                                                                  ├─► PNG/TIFF (productions)
                                                                  ├─► PDF export
                                                                  └─► text with character boxes (search hits, redaction)
```

- [x] Page model: glyph runs (font, size, glyph ids, positions, source text and
      cluster mapping), vector paths, images, links, annotation anchors.
- [x] Fonts: discovery and fallback (`fontique`), shaping (`harfrust`),
      parsing (`skrifa`). Bundle metric-compatible open fonts
      (Carlito, Caladea, Liberation, Noto) and use fonts embedded in the document
      (including DOCX `.odttf` deobfuscation and PDF embedded fonts).
- [x] Paragraph layout: `parley` (line breaking, bidi, rich text) plus flow
      pagination for paragraphs, lists, images and tables.
- [x] Rasterization: `tiny-skia` (deterministic PNG and multi-page TIFF). PDF writing: `krilla`.
- [x] PDF input: `hayro` (pure-Rust PDF interpreter) converts PDF pages directly
      into `LayoutDocument`, so PDFs share the same page model, text extraction,
      character boxes and search code.
- [x] Viewer component: offline TypeScript/JS viewer (`prism-viewer.js`) with
      virtualized pages, selectable text layer, search-hit highlighting,
      thumbnails, zoom/rotate. Page-on-demand C ABI (`bindings/c/prism.h`,
      `Prism.Native`):
      `open → page_count → render_page(n, dpi) → page_text(n) → search → export`.

## Phase 2 – Word (DOCX, then DOC), 4–8 months

- [x] **M0 – Complete Word model & DOCX reader (`prism-word`).** Style inheritance
      (docDefaults → style chain → direct with `ToggleProp` XOR semantics),
      numbering/lists (`abstractNum`, `num`, `lvlOverride`), sections and columns,
      headers/footers, footnotes/endnotes, tables (`tblGrid`, `gridSpan`, `vMerge`,
      borders, shading, conditional table styles), custom tab stops, fields
      (cached results, external reference capture), text boxes, anchored images
      and shapes, tracked changes (`w:ins`, `w:del`, `w:moveFrom`, `w:moveTo`),
      comments, and forensic indicators (`ForensicFacts`).
- [x] **Flow layout.** Block stacking, page breaking (keep-with-next, widow/orphan
      control, table row splitting), header/footer placement, footnote areas.
- [ ] **Floats and wrapping.** Square, top-and-bottom and behind/in front of text
      done; tight and through wrap like square (wrap polygons are parsed but not
      used yet).
- [ ] **Word compatibility quirks.** Contextual spacing done; `compatibilityMode`
      and `w:compat` flags are parsed but do not change layout yet.
- [ ] **Legacy DOC** ([MS-DOC] binary format) parsed into the same model, so the
      layout engine is reused. Done: FIB, piece table, CHPX/PAPX direct formatting,
      fields, single-level tables, encryption detection. Footnote, header,
      annotation, endnote and text box text is kept as unplaced stories (extracted,
      not drawn). Pending: stylesheet, fonts, sections, story placement, lists,
      pictures and shapes; Word 6/95 still uses the text fallback.
- [ ] **Fidelity and performance.** Word reference fixtures, fidelity scores in
      `fidelity-baseline.json`, and Word latency budgets are not in place yet; the
      page-on-demand C ABI already opens DOCX/DOC through `prism-word`.

## Phase 3 – PowerPoint and Excel, 3–6 months (in progress)

- [ ] **Shared DrawingML (`prism-drawingml`).** Done: theme colour scheme and
      colour transforms, fills and lines, custom geometry, the guide formula
      evaluator, and a path for each of the 187 preset names. Pending: presets are
      hand-written approximations (several draw as plain or rounded rectangles),
      not generated from `presetShapeDefinitions.xml`, and have no golden tests.
- [ ] **PowerPoint (`prism-ppt`).** Done: `Presentation` model; PPTX reader with
      master → layout → slide placeholder inheritance, stored `normAutofit` scale,
      tables, SmartArt from the drawing part, charts, speaker notes, comments and
      hidden slides; legacy PPT text and basic shapes. Pending: PPT backgrounds,
      text colours and connectors; notes-page output; off-slide content in review
      mode; fidelity scores.
- [ ] **Excel (`prism-excel`, `prism-numfmt`).** Done: sparse `Workbook` model;
      XLSX and BIFF8 XLS readers (cells, styles, number formats, hidden sheets,
      rows and columns, merges, comments); stored-value conditional formatting;
      print layout over the whole used range (blank pages skipped, `max_pages`
      enforced); workbook C ABI (`prism_workbook_*`) and `PrismWorkbook`. Pending:
      lazy sheet loading and limits on cells, shared strings and styles; XLS
      formula text (stored as `BIFF8_FORMULA`), drawings and charts; Excel's
      default "VelvetSweatshop" RC4 encryption (reported as encrypted); the canvas
      grid viewer (only sheet tabs and a formula bar showing the first cell exist);
      the Excel-generated number-format conformance fixture.
- [ ] **Charts (`prism_drawingml::chart`).** Done: chart parser using cached
      values only (sparse caches keep point positions), bar/column, line, area,
      pie/doughnut and chartex basics, automatic axis scaling. Pending: scatter and
      bubble X values (points are placed by index), log axes, radar geometry,
      secondary axes, axis-scaling fixtures against Excel.
- [ ] **Fidelity and performance.** PowerPoint and Excel reference fixtures,
      SSIM and displayed-text scores, and the workbook latency and memory budgets
      are not in place yet; the page-on-demand C ABI already opens PPTX/PPT and
      XLSX/XLS through the new engines.

## Phase 4 – Review and production features, 3–6 months

- Hidden-content report across formats: tracked changes, comments, hidden text,
  white or tiny text, speaker notes, hidden slides/sheets/rows, embedded
  objects, macros present, external links.
- Annotation and redaction API with stable page coordinates; burned-in
  redactions on export.
- Image/PDF productions with Bates numbering and endorsements.
- Process-isolated parser worker pool (memory, CPU and time limits) so a
  hostile document cannot take down the host. WASM sandboxing re-evaluated
  after that.
- Caching by content hash; deterministic, versioned output.

## Measuring fidelity

- Reference renders: the sample corpus is printed to PDF once with Microsoft
  Word, PowerPoint and Excel (COM automation on Windows) and rasterized.
- Each CI run renders the corpus with Prism and compares per page (SSIM).
  Target ≥ 0.95 for common business documents, tracked per format.
- Also tracked: robustness failures (must be zero), p95 time to first page,
  and a zero-network-request test of the viewer output.
