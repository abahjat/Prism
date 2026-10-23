# Word Format Compatibility Matrix (`prism-word`)

This document summarizes the WordprocessingML (`ECMA-376 Part 1, §17 / §20.4`) and legacy binary Word (`[MS-DOC]`) features supported by `prism-word` in Phase 2, along with intentional forensic constraints and known limitations.

## Supported DOCX (`WordprocessingML`) Features

| Category | Elements / Features | Status |
|---|---|---|
| **Document Parts** | `document.xml`, `styles.xml`, `numbering.xml`, `settings.xml`, `fontTable.xml`, `theme1.xml`, `header*.xml`, `footer*.xml`, `footnotes.xml`, `endnotes.xml`, `comments.xml`, `vbaProject.bin` detection | Supported |
| **Style Hierarchy** | `w:docDefaults`, `w:style` (`paragraph`, `character`, `table`, `numbering`), `w:basedOn` (cycle-safe), `w:link`, XOR toggle properties (`w:b`, `w:i`, `w:caps`, `w:smallCaps`, `w:strike`, `w:vanish`), `w:rFonts` theme slots, `w:themeColor` + `w:themeTint` / `w:themeShade` | Supported |
| **Paragraphs & Runs** | `w:jc`, `w:ind` (`left`/`start`, `right`/`end`, `firstLine`, `hanging`), `w:spacing` (`before`, `after`, `line`, `lineRule`, `beforeAutospacing`, `afterAutospacing`, `contextualSpacing`), `w:tabs` (`left`, `center`, `right`, `decimal`, `bar`, `clear`, leaders), `w:pBdr` (including `between` merging), `w:shd`, `w:keepNext`, `w:keepLines`, `w:widowControl`, `w:pageBreakBefore`, `w:u`, `w:strike`, `w:dstrike`, `w:vertAlign`, `w:highlight`, `w:br`, `w:tab`, `w:sym`, `w:ptab` | Supported |
| **Numbering & Lists** | `w:abstractNum`, `w:num`, `w:lvl`, `w:lvlOverride`, `w:startOverride`, `w:lvlRestart`, `w:isLgl`, `w:suff`, multi-level `%1.%2.` templates, Wingdings/Symbol/Webdings PUA bullet mapping | Supported |
| **Tables** | `w:tblGrid`, `w:tblPr`, `w:tblW`, `w:tblLayout` (`fixed` and `autofit`), `w:tblCellMar`, `w:tblLook` conditional formatting (`firstRow`, `lastRow`, `firstColumn`, `lastColumn`, `band1Vert`, `band1Horz`), `w:gridSpan`, `w:vMerge`, `w:tcMar`, `w:vAlign`, `w:tblHeader` repeating headers, `w:cantSplit`, ECMA-376 §17.4.26 border conflict resolution, nested tables | Supported |
| **Sections & Columns** | Body and paragraph `w:sectPr`, `w:pgSz` (portrait/landscape), `w:pgMar` (margins, `header`, `footer`, `gutter`), `w:mirrorMargins`, `w:type` (`nextPage`, `continuous`, `evenPage`, `oddPage`), `w:cols` (equal and custom column widths), `w:titlePg`, `w:evenAndOddHeaders`, `w:pgBdr`, `w:lnNumType`, `w:watermark` | Supported |
| **Fields, Notes & Links** | `w:fldSimple`, complex `w:fldChar` (`begin`/`separate`/`end`) with nested fields, stored result preservation, two-pass `PAGE`/`NUMPAGES`/`SECTIONPAGES`, `w:hyperlink` + `w:bookmarkStart`, `w:footnoteReference`, `w:endnoteReference` | Supported |
| **Drawings & Shapes** | `wp:inline`, `wp:anchor`, `a:blip`, `wps:wsp` / `wps:txbx`, VML `v:shape` / `v:imagedata` / `v:textbox`, WMF/EMF vector metafile rendering | Supported |
| **Tracked Changes** | `w:ins`, `w:del`, `w:rPrChange`, `w:pPrChange`, `w:moveFrom`, `w:moveTo`, `w:commentRangeStart`/`End` + `comments.xml`, `AllMarkup`, `SimpleMarkup`, `NoMarkup`, and `Original` modes | Supported |

## Supported Legacy Binary `.doc` (`[MS-DOC]`) Features

| Feature | Details |
|---|---|
| **Container & FIB** | OLE2/CFB `WordDocument` + `0Table`/`1Table` streams, `FibBase` (`fEncrypted` detection returning `Error::Encrypted`), `FibRgLw97`, `FibRgFcLcb97` |
| **Text & Piece Table** | Full `Clx` / `PlcPcd` piece table supporting mixed 8-bit `CP-1252` (`fc` bit 30 set) and 16-bit `UTF-16LE` pieces |
| **Formatting (FKPs & SPRMs)** | `PlcfBteChpx` and `PlcfBtePapx` 512-byte FKP pages and `grpprl` SPRMs (bold, italic, underline, strike, super/subscript, font size, color, alignment, indents, spacing, page/keep flags, table flags) |
| **Fields & Tables** | Inline `0x13`/`0x14`/`0x15` field trees (preserving stored results, nesting limited to 32 levels) and `0x07` cell/row markers (`sprmPFInTable` / `sprmPFTtp`) |
| **Other stories** | Footnote, header, annotation, endnote and text box text (the CP ranges after `ccpText`) is kept in `WordDocument::unplaced_stories` and appended to the UDM text; it is not drawn on pages yet |
| **Word 6/95** | `nFib` below `0x00C0` returns `Error::UnsupportedFormat`, so `DocParser` uses its text fallback |

## Intentional Forensic Constraints & Known Limitations

- **Zero External Network or File Access**: Linked images (`r:link`), `attachedTemplate`, `INCLUDEPICTURE`, `INCLUDETEXT`, `IMPORT`, `LINK`, `DDE`, and `DDEAUTO` are never fetched or executed; they are recorded in `WordDocument::forensic_facts` and rendered as deterministic inert placeholders when no cached result exists.
- **Stored Field Fidelity**: Non-pagination fields (`DATE`, `TIME`, `AUTHOR`, `FILENAME`, `DOCPROPERTY`, `REF`, `SEQ`, `TOC`, `IF`, `=`) always display the cached result stored in the file at save time.
- **OMML Math (`m:oMath`, `m:oMathPara`)**: Linear/plain-text math runs are extracted and laid out inline; full 2D mathematical fraction/radical box layout is planned for a future phase.
