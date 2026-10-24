# Review, Hidden-Content Inspection, Annotations, and Destructive Redaction

This document describes Prism's Phase 4 review and redaction subsystem: `prism-core::review`,
`prism-parsers::review`, `prism-parsers::tree`, and `prism-review`.

## 1. Hidden-Content Report (`HiddenContentReport`)

Before a document is produced, a reviewer needs to know what the native file holds that a printed
copy does not show.

- `HiddenContentReport { items: Vec<Finding>, coverage: Vec<FormatCoverage> }`. The coverage list
  names the finding kinds each provider inspected, so "no findings" is never confused with "not
  checked".
- Each `Finding` has a `FindingKind`, a `Severity` (`Info`, `Notable`, `High`), a `Location`
  (`PageRects`, `Slide`, `SheetRange`, `PartPath`, `Document`), an optional `SourceRef` into the
  page model, an excerpt of at most 240 characters, and structured `detail` JSON.
- `FindingKind` covers revisions and comments (`TrackedInsertion`, `TrackedDeletion`,
  `TrackedMove`, `TrackedFormatChange`, `Comment`, `PdfAnnotation`), concealed text
  (`HiddenText`, `WhiteOrLowContrastText`, `TinyText`, `OffPageOrOffSlideText`, `CoveredText`,
  `CustomSuppressedCell`, `ZeroWidthCharacters`), hidden structure (`HiddenSlide`, `HiddenSheet`,
  `HiddenRowsOrColumns`, `CollapsedOutlineGroup`, `HiddenDefinedName`, `HiddenPdfLayer`,
  `SpeakerNotes`), active and linked content (`MacroCode`, `ActiveXControl`,
  `EmbeddedOleObject`, `ExternalLinkOrConnection`, `DynamicField`, `PdfJavaScript`,
  `PdfLaunchAction`, `EmbeddedFile`, `RemoteEmailImage`), and metadata
  (`AuthorAndModificationHistory`, `CustomPropertiesOrVariables`, `RsidDiversity`,
  `XmpHistory`, `IncrementalPdfUpdates`, `InconsistentEmailHeaders`).

### Providers

`generate_hidden_content_report` dispatches on the detected format:

| Format | Provider |
| :--- | :--- |
| DOCX, DOC | `prism-word` model (revisions, comments, hidden and concealed text), plus OOXML package parts (ActiveX, embeddings, RSIDs) |
| PPTX, PPT | `prism-ppt` model (hidden slides, notes, off-slide text, comments), plus OOXML package parts |
| XLSX, XLS | `prism-excel` model (hidden and very hidden sheets, hidden rows and columns, `;;;` cells, hidden names, comments) |
| PDF | annotations, optional-content layers, JavaScript and launch actions, embedded files, incremental updates |
| EML, MBOX, MSG | header consistency, remote images, CSS-concealed HTML, attachments (MBOX: first message only) |
| ZIP, TAR, 7z | members, macro or script files, hidden dot-files |
| Anything else (RTF, ODF, CSV, text, …) | zero-width characters only, plus concealed text in a precomputed layout if one is passed |

## 2. Container Family Tree (`FamilyTree`)

`prism_parsers::tree::build_family_tree` walks ZIP, TAR, 7z, GZIP and BZIP2 archives, EML/MBOX
attachments, MSG attachments and OLE `ObjectPool` streams, OOXML `embeddings/` parts (with
`media/` and `fonts/` listed but not expanded), and PDF embedded files.

- Each `ContainerNode` records its `id` (`"0/1/2"`), path, name, detected format, MIME type,
  size, MD5 / SHA-1 / SHA-256, depth, `ContainerNodeStatus`, and children.
- `ContainerLimits { max_depth, max_total_bytes, max_members }` bound the walk. Members are
  charged against the byte budget as they are decompressed, and a container stops decompressing
  once the member budget is spent, so a decompression bomb is cut off at the budget rather than
  read into memory first.
- Every cut is recorded as a status (`DepthLimitReached`, `SizeLimitReached`,
  `MemberLimitReached`, `CycleDetected` by ancestor SHA-256), never dropped silently.

## 3. Annotation Sidecar and Anchors

Annotations live outside the source document in an `AnnotationSidecar` (schema:
`docs/schemas/annotation-sidecar.schema.json`), keyed by `schema_version`, `document_sha256`,
and `engine_version`.

- Kinds: `Highlight`, `Note`, `AreaBox`, `Redaction` (`reason_code`, `label`, `fill_color`). A
  redaction with `proposed: true` is shown in preview but never applied.
- Targets: `TextRange` (rectangles plus a `TextAnchor` with quoted text and 32 characters of
  context on each side), `PageRect`, `WholePage`, `CellRange`, `MetadataField`.
- `resolve_against` checks the document hash, then uses the stored rectangles when the engine
  version matches. Under another engine version, text anchors are searched again by quoted text,
  scored by `SourceRef` and context, and reported as `Exact`, `Moved` or `Unresolved`. An
  approved redaction that is `Unresolved` stops the export.

## 4. Destructive Redaction and Verification

`prism-review::redaction` removes content rather than covering it:

1. **Text** (`apply_redactions`): glyph clusters whose box overlaps a redaction rectangle are
   removed from the run's glyphs, text and clusters, and the run is split around them. The glyph
   box is inset slightly so neighbours that only touch the edge survive; a character only partly
   under the box can therefore remain.
2. **Images**: an image entirely inside a rectangle is dropped. One partly covered is decoded,
   the covered pixels are filled (each rectangle is mapped back through the image's own
   transform, so flipped and rotated images are filled in the right place), and it is re-encoded
   as PNG. Images that cannot be decoded are dropped. Images no page uses any more are removed
   from the image store.
3. **Vector paths, links, anchors**: paths entirely inside a rectangle are removed; links and
   anchors that overlap one are removed. A path that only partly overlaps is kept under the
   burned-in box. Text drawn as outlines rather than glyphs is therefore removed only when it
   lies entirely inside the rectangle.
4. **Spreadsheets** (`apply_workbook_redactions`): cell values, display text, formulas,
   hyperlinks and comments in the range are replaced, and defined names that mention a redacted
   string are dropped. `export_redacted_document` does this on the workbook before laying it out
   when the sidecar has cell-range redactions, and refuses cell-range redactions on documents
   that are not spreadsheets.
5. **Metadata** (`scrub_metadata`): targeted fields, and any field containing a redacted string.
6. **Verification** (`verify_redacted_output`): after export, the output is searched (raw bytes,
   UTF-16LE/BE, every decompressed PDF stream and object, and the text Prism extracts from the
   PDF) for every recorded string: quoted text, each removed stretch of four or more characters,
   cell values, and metadata values. A match fails the export. Error messages identify the string
   by number and length, never by its text.

Because verification searches the whole output, redacting one occurrence of text that also
appears elsewhere unredacted fails the export. Redact every occurrence
(`propose_pattern_redactions` finds them).
