# ADR 0008: Sparse Workbook Model (`prism-excel`) and Deterministic Number Format Engine (`prism-numfmt`)

## Status

Accepted (Phase 3, Milestone M0 / M3)

## Context

Excel workbooks (`XLSX` and legacy `XLS`) can declare formatting over the entire `1,048,576 × 16,384` worksheet grid while containing only a small number of populated cells, or contain hundreds of thousands of numeric cells whose displayed representation depends on ECMA-376 number format codes (`numFmtId` 0–49 plus custom format strings).

For eDiscovery and forensic review:
- Formulas must **never** be recalculated; only the cached result stored at save time may be formatted and displayed, while the formula string is preserved for inspection.
- Number and date formatting must be 100% deterministic across host operating systems and locales, reproducing Excel's 1900 leap-year bug (`1900-02-29` at serial `60`) and 1904 date system.
- Memory consumption must remain bounded even on sheets with full-column or full-row styles.

## Decision

1. **Create `crates/prism-numfmt`**:
   - Standalone, pure-Rust ECMA-376 / Excel number-format parser and formatter.
   - Supports built-in format IDs `0..=49`, up to 4 sections (`positive;negative;zero;text`), conditional brackets (`[>=100]`), color tokens (`[Red]`, `[Color10]`), currency/locale prefixes (`[$€-407]`), thousands grouping and scaling commas, percentages, scientific notation, continued-fraction rational approximation (`# ?/?`), 1900/1904 serial dates and elapsed times (`[h]:mm:ss`), `*` fill, `_` alignment spaces, and `;;;` concealment detection.
   - Defaults to a fixed deterministic `en-US` locale unless overridden in options.
2. **Sparse Cell and Span Storage in `crates/prism-excel`**:
   - Cells are stored in a sparse `BTreeMap<(u32, u32), Cell>` keyed by `(row, col)` (0-based), bounded by `WorkbookLimits::max_cells_per_sheet`.
   - Column widths/styles/hidden flags are stored as compressed `ColumnSpan { min_col, max_col, width_pt, hidden, style_id, outline_level }` intervals rather than per-column arrays.
   - Row heights/styles/hidden flags are stored sparsely in `BTreeMap<u32, RowProps>`.

## Consequences

- Sheets with `1,048,576` formatted rows open in milliseconds with minimal memory allocation.
- Cell display strings match Excel's `Range.Text` deterministically without invoking Excel or host OS locale APIs.
