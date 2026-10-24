# ADR 0013: Deterministic Productions, Bates Numbering, Load Files, and PDF/A-2b

## Status

Accepted (Phase 4, Milestone M0 / M7)

## Context

Legal productions require converting batches of documents into numbered pages (single-page or multi-page TIFF and searchable PDF), stamping Bates numbers and 6-position endorsements without obscuring document content, generating placeholder slip sheets for withheld or native-only files, extracting per-document text, and emitting standard review-platform load files (Concordance `.dat`, Opticon `.opt`, and `.csv`) alongside a reproducible manifest.

## Decision

1. **Deterministic Bates Assignment and Resumption**:
   - Bates numbers (`{prefix}{number:0padding}{suffix}`) are assigned in deterministic input order after page-count resolution, independent of parallel worker completion order.
   - Every production writes a deterministic `manifest.json` (rewritten atomically after each document) containing the engine version, options hash, per-document status, hashes (`MD5`, `SHA-1`, `SHA-256`), and assigned begin/end Bates and begin/end family attachment Bates ranges.
   - Re-running a production with `resume` and an existing manifest reuses each completed document whose input hash, starting Bates number and on-disk artifact hashes still match, and produces the same numbering and outputs as a clean run.

2. **Non-Overlapping Endorsements**:
   - When header or footer endorsements are configured, `prism-production` reserves top and/or bottom margin bands on the target page, uniformly scales and translates the original `LayoutPage` content into the remaining safe box, and renders the 6-position endorsement text (`header_left`, `header_center`, `header_right`, `footer_left`, `footer_center`, `footer_right`) using deterministic bundled fonts.
   - Dates in endorsements and slip sheets are taken strictly from `ProductionOptions::production_date` (never `Utc::now()` unless explicitly requested by the caller).

3. **Searchable PDF and PDF/A-2b Conformance**:
   - Searchable PDFs are generated through `prism-layout`'s `krilla`-backed PDF renderer, preserving the positioned text layer (and any destructive redactions).
   - When `pdf_a2b` is set on the PDF mode, the PDF exporter runs `krilla`'s PDF/A-2b validator, which adds the XMP metadata and sRGB output intent and fails the export if the content cannot conform. The manifest does not record a separate conformance status, and the output has not been checked with an external validator yet.

4. **Load File Conventions**:
   - Concordance `.dat` defaults to standard Concordance delimiters (`\u{0014}` column delimiter `DC4`, `\u{00FE}` quote `þ`, `\u{00AE}` newline replacement `®`, CRLF line endings) with configurable delimiter overrides.
   - Opticon `.opt` emits one comma-separated record per page (`Bates,Volume,ImagePath,DocBreak,FolderBreak,BoxBreak,PageCount`).

## Consequences

- Productions are 100% deterministic, resumable after crashes, and directly importable into Relativity, Concordance, Reveal, and other standard review platforms.
