# Word Engine Architecture (`prism-word`)

`prism-word` is Prism's native pure-Rust Word processing engine for OOXML (`DOCX`) and legacy binary Word (`DOC`, Word 97–2003 `[MS-DOC]`). It parses documents into a rich intermediate model (`WordDocument`) and lays them out directly onto `prism-layout`'s positioned `LayoutDocument` page model.

## Pipeline Overview

```
DOCX (OPC + WordprocessingML) ──► parse_docx() ──┐
                                                 ├──► WordDocument ──┬──► layout_word_document() ──► LayoutDocument
DOC (OLE2/CFB + [MS-DOC] FIB) ──► parse_doc()  ──┘                   └──► word_to_udm()          ──► Document (UDM)
```

1. **Front-ends (`docx.rs` and `doc.rs`)**:
   - `parse_docx` reads all relevant OPC parts (`word/document.xml`, `word/styles.xml`, `word/numbering.xml`, `word/settings.xml`, `word/fontTable.xml`, `word/theme/theme1.xml`, `word/header*.xml`, `word/footer*.xml`, `word/footnotes.xml`, `word/endnotes.xml`, `word/comments.xml`, media, and `.odttf` embedded fonts).
   - `parse_doc` reads the OLE2/CFB `WordDocument` stream, `FIB` (`FibBase`, `FibRgW97`, `FibRgLw97`, `FibRgFcLcb97`), the `0Table`/`1Table` stream, the `Clx`/`PlcPcd` piece table (supporting mixed 8-bit `CP-1252` and 16-bit `UTF-16LE` pieces), `PlcfBteChpx` and `PlcfBtePapx` FKP formatting pages (`grpprl` SPRM sequences), inline fields (`0x13`/`0x14`/`0x15`), and `0x07`-delimited table rows/cells.
2. **Style & Theme Resolution (`inheritance.rs`)**:
   - Implements the full ECMA-376 §17.7.2 style hierarchy (`docDefaults` → table style + `tblLook` conditional formatting → paragraph `basedOn` chain → character `basedOn` chain → numbering level `pPr`/`rPr` → direct formatting) with cycle detection (`MAX_STYLE_DEPTH = 32`), XOR toggle property semantics (`b`, `i`, `caps`, `smallCaps`, `strike`, `vanish`), and `theme1.xml` color/font slot resolution.
3. **Numbering Engine (`numbering.rs`)**:
   - Stateful `NumberingTracker` formatting decimal, roman, letter, ordinal, cardinal, legal (`isLgl`), and symbol bullets (mapping Symbol/Wingdings/Webdings PUA code points to Unicode).
4. **Two-Pass Page & Section Layout (`layout.rs`)**:
   - **Pass 1** lays out all sections (`nextPage`, `continuous`, `evenPage`, `oddPage`), multi-column flows (`w:cols`), headers/footers with section inheritance (`first`/`even`/`default`), footnotes at the bottom of the referencing page, endnotes at document end, floating DrawingML/VML objects with wrap exclusion zones, tables (`AutoFit` + `gridSpan` + `vMerge` + ECMA-376 §17.4.26 border conflict resolution + repeating header rows), and tracked changes (`MarkupMode`), recording total and per-section page counts.
   - **Pass 2** re-runs only when `PAGE`, `NUMPAGES`, or `SECTIONPAGES` fields were encountered and section page counts changed, guaranteeing exact pagination field values without mutating any non-pagination stored field results.
5. **WMF/EMF Vector Metafile Interpreter (`emf.rs`)**:
   - Pure-Rust interpreter for standard and Aldus Placeable WMF and 32-bit EMF records into vector `PathItem` and `GlyphRun` primitives, bounded by `MAX_METAFILE_RECORDS = 50_000`.
