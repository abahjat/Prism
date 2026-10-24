# ADR 0011: Stable Annotation Anchors, Sidecar Schema, and Destructive Redaction

## Status

Accepted (Phase 4, Milestone M0 / M5 / M6)

## Context

Review platforms require two distinct mark-up capabilities:
1. Non-destructive annotations (`Highlight`, `Note`, `AreaBox`) and proposed/committed `Redaction`s stored outside the original evidence file.
2. Burned-in, **destructive redactions** on export where every redacted character, glyph, image pixel, cell value, formula, comment, bookmark, link target, and metadata string is permanently removed from the exported artifact.

## Decision

1. **External Versioned Sidecar (`AnnotationSidecar`)**:
   - Original source documents are never mutated. Mark-up is serialized deterministically as a versioned JSON sidecar (`schema_version: 1`, documented in `docs/schemas/annotation-sidecar.schema.json`) keyed by `(document_sha256, engine_version)`.
   - Each annotation carries a geometric anchor (`page_index`, `rects_pt`) and, when attached to text, a `TextAnchor` (`SourceRef` range, `quoted_text`, `prefix`, `suffix`).
   - When loading a sidecar produced under a different `engine_version`, anchors are re-resolved against the new `LayoutDocument` via `SourceRef` and quoted-text context, reporting explicit `AnchorResolutionStatus` (`Exact`, `Moved`, `Unresolved`) and never silently dropping an annotation.

2. **Destructive Redaction Pipeline (`apply_redactions`)**:
   - **Glyph Runs**: Any glyph in a `GlyphRun` whose bounding box intersects a redaction rectangle (or whose `SourceRef` falls in a redacted text/cell target) is removed from `glyphs` and its corresponding character range is stripped from `GlyphRun.text`. Runs are split around redacted spans so no hidden text or `ToUnicode` mapping survives.
   - **Vector Paths & Images**: Raster images intersecting a redaction rectangle are decoded, the pixels under the rectangle (mapped through the image's transform) are overwritten with opaque fill, and the image is re-encoded before embedding into PDF/TIFF/SVG. Vector paths are removed only when they lie entirely inside a rectangle; partly overlapping paths stay under the burned-in box.
   - **Spreadsheets & Metadata**: Redacted cells have their value, formula, and comment stripped prior to layout and text extraction; redacted strings/fields are scrubbed from document metadata, outlines, and link targets.
   - **Verification Gate (`verify_redacted_output`)**: Every redacted export automatically re-parses the output artifact (including raw and decompressed PDF streams) and searches for all redacted strings (case-insensitive, whitespace-normalized). Any match aborts the export with an error.

## Consequences

- Redacted text, cell values and metadata are removed from the page model, and every export is searched for them before it is returned. Two limits remain: vector paths that only partly overlap a redaction are covered rather than cut, and redacting one occurrence of text that appears elsewhere unredacted fails verification.
- Hosts retain full control over sidecar persistence while getting deterministic cross-version anchor resolution.
