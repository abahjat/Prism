# ADR 0009: Forensic Conditional Formatting and Cached-Data Chart Rendering

## Status

Accepted (Phase 3, Milestone M0 / M4 / M6)

## Context

Spreadsheets and presentations frequently contain conditional formatting rules (`cfRule`) and charts (`c:chartSpace` / `cx:chartSpace`). In desktop Excel, expression-based conditional formatting rules (`type="expression"`) evaluate arbitrary formulas (including volatile functions like `NOW()`, `TODAY()`, `INDIRECT()`), and charts re-query worksheet cell ranges or external workbook links.

In a forensic viewer:
- Evaluating formulas or volatile functions could alter the visual appearance of evidence based on the time of viewing.
- Following external workbook references would violate the zero-network/zero-external-file security boundary.

## Decision

1. **Conditional Formatting Policy**:
   - Rules that depend only on **stored cell values** (`cellIs`, `containsText`, `notContainsText`, `beginsWith`, `endsWith`, `containsBlanks`, `notContainsBlanks`, `containsErrors`, `top10`, `aboveAverage`, `duplicateValues`, `uniqueValues`, `colorScale`, `dataBar`, `iconSet`, and `timePeriod` evaluated against a fixed reference date from document metadata or options) are evaluated deterministically.
   - Formula expression rules (`type="expression"`) are **never evaluated**; instead, they are recorded in `WorkbookForensicFacts::unevaluated_cf_formulas` and surfaced to the reviewer as metadata.
2. **Chart Data and Axis Scaling Policy**:
   - Charts render exclusively from cached series data (`c:numCache`, `c:strCache`, `cx:data`) embedded in the chart XML or BIFF8 chart records. If a series has no cached values, the chart renders a clear `"No cached chart data"` indicator rather than reading worksheet cells.
   - When an axis omits explicit `<c:min>`, `<c:max>`, or `<c:majorUnit>`, the chart renderer applies Excel's deterministic nice-number auto-scaling algorithm (`1, 2, 5 × 10^k` major steps, zero baseline for same-sign bar/column/area series).

## Consequences

- Conditional formatting and charts are 100% deterministic, offline, and forensically sound.
- Reviewers are explicitly informed when a worksheet contains formula-driven conditional formatting rules or uncached formulas.
