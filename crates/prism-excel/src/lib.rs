// SPDX-License-Identifier: AGPL-3.0-only
//! Native `Excel` workbook model (`Workbook`), sparse `.xlsx` and BIFF8 `.xls` (`[MS-XLS]`)
//! readers, stored-value conditional formatting, and print/grid layout for Prism.

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

pub mod condfmt;
pub mod layout;
pub mod model;
pub mod udm;
pub mod xls;
pub mod xlsx;

pub use condfmt::{ResolvedCellConditional, evaluate_sheet_conditionals};
pub use layout::{
    compute_drawing_bounds_on_sheet, layout_workbook, render_worksheet_drawing_to_doc,
};
pub use model::{
    AnchoredDrawing, BorderEdge, Cell, CellHAlign, CellRange, CellVAlign, CellValue, CellXf,
    ColumnSpan, CondOperator, ConditionalFormatRule, ConditionalRuleKind, DefinedName,
    DrawingAnchor, DxfStyle, ExcelBorder, ExcelFill, ExcelFont, FormulaInfo, PageOrientation,
    PaneInfo, PrintSetup, RichTextRun, RowProps, Sheet, SheetComment, SheetTable, SheetVisibility,
    Workbook, WorkbookForensicFacts, WorkbookStyles, WorksheetDrawingKind, col_index_to_letters,
    parse_a1_cell_ref, parse_a1_range,
};
pub use udm::workbook_to_udm;
pub use xls::{DEFAULT_MAX_XLS_STREAM_BYTES, decode_rk_number, is_xls_bytes, parse_xls};
pub use xlsx::{DEFAULT_MAX_XLSX_ENTRY_BYTES, excel_chars_to_points, is_xlsx_zip, parse_xlsx};

#[cfg(test)]
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::format_collect
)]
mod tests {
    use super::*;
    use prism_core::error::Error;
    use prism_layout::{LayoutOptions, MarkupMode};
    use std::io::{Cursor, Write};
    use zip::write::FileOptions;

    fn build_xlsx_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let opts = FileOptions::default();
            for (name, data) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn sparse_xlsx_handles_xfd1048576_and_never_recalculates_formulas() {
        let wb_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Summary" sheetId="1" r:id="rId1"/>
    <sheet name="AuditTrail" sheetId="2" state="hidden" r:id="rId2"/>
    <sheet name="Secrets" sheetId="3" state="veryHidden" r:id="rId3"/>
  </sheets>
  <definedNames>
    <definedName name="_xlnm.Print_Area" localSheetId="0">Summary!$A$1:$C$5</definedName>
  </definedNames>
</workbook>"#;

        let wb_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet3.xml"/>
</Relationships>"#;

        let styles_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <numFmts count="2">
    <numFmt numFmtId="164" formatCode="$#,##0.00"/>
    <numFmt numFmtId="165" formatCode=";;;"/>
  </numFmts>
  <fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
  <fills count="1"><fill><patternFill patternType="none"/></fill></fills>
  <borders count="1"><border><left/><right/><top/><bottom/></border></borders>
  <cellXfs count="3">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="164" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="165" fontId="0" fillId="0" borderId="0"/>
  </cellXfs>
  <dxfs count="1">
    <dxf><font><b/><color rgb="FF9C0006"/></font><fill><fgColor rgb="FFFFC7CE"/></fill></dxf>
  </dxfs>
</styleSheet>"#;

        let sst_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2">
  <si><t>Revenue</t></si>
  <si><t>Far Corner</t></si>
</sst>"#;

        // Note: B2 has formula `1+1` with cached value `999.5` -> must display `$999.50` (never recalculated to 2!)
        // C2 has `;;;` concealed format
        // XFD1048576 is at row 1,048,576, col 16,384
        let sheet1_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <cols>
    <col min="1" max="16384" width="12"/>
    <col min="4" max="5" width="0" hidden="1"/>
  </cols>
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1"><f>SUM(Z1:Z10)</f></c>
    </row>
    <row r="2" hidden="1">
      <c r="B2" s="1"><f>1+1</f><v>999.5</v></c>
      <c r="C2" s="2"><v>42</v></c>
    </row>
    <row r="1048576">
      <c r="XFD1048576" t="s"><v>1</v></c>
    </row>
  </sheetData>
  <conditionalFormatting sqref="B2:C2">
    <cfRule type="cellIs" operator="greaterThan" dxfId="0" priority="1">
      <formula>500</formula>
    </cfRule>
    <cfRule type="expression" dxfId="0" priority="2">
      <formula>A1="Revenue"</formula>
    </cfRule>
  </conditionalFormatting>
</worksheet>"#;

        let sheet2_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Hidden Audit Row</t></is></c></row></sheetData>
</worksheet>"#;

        let sheet3_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>VeryHidden Token</t></is></c></row></sheetData>
</worksheet>"#;

        let zip_bytes = build_xlsx_zip(&[
            ("xl/workbook.xml", wb_xml),
            ("xl/_rels/workbook.xml.rels", wb_rels),
            ("xl/styles.xml", styles_xml),
            ("xl/sharedStrings.xml", sst_xml),
            ("xl/worksheets/sheet1.xml", sheet1_xml),
            ("xl/worksheets/sheet2.xml", sheet2_xml),
            ("xl/worksheets/sheet3.xml", sheet3_xml),
        ]);

        assert!(is_xlsx_zip(&zip_bytes));
        let wb = parse_xlsx(&zip_bytes, DEFAULT_MAX_XLSX_ENTRY_BYTES).unwrap();
        assert_eq!(wb.sheets.len(), 3);
        assert_eq!(wb.forensic.visible_sheet_names, vec!["Summary"]);
        assert_eq!(wb.forensic.hidden_sheet_names, vec!["AuditTrail"]);
        assert_eq!(wb.forensic.very_hidden_sheet_names, vec!["Secrets"]);
        assert_eq!(wb.forensic.hidden_row_count, 1);
        assert_eq!(wb.forensic.hidden_col_count, 2);
        assert_eq!(wb.forensic.concealed_cell_count, 1);
        assert_eq!(wb.forensic.formulas_without_cached_value, 1);
        assert_eq!(wb.forensic.unevaluated_conditional_rules, 1);

        let s1 = &wb.sheets[0];
        // Sparse cell storage: only 5 populated cells exist in the BTreeMap despite XFD1048576!
        assert_eq!(s1.cells.len(), 5);
        assert_eq!(
            s1.cells.get(&(1_048_575, 16_383)).unwrap().display_text,
            "Far Corner"
        );
        // Formula B2 (`1+1` with cached `999.5`) displays `$999.50`
        let b2 = s1.cells.get(&(1, 1)).unwrap();
        assert_eq!(b2.display_text, "$999.50");
        assert_eq!(b2.formula.as_ref().unwrap().expression, "1+1");

        // Conditional formatting evaluates `cellIs > 500` on B2 (`999.5 > 500`)
        let conds = evaluate_sheet_conditionals(s1, &wb.styles);
        assert!(conds.contains_key(&(1, 1)));
        assert_eq!(conds[&(1, 1)].bold, Some(true));

        // Layout with show_hidden_text = true includes all 3 sheets
        let doc_all = layout_workbook(
            &wb,
            &LayoutOptions {
                show_hidden_text: true,
                markup_mode: MarkupMode::AllMarkup,
                ..LayoutOptions::default()
            },
        )
        .unwrap();
        assert_eq!(doc_all.pages.len(), 3);
        assert!(doc_all.pages[0].text().contains("$999.50"));
        assert!(doc_all.pages[2].text().contains("VeryHidden Token"));
    }

    #[test]
    fn parses_biff8_xls_and_rejects_encrypted_filepass() {
        // 1. Test encrypted XLS with FILEPASS (0x002F)
        let cursor = Cursor::new(Vec::new());
        let mut comp = cfb::CompoundFile::create(cursor).unwrap();
        {
            let mut stream = comp.create_stream("/Workbook").unwrap();
            let mut biff = Vec::new();
            // BOF (0x0809), len 16, version 0x0600, dt 0x0005 (globals)
            biff.extend_from_slice(&0x0809u16.to_le_bytes());
            biff.extend_from_slice(&16u16.to_le_bytes());
            biff.extend_from_slice(&0x0600u16.to_le_bytes());
            biff.extend_from_slice(&0x0005u16.to_le_bytes());
            biff.extend_from_slice(&[0u8; 12]);
            // FILEPASS (0x002F), len 4
            biff.extend_from_slice(&0x002Fu16.to_le_bytes());
            biff.extend_from_slice(&4u16.to_le_bytes());
            biff.extend_from_slice(&[0u8; 4]);
            stream.write_all(&biff).unwrap();
        }
        let enc_bytes = comp.into_inner().into_inner();
        assert!(is_xls_bytes(&enc_bytes));
        let err = parse_xls(&enc_bytes, DEFAULT_MAX_XLS_STREAM_BYTES).unwrap_err();
        assert!(matches!(err, Error::Encrypted(_)));

        // 2. Test unencrypted BIFF8 XLS with BOUNDSHEET8 (Visible + VeryHidden) and RK/NUMBER cells
        let cursor2 = Cursor::new(Vec::new());
        let mut comp2 = cfb::CompoundFile::create(cursor2).unwrap();
        {
            let mut stream = comp2.create_stream("/Workbook").unwrap();
            let mut biff = Vec::new();
            // Globals BOF
            biff.extend_from_slice(&0x0809u16.to_le_bytes());
            biff.extend_from_slice(&16u16.to_le_bytes());
            biff.extend_from_slice(&0x0600u16.to_le_bytes());
            biff.extend_from_slice(&0x0005u16.to_le_bytes());
            biff.extend_from_slice(&[0u8; 12]);
            // BOUNDSHEET8 Visible "Ledger"
            let bs1_name = b"Ledger";
            biff.extend_from_slice(&0x0085u16.to_le_bytes());
            biff.extend_from_slice(&14u16.to_le_bytes());
            biff.extend_from_slice(&0u32.to_le_bytes());
            biff.push(0x00); // Visible
            biff.push(0x00); // Worksheet
            biff.push(bs1_name.len() as u8);
            biff.push(0x00); // Latin-1
            biff.extend_from_slice(bs1_name);
            // BOUNDSHEET8 VeryHidden "InternalMacroData"
            let bs2_name = b"InternalMacroData";
            biff.extend_from_slice(&0x0085u16.to_le_bytes());
            biff.extend_from_slice(&(8 + bs2_name.len() as u16).to_le_bytes());
            biff.extend_from_slice(&0u32.to_le_bytes());
            biff.push(0x02); // VeryHidden
            biff.push(0x00);
            biff.push(bs2_name.len() as u8);
            biff.push(0x00);
            biff.extend_from_slice(bs2_name);
            // Globals EOF
            biff.extend_from_slice(&0x000Au16.to_le_bytes());
            biff.extend_from_slice(&0u16.to_le_bytes());

            // Worksheet 1 BOF (dt = 0x0010)
            biff.extend_from_slice(&0x0809u16.to_le_bytes());
            biff.extend_from_slice(&16u16.to_le_bytes());
            biff.extend_from_slice(&0x0600u16.to_le_bytes());
            biff.extend_from_slice(&0x0010u16.to_le_bytes());
            biff.extend_from_slice(&[0u8; 12]);
            // NUMBER (0x0203) at (0, 0) = 1250.75
            biff.extend_from_slice(&0x0203u16.to_le_bytes());
            biff.extend_from_slice(&14u16.to_le_bytes());
            biff.extend_from_slice(&0u16.to_le_bytes()); // row 0
            biff.extend_from_slice(&0u16.to_le_bytes()); // col 0
            biff.extend_from_slice(&0u16.to_le_bytes()); // ixfe 0
            biff.extend_from_slice(&1250.75f64.to_le_bytes());
            // Worksheet 1 EOF
            biff.extend_from_slice(&0x000Au16.to_le_bytes());
            biff.extend_from_slice(&0u16.to_le_bytes());

            stream.write_all(&biff).unwrap();
        }
        let xls_bytes = comp2.into_inner().into_inner();
        let wb = parse_xls(&xls_bytes, DEFAULT_MAX_XLS_STREAM_BYTES).unwrap();
        assert_eq!(wb.sheets.len(), 2);
        assert_eq!(wb.forensic.visible_sheet_names, vec!["Ledger"]);
        assert_eq!(
            wb.forensic.very_hidden_sheet_names,
            vec!["InternalMacroData"]
        );
        assert_eq!(
            wb.sheets[0].cells.get(&(0, 0)).unwrap().display_text,
            "1250.75"
        );
    }

    /// One-sheet workbook around `sheet_inner` (the children of `<worksheet>`).
    fn one_sheet_xlsx(sheet_inner: &str, defined_names: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let wb = format!(
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets><definedNames>{defined_names}</definedNames></workbook>"#
        );
        let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let sheet = format!(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">{sheet_inner}</worksheet>"#
        );
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
        ];
        entries.extend_from_slice(extra);
        build_xlsx_zip(&entries)
    }

    fn inline(cell_ref: &str, text: &str) -> String {
        format!(r#"<c r="{cell_ref}" t="inlineStr"><is><t>{text}</t></is></c>"#)
    }

    fn print_text(wb: &Workbook, options: &LayoutOptions) -> (usize, String) {
        let doc = layout_workbook(wb, options).unwrap();
        let text = doc
            .pages
            .iter()
            .map(prism_layout::LayoutPage::text)
            .collect();
        (doc.pages.len(), text)
    }

    #[test]
    fn print_layout_covers_the_whole_used_range_and_skips_blank_pages() {
        let rows: String = (1..=6000)
            .map(|r| {
                format!(
                    r#"<row r="{r}">{}</row>"#,
                    inline(&format!("A{r}"), &format!("row{r}"))
                )
            })
            .collect();
        let far = format!(
            r#"<row r="1048576">{}</row>"#,
            inline("XFD1048576", "FarCorner")
        );
        let data = one_sheet_xlsx(&format!("<sheetData>{rows}{far}</sheetData>"), "", &[]);
        let wb = parse_xlsx(&data, DEFAULT_MAX_XLSX_ENTRY_BYTES).unwrap();
        let (pages, text) = print_text(&wb, &LayoutOptions::default());
        assert!(text.contains("row6000"), "rows past 5,000 are printed");
        assert!(text.contains("FarCorner"), "XFD1048576 is printed");
        // The pages of column A plus the one page holding XFD1048576; not the millions of
        // blank pages in between.
        assert!(pages < 200, "{pages} pages");
    }

    #[test]
    fn whole_sheet_print_area_and_merge_stay_cheap() {
        let start = std::time::Instant::now();
        let data = one_sheet_xlsx(
            &format!(
                r#"<sheetData><row r="1">{}</row></sheetData><mergeCells><mergeCell ref="A1:XFD1048576"/></mergeCells>"#,
                inline("A1", "Top")
            ),
            r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Data!$A$1:$XFD$1048576</definedName>"#,
            &[],
        );
        let wb = parse_xlsx(&data, DEFAULT_MAX_XLSX_ENTRY_BYTES).unwrap();
        let (pages, text) = print_text(&wb, &LayoutOptions::default());
        assert_eq!(pages, 1);
        assert!(text.contains("Top"));
        assert!(start.elapsed().as_secs() < 20);
    }

    #[test]
    fn print_layout_reports_too_many_pages() {
        let data = one_sheet_xlsx(
            &format!(
                r#"<sheetData><row r="1">{}</row><row r="5000">{}</row></sheetData>"#,
                inline("A1", "a"),
                inline("A5000", "b")
            ),
            "",
            &[],
        );
        let wb = parse_xlsx(&data, DEFAULT_MAX_XLSX_ENTRY_BYTES).unwrap();
        let options = LayoutOptions {
            max_pages: 1,
            ..LayoutOptions::default()
        };
        assert!(layout_workbook(&wb, &options).is_err());
    }

    #[test]
    fn udm_text_keeps_every_row_concealed_values_and_comments() {
        let rows: String = (1..=2500)
            .map(|r| {
                format!(
                    r#"<row r="{r}">{}</row>"#,
                    inline(&format!("A{r}"), &format!("row{r}"))
                )
            })
            .collect();
        let hidden = r#"<row r="2501"><c r="B2501" s="1"><v>4242</v></c></row>"#;
        let far = format!(r#"<row r="2502">{}</row>"#, inline("ZZ2502", "WideCell"));
        let styles = br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts><numFmt numFmtId="164" formatCode=";;;"/></numFmts><cellXfs><xf numFmtId="0"/><xf numFmtId="164"/></cellXfs></styleSheet>"#;
        let comments = br#"<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors><author>Reviewer</author></authors><commentList><comment ref="A1" authorId="0"><text><r><t>Check this</t></r></text></comment></commentList></comments>"#;
        let sheet_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments1.xml"/></Relationships>"#;
        let data = one_sheet_xlsx(
            &format!("<sheetData>{rows}{hidden}{far}</sheetData>"),
            "",
            &[
                ("xl/styles.xml", styles),
                ("xl/comments1.xml", comments),
                ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels),
            ],
        );
        let wb = parse_xlsx(&data, DEFAULT_MAX_XLSX_ENTRY_BYTES).unwrap();
        let text = workbook_to_udm(&wb, "XLSX").extract_text();
        assert!(text.contains("row2500"), "rows past 2,000 are extracted");
        assert!(text.contains("4242"), "a `;;;` value is still extractable");
        assert!(
            text.contains("WideCell"),
            "cells past column 128 are extracted"
        );
        assert!(text.contains("Check this"), "comments are extracted");
    }

    #[test]
    fn entity_references_are_kept_in_strings_and_formulas() {
        let sst = br#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>AT&amp;T&#10;line 2</t></si></sst>"#;
        let data = one_sheet_xlsx(
            r#"<sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="str"><f>A1&amp;"&lt;x&gt;"</f><v>AT&amp;T</v></c></row></sheetData>"#,
            "",
            &[("xl/sharedStrings.xml", sst)],
        );
        let wb = parse_xlsx(&data, DEFAULT_MAX_XLSX_ENTRY_BYTES).unwrap();
        let cells = &wb.sheets[0].cells;
        assert_eq!(cells[&(0, 0)].display_text, "AT&T\nline 2");
        assert_eq!(
            cells[&(0, 1)].formula.as_ref().unwrap().expression,
            r#"A1&"<x>""#
        );
        assert_eq!(cells[&(0, 1)].display_text, "AT&T");
    }

    #[test]
    fn xls_cell_comments_are_read() {
        fn rec(out: &mut Vec<u8>, rec_type: u16, payload: &[u8]) {
            out.extend_from_slice(&rec_type.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
            out.extend_from_slice(payload);
        }
        let bof = |dt: u16| {
            let mut p = 0x0600u16.to_le_bytes().to_vec();
            p.extend_from_slice(&dt.to_le_bytes());
            p.extend_from_slice(&[0u8; 12]);
            p
        };
        let mut biff = Vec::new();
        rec(&mut biff, 0x0809, &bof(0x0005));
        let mut bs = 0u32.to_le_bytes().to_vec();
        bs.extend_from_slice(&[0, 0, 4, 0]);
        bs.extend_from_slice(b"Data");
        rec(&mut biff, 0x0085, &bs);
        rec(&mut biff, 0x000A, &[]);
        rec(&mut biff, 0x0809, &bof(0x0010));
        // OBJ: ftCmo (ft 0x15, cb 0x12, ot 0x19 = note, id 7)
        let mut obj = vec![0x15, 0x00, 0x12, 0x00, 0x19, 0x00, 0x07, 0x00];
        obj.extend_from_slice(&[0u8; 14]);
        rec(&mut biff, 0x005D, &obj);
        // TXO with cchText = 11, then the text in a CONTINUE record (8-bit characters)
        let mut txo = vec![0u8; 18];
        txo[10..12].copy_from_slice(&11u16.to_le_bytes());
        rec(&mut biff, 0x01B6, &txo);
        let mut cont = vec![0u8];
        cont.extend_from_slice(b"Check this!");
        rec(&mut biff, 0x003C, &cont);
        // NOTE at B3 for object 7, author "Reviewer"
        let mut note = Vec::new();
        for v in [2u16, 1, 0, 7] {
            note.extend_from_slice(&v.to_le_bytes());
        }
        note.extend_from_slice(&8u16.to_le_bytes());
        note.push(0);
        note.extend_from_slice(b"Reviewer");
        rec(&mut biff, 0x001C, &note);
        rec(&mut biff, 0x000A, &[]);

        let mut comp = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        comp.create_stream("/Workbook")
            .unwrap()
            .write_all(&biff)
            .unwrap();
        let bytes = comp.into_inner().into_inner();
        let wb = parse_xls(&bytes, DEFAULT_MAX_XLS_STREAM_BYTES).unwrap();
        let comment = &wb.sheets[0].comments[&(2, 1)];
        assert_eq!(comment.author, "Reviewer");
        assert_eq!(comment.text, "Check this!");
        assert!(
            workbook_to_udm(&wb, "XLS")
                .extract_text()
                .contains("Check this!")
        );
    }
}
