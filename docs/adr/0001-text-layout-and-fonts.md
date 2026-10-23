# ADR 0001: Text Layout and Font Stack (`parley`, `fontique`, `harfrust`, `skrifa`)

- **Status:** Accepted
- **Date:** 2026-10-23
- **Milestone:** Phase 1 — M0 (Spikes and ADRs)

## Context

Phase 1 introduces `prism-layout`, a native Rust layout engine that converts Prism's Unified Document Model (UDM) into positioned `LayoutPage`s. Text layout and font handling must satisfy five constraints:

1. **Pure Rust and permissive licensing:** Only `MIT`, `Apache-2.0`, `BSD-*`, `ISC`, `Zlib`, or `MPL-2.0` crates are allowed (`cargo deny check` must pass with zero unmaintained or copyleft violations, and no C/C++ `-sys` bindings such as HarfBuzz C, FreeType, or `libfontconfig`).
2. **Forensic determinism:** Identical document input and engine version must produce byte-identical SVG, PNG, TIFF, and PDF output across machines. System fonts must **not** be queried by default; only document-embedded fonts and Prism's bundled metric-compatible fonts may be used unless the caller explicitly opts into system fonts via `LayoutOptions`.
3. **Font substitution reporting:** Every font resolution must record the requested family/style and the actual resolved family/source in a per-document `FontReport`.
4. **Rich paragraph and run layout:** Support left/center/right/justify alignment, multiple/exact/at-least line spacing, first-line/left/right/hanging indents, space before/after, default tab stops, mixed runs (font, size, bold, italic, color, underline, strikethrough, highlight, superscript, subscript), fixed text boxes (PPTX), and multi-page flow pagination (DOCX, TXT).
5. **Single source of truth for text and geometry:** `GlyphRun`s must carry the source text, grapheme `Cluster`s (mapping byte ranges to glyph ranges), and positioned glyphs so that text extraction (`LayoutPage::text()`), selection/hit-testing (`LayoutPage::char_boxes()`), and search (`LayoutDocument::find()`) all derive from the same laid-out runs.

## Candidates Evaluated

### 1. Text Layout: `parley 0.12.0` vs Custom (`unicode-linebreak` + `unicode-bidi` + Shaping)

| Criterion | `parley 0.12.0` | Custom (`unicode-linebreak 0.1.5` + `unicode-bidi 0.3.18` + `harfrust`) |
| :--- | :--- | :--- |
| License | `Apache-2.0 OR MIT` | `Apache-2.0` / `MIT OR Apache-2.0` / `MIT` |
| Unsafe code | `#![deny(unsafe_code)]` (`parley` & `parley_engine`) | `#![forbid(unsafe_code)]` |
| Bidi & script itemization | Built-in (`parley_engine` + ICU4X segmenter/normalizer) | Manual bidi level resolution, script run splitting, and visual reordering |
| Line breaking & pagination | Built-in `Layout::break_all_lines` and incremental `BreakLines::break_next(max_advance)` | Manual UAX #14 line breaker, run splitting, and line-box height accumulation |
| Paragraph alignment & indents | `Alignment::{Start, End, Left, Center, Right, Justify}` + `Layout::set_text_indent(amount, IndentOptions { each_line, hanging })` | Manual inter-word justification gap distribution and indent width adjustment |
| Line height models | `LineHeight::{MetricsRelative, FontSizeRelative, Absolute}` | Manual ascent/descent/leading calculation |
| Superscript / subscript | Built-in `VerticalAlign::SUPER` and `VerticalAlign::SUB` (`BaselineShift`) | Manual baseline offset and font size scaling |
| Inline boxes / tabs | `InlineBox { id, kind: InlineBoxKind::InFlow, index, width, height, .. }` | Custom inline atom representation |
| Cluster & glyph mapping | `Line::runs()` → `Run::visual_clusters()` yielding `Cluster` with `text_range()`, `advance()`, `glyphs()`, `is_ligature_continuation()` | Custom cluster coalescing and UTF-8 byte-to-glyph index tracking |

### 2. OpenType Shaping: `harfrust` vs `rustybuzz`

| Criterion | `harfrust 0.12.0` / `0.15.0` | `rustybuzz 0.20.1` |
| :--- | :--- | :--- |
| License | `MIT` | `MIT` |
| Maintenance & `cargo-deny` | Actively maintained by the HarfBuzz team (`github.com/harfbuzz/harfrust`); **passes `cargo deny check`** | **Fails `cargo deny check`** with `RUSTSEC-2026-0206` (`rustybuzz` is unmaintained) and pulls in `ttf-parser 0.25.1` which fails with `RUSTSEC-2026-0192` (`ttf-parser` is unmaintained) |
| Font parsing backend | `read-fonts` / `skrifa` (`fontations`) | `ttf-parser` (unmaintained) |
| Integration | Used natively by `parley_engine 0.12.0` | Requires standalone buffer conversion |

### 3. Font Discovery & Fallback: `fontique 0.12.0` vs `fontdb 0.24.0`

| Criterion | `fontique 0.12.0` (`default-features = false, features = ["std"]`) | `fontdb 0.24.0` (`default-features = false, features = ["std"]`) |
| :--- | :--- | :--- |
| License | `Apache-2.0 OR MIT` | `MIT` |
| Pure Rust check | With `default-features = false, features = ["std"]`, `yeslogic-fontconfig-sys` and OS FFI crates (`windows`, `objc2`) are **not** compiled | Pure Rust when `fontconfig` feature is disabled, but uses internal `ttf-parser`-style parsing |
| Isolated deterministic mode | `Collection::new(CollectionOptions { shared: false, system_fonts: false })` creates a hermetic font collection containing only registered blobs | `Database::new()` starts empty, but requires manual bridging to `parley::FontContext` |
| Script & attribute fallback | Built-in CSS font-matching algorithm + script-based fallback family registration | CSS query matching only (no script fallback tables) |

### 4. Font Metrics & Outlines: `skrifa 0.44+`

- `skrifa` (`MIT OR Apache-2.0`, Google Fonts `fontations` workspace) is already in the dependency tree of `parley_engine`, `hayro-interpret`, and `krilla`.
- Exposes `FontRef::from_index(bytes, index)`, `MetadataProvider::outline_glyphs()`, and `OutlineGlyph::draw(DrawSettings::unhinted(Size::new(font_size), LocationRef::default()), &mut pen)` to produce deterministic, unhinted Bézier glyph outlines for SVG `<defs>` and rasterization.

## M0 Spike Results

In the M0 evaluation spike:
1. Configured `parley = { version = "0.12.0", default-features = false, features = ["std"] }` and `fontique = { version = "0.12.0", default-features = false, features = ["std"] }`.
2. Verified with `cargo deny check advisories licenses bans sources` that the entire dependency graph is 100% pure Rust (zero `-sys` crates) and passes all license and advisory checks (`advisories ok, bans ok, licenses ok, sources ok`).
3. Registered a TTF font blob into an isolated `fontique::Collection` (`CollectionOptions { shared: false, system_fonts: false }`), built a rich paragraph in `parley` with bold, underline, subscript (`VerticalAlign::SUB`), superscript (`VerticalAlign::SUPER`), `LineHeight::FontSizeRelative(1.2)`, `set_text_indent(18.0, IndentOptions::default())`, and `Alignment::Justify` wrapped to `300.0 pt`, and extracted 246 positioned glyphs and 4,129 Bézier outline segments via `skrifa`.

## Decision

1. **Adopt `parley 0.12` (`default-features = false, features = ["std"]`)** for rich-text paragraph and text-box layout, using custom `Brush` metadata to carry run styling (color, highlight, decorations, and `SourceRef`) and `InlineBox` to implement tab stops.
2. **Adopt `fontique 0.12` (`default-features = false, features = ["std"]`)** with `CollectionOptions { shared: false, system_fonts: false }` as the font registry inside `prism-layout`:
   - Never enable the `system` cargo feature on `parley` or `fontique`, keeping `yeslogic-fontconfig-sys` out of the build on all platforms.
   - When `LayoutOptions::allow_system_fonts` is `true` (explicit opt-in), scan standard OS font directories in pure Rust and register discovered font files into the `fontique::Collection`.
   - Wrap font resolution in a `FontResolver` layer that applies Prism's deterministic Office font substitution table (e.g., `Calibri` → `Carlito`, `Cambria` → `Caladea`, `Arial`/`Helvetica` → `Liberation Sans`, `Times New Roman` → `Liberation Serif`, `Courier New` → `Liberation Mono`, fallback → `Noto Sans` / `Noto Serif`) before querying `fontique`, recording every mapping in `FontReport`.
3. **Adopt `harfrust`** (via `parley_engine` and directly where needed) and **reject `rustybuzz` / `ttf-parser`** due to `RUSTSEC-2026-0206` and `RUSTSEC-2026-0192`.
4. **Adopt `skrifa`** for font table inspection, metric queries, and unhinted glyph outline extraction.

## Consequences

- **Positive:** Eliminates ~2,000+ lines of custom bidi, UAX #14 line-breaking, and OpenType shaping glue while maintaining 100% pure-Rust compilation and hermetic font resolution by default.
- **Negative / Trade-offs:** `parley 0.12` pulls in ICU4X compiled data crates (`icu_segmenter_data`, `icu_normalizer_data`, `icu_properties_data`), adding ~1.2 MB to binary size in exchange for standards-compliant Unicode line breaking and script segmentation.
- **Line-spacing nuance:** Word's "at-least" (`w:lineRule="atLeast"`) vs "exact" (`w:lineRule="exact"`) vs "multiple" (`w:lineRule="auto"`) line spacing maps cleanly onto `parley`:
  - `Multiple(m)` → `LineHeight::MetricsRelative(m)`
  - `AtLeast(pt)` → `LineHeight::MetricsRelative(1.0)` during `parley` layout, with `prism-layout`'s line-stacking pass enforcing `line_step = line.metrics().line_height.max(pt)`
  - `Exact(pt)` → `LineHeight::Absolute(pt)` with fixed baseline step `pt`.
