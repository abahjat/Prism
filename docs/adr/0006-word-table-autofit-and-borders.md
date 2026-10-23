# ADR 0006: Word Table Grid, Autofit Algorithm, and Border Conflict Resolution

- **Status**: Accepted
- **Date**: 2026-10-23
- **Phase**: Phase 2 (Milestone M0 / M2)

## Context

Word tables (`w:tbl`) rely on a shared column grid (`w:tblGrid/w:gridCol`), optional cell preferred widths (`w:tcW`), table preferred width (`w:tblW`), horizontal merges (`w:gridSpan`), vertical merges (`w:vMerge`), and per-edge border conflict resolution (ECMA-376 §17.4.26).

## Decision

### 1. Table Grid & Autofit Column Sizing
Each `WordTable` records `tbl_grid: Vec<f32>` (grid column widths in points) and `layout_mode: TableLayoutMode` (`Fixed` vs `AutoFit`):
1. **Fixed Layout (`<w:tblLayout w:type="fixed"/>`)**:
   - Column widths come directly from `w:tblGrid/w:gridCol` (or cell `w:tcW` on the first row when `tblGrid` is omitted).
   - If total width exceeds available column width and `tblW` is `pct` or `auto`, columns scale proportionally.
2. **AutoFit Layout (default in Word)**:
   - For each grid column $c$, compute:
     - $W_{\min}[c]$: longest unbreakable word / inline object width plus horizontal cell padding (`left + right` cell margins).
     - $W_{\max}[c]$: single-line unwrapped width of the longest paragraph in column $c$ plus cell padding (honoring explicit `w:tcW` when specified).
   - Multi-column spanned cells (`gridSpan > 1`) distribute excess minimum/maximum width across their spanned columns proportionally.
   - Given target table width $W_{\text{target}}$ (from `w:tblW` or available body/column width):
     - If $\sum W_{\max} \le W_{\text{target}}$ and `w:tblW` is `auto`, each column gets $W_{\max}[c]$.
     - If $\sum W_{\min} < W_{\text{target}} < \sum W_{\max}$, each column receives:
       $$W[c] = W_{\min}[c] + (W_{\text{target}} - \sum W_{\min}) \cdot \frac{W_{\max}[c] - W_{\min}[c]}{\sum (W_{\max} - W_{\min})}$$
     - If $\sum W_{\min} \ge W_{\text{target}}$, columns receive $W_{\min}[c]$ scaled down to fit $W_{\text{target}}$ (with a 4 pt minimum floor so layout always terminates).

### 2. Border Conflict Resolution (ECMA-376 §17.4.26)
Where two adjacent cells (or a cell and the table perimeter) share an edge, the winning `BorderSpec` is chosen deterministically:
1. Any border with `BorderStyle::None` or `nil` loses to a visible border (unless explicitly overriding a table-level inside border with `nil`).
2. Larger border weight (effective stroke width in points) wins.
3. On equal width, style priority wins: `Double` > `Thick` > `Single` > `Dashed` > `Dotted`.
4. On equal style, darker color (lower luminance) wins; on tie, the top/left cell wins.

### 3. Row Splitting & Repeating Header Rows
- Rows with `w:tblHeader` repeat at the top of each new page/column when a table splits across pages.
- Rows with `w:cantSplit` are moved intact to the next page unless the row alone is taller than a full page (in which case it is force-split so pagination always makes forward progress).

## Consequences

- **Positive**: Deterministic, single-pass-compatible table column sizing that closely matches Word's Print Layout table rendering.
- **Positive**: Guaranteed forward progress on pathological tables (rows taller than the page or deeply nested tables up to `MAX_TABLE_NESTING_DEPTH = 8`).
