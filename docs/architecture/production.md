# Productions, Bates Numbering, Load Files, and PDF/A-2b

This document describes `prism-production`, Prism's deterministic eDiscovery production engine.

## 1. Overview

`run_production` takes an ordered slice of `ProductionInput` documents and a `ProductionOptions`
configuration and writes a self-contained production volume:

```text
<output_dir>/
  IMAGES/       # Single-page TIFF or PNG, multi-page TIFF, per-document PDFs, and/or a combined volume PDF
  TEXT/         # Per-document extracted text (after redaction, optionally page-break delimited)
  NATIVES/      # Native files, when requested and the document has no approved redactions
  LOADFILES/    # Concordance (.dat), Opticon (.opt), and CSV (.csv) load files
  manifest.json # Hashes, Bates assignments, per-document status, and every artifact's SHA-256
```

## 2. Bates Numbering and Family Ranges

- `BatesConfig` defines `prefix`, `start_number`, `padding` (zero-padded width), and `suffix`.
- Every produced page across the batch gets the next sequential number
  (`{prefix}{number:0padding}{suffix}`), assigned in input order.
- Each document's `begin_bates` / `end_bates` are recorded in `ProducedDocumentRecord`.
- Documents linked by `parent_doc_id` share a family range: `begin_attach` / `end_attach` span
  the whole family. Numbers that outgrow their padding still sort correctly.

## 3. Page Endorsements

- `EndorsementConfig` has six templates: `header_left`, `header_center`, `header_right`,
  `footer_left`, `footer_center`, `footer_right`.
- Templates expand `{bates}`, `{doc_id}`, `{page}`, `{page_count}`, `{confidentiality}`, and
  `{date}`. The date comes from `ProductionOptions::production_date`, never the clock.
- When any header or footer is set, `apply_endorsements_to_page` scales the page content into
  the inner box left by the bands, so endorsements never overlap content.

## 4. Slip Sheets

Documents that are withheld (`Privileged`, `Withheld`), produced natively (`NativeOnly`, or
spreadsheets under `SpreadsheetProductionMode::NativeWithSlipSheet`), or fail to parse
(`Failed`) are replaced by a one-page slip sheet showing the Bates number, document ID, file
name, date, and reason.

A document with approved redactions never gets its native file produced, and a document that
fails to parse while carrying approved redactions stops the production with an error instead of
falling back to a slip sheet.

## 5. Output Formats

- **TIFF / PNG**: rendered at `options.dpi`.
- **PDF**: `prism_render::pdf::export_document_pdf`. With `pdf_a2b`, krilla validates the
  output against PDF/A-2b while writing it and adds the XMP metadata and sRGB output intent;
  export fails if the content cannot conform. The output has not yet been checked with an
  external validator such as veraPDF.
- Every artifact of a redacted document is passed through `verify_redacted_output` before it is
  written.

## 6. Load Files and Resumption

- **Concordance `.dat`**: field separator `\u{0014}` (DC4), quote `\u{00FE}` (`þ`), in-field
  newline `\u{00AE}` (`®`).
- **Opticon `.opt`**: one row per page (`Bates,Volume,ImagePath,DocBreak,FolderBreak,BoxBreak,PageCount`).
- **CSV `.csv`**: RFC 4180.
- Reference parsers (`parse_concordance_dat`, `parse_opticon_opt`, `parse_csv_load_file`) are used
  by the tests to check round trips. The CSV reader does not accept quoted fields that contain
  line breaks.
- **Resumption**: `manifest.json` is rewritten (to a temporary file, then renamed) after every
  document. A rerun with `options.resume` and the same options hash and engine version reuses a
  document only when its input hash matches, it still starts at the same Bates number, and every
  recorded artifact is on disk with the recorded SHA-256. Combined-volume PDFs are always
  rebuilt.
