# ADR 0004: Rich Word Document Model (`prism-word`)

- **Status**: Accepted
- **Date**: 2026-10-23
- **Phase**: Phase 2 (Milestone M0)

## Context

In Phase 0 and Phase 1, `DocxParser` performed a single streaming pass over `word/document.xml` directly into the format-agnostic Unified Document Model (`prism_core::Document`). While sufficient for basic text extraction and simple flow pagination, the UDM intentionally omits WordprocessingML-specific semantics required for Microsoft Word Print Layout fidelity and forensic review:
- Multi-section page geometry (`w:sectPr` inside `w:pPr` vs body-level `w:sectPr`), columns, and first/even/default headers and footers.
- Full style hierarchy (`w:docDefaults`, `w:style` `basedOn` chains, conditional table styles `w:tblStylePr`, and numbering styles).
- Multi-level numbering (`w:abstractNum`, `w:num`, `w:lvl`, `w:lvlOverride`).
- Footnotes, endnotes, threaded comments, and granular revision tracking (`w:ins`, `w:del`, `w:moveFrom`, `w:moveTo`, `*Change`).
- Complex fields (`w:fldSimple`, `w:fldChar` `begin`/`separate`/`end`) where cached results must be preserved verbatim except for pagination fields (`PAGE`, `NUMPAGES`, `SECTIONPAGES`).
- Floating DrawingML (`wp:anchor`) and VML objects with wrap modes (`square`, `tight`, `through`, `topAndBottom`, `none`) and text boxes (`wps:txbx`).
- Legacy binary `.doc` ([MS-DOC]) documents, which share identical layout and forensic semantics with `.docx` once parsed.

## Decision

We introduce a dedicated crate, `crates/prism-word`, defining a rich, strongly typed [`WordDocument`](../../crates/prism-word/src/model.rs) intermediate representation:

```
DOCX Reader (prism_word::docx) ──┐
                                 ├──► WordDocument ──┬──► prism_word::layout ──► LayoutDocument (prism-layout)
DOC Reader  (prism_word::doc)  ──┘                   └──► prism_word::udm    ──► UDM Document   (prism-core)
```

1. **Unified Intermediate Representation for DOCX and DOC**:
   Both `prism_word::docx::parse_docx` and `prism_word::doc::parse_doc` produce `WordDocument`. Layout, forensic markup modes, and UDM conversion operate solely on `WordDocument`.
2. **Sections & Stories**:
   - `WordDocument::sections` holds an ordered `Vec<WordSection>`, each with its own `SectionProperties` (page size, orientation, margins, gutter, column definitions, break type, header/footer references, line numbering, page number format/start) and `Vec<WordBlock>`.
   - Headers, footers, footnotes, endnotes, and comments are stored in dedicated maps keyed by part/relationship ID or note ID, each containing `Vec<WordBlock>` so they are laid out by the exact same block/paragraph/table layout engine.
3. **Forensic Soundness Built Into the Model**:
   - External relationships (`TargetMode="External"`), attached templates, `INCLUDEPICTURE` / `INCLUDETEXT` field targets, DDE links, and VBA macro presence (`vbaProject.bin`) are recorded in `WordDocument::forensic` (`ForensicFacts`) as inert metadata and are **never** fetched or executed.
   - Fields store both `instruction: String` and `cached_result: Vec<ParagraphContent>`, ensuring stored values are displayed without recalculation.
4. **Backward Compatibility via `word_to_udm`**:
   `prism_word::udm::word_to_udm` converts any `WordDocument` into a `prism_core::Document`, keeping `DocxParser`, `HtmlRenderer`, `Document::extract_text()`, and all existing C ABI symbols working without regression.

## Consequences

- **Positive**: Clean separation between format parsing (XML / binary FIB+SPRMs), WordprocessingML semantic resolution (styles, lists, fields, revisions), and page layout (`prism-layout`).
- **Positive**: Legacy `.doc` automatically inherits full Print Layout pagination, table autofit, headers/footers, and forensic review views once mapped to `WordDocument`.
- **Negative**: Slightly higher memory footprint than a single-pass streaming UDM converter for very large documents, mitigated by bounded decompression and explicit depth/count caps.
