// SPDX-License-Identifier: AGPL-3.0-only
//! Phase 3 (`prism-drawingml`, `prism-numfmt`, `prism-ppt`, `prism-excel`) integration,
//! determinism, forensic-soundness, and zero-network-connection tests.

use prism_drawingml::{
    ColorTransform, DrawingTheme, apply_color_transforms, build_preset_geometry_paths,
    parse_chart_xml, render_chart,
};
use prism_excel::{
    DEFAULT_MAX_XLS_STREAM_BYTES, DEFAULT_MAX_XLSX_ENTRY_BYTES, evaluate_sheet_conditionals,
    is_xls_bytes, is_xlsx_zip, layout_workbook, parse_xls, parse_xlsx,
    render_worksheet_drawing_to_doc,
};
use prism_layout::{FontSystem, LayoutOptions, MarkupMode, Rect, Rgba, TextLayoutEngine};
use prism_numfmt::{CellInputValue, FormatOptions, format_cell_value, is_concealed_format};
use prism_ppt::{
    DEFAULT_MAX_PPT_STREAM_BYTES, DEFAULT_MAX_PPTX_ENTRY_BYTES, is_ppt_bytes, is_pptx_zip,
    layout_presentation, parse_ppt, parse_pptx,
};
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_render::raster::{RasterOptions, render_page_png};
use prism_render::svg::{SvgViewerOptions, render_document_viewer_html, render_page_svg};
use std::io::{Cursor, Write};
use std::net::TcpListener;
use std::path::Path;
use walkdir::WalkDir;

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let opts =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }
    cursor.into_inner()
}

#[test]
fn corpus_pptx_ppt_xlsx_xls_layout_deterministically() {
    let corpus_path = Path::new("../../test-files");
    if !corpus_path.exists() {
        return;
    }

    let options = LayoutOptions::default();
    let mut tested_count = 0usize;

    for entry in WalkDir::new(corpus_path).into_iter().flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(ext.as_str(), "pptx" | "ppt" | "xlsx" | "xls") {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }

        let (layout_a, layout_b) = if ext == "pptx" && is_pptx_zip(&bytes) {
            let Ok(pres) = parse_pptx(&bytes, DEFAULT_MAX_PPTX_ENTRY_BYTES) else {
                continue;
            };
            (
                layout_presentation(&pres, &options).expect("pptx layout a"),
                layout_presentation(&pres, &options).expect("pptx layout b"),
            )
        } else if ext == "ppt" && is_ppt_bytes(&bytes) {
            let Ok(pres) = parse_ppt(&bytes, DEFAULT_MAX_PPT_STREAM_BYTES) else {
                continue;
            };
            (
                layout_presentation(&pres, &options).expect("ppt layout a"),
                layout_presentation(&pres, &options).expect("ppt layout b"),
            )
        } else if ext == "xlsx" && is_xlsx_zip(&bytes) {
            let Ok(wb) = parse_xlsx(&bytes, DEFAULT_MAX_XLSX_ENTRY_BYTES) else {
                continue;
            };
            (
                layout_workbook(&wb, &options).expect("xlsx layout a"),
                layout_workbook(&wb, &options).expect("xlsx layout b"),
            )
        } else if ext == "xls" && is_xls_bytes(&bytes) {
            let Ok(wb) = parse_xls(&bytes, DEFAULT_MAX_XLS_STREAM_BYTES) else {
                continue;
            };
            (
                layout_workbook(&wb, &options).expect("xls layout a"),
                layout_workbook(&wb, &options).expect("xls layout b"),
            )
        } else {
            continue;
        };

        assert_eq!(layout_a.pages.len(), layout_b.pages.len());
        if let (Some(p1), Some(p2)) = (layout_a.pages.first(), layout_b.pages.first()) {
            let svg_a =
                render_page_svg(p1, &layout_a.fonts, &layout_a.images, &options).expect("svg a");
            let svg_b =
                render_page_svg(p2, &layout_b.fonts, &layout_b.images, &options).expect("svg b");
            assert_eq!(svg_a, svg_b, "Non-deterministic SVG output for {path:?}");

            let raster_opts = RasterOptions {
                dpi: 72.0,
                layout_options: options.clone(),
                ..RasterOptions::default()
            };
            let png_a =
                render_page_png(p1, &layout_a.fonts, &layout_a.images, &raster_opts).expect("png");
            assert!(png_a.starts_with(b"\x89PNG\r\n\x1a\n"));
        }

        let pdf_a = export_document_pdf(&layout_a, &PdfExportOptions::default()).expect("pdf a");
        let pdf_b = export_document_pdf(&layout_b, &PdfExportOptions::default()).expect("pdf b");
        assert_eq!(pdf_a, pdf_b, "Non-deterministic PDF output for {path:?}");

        tested_count += 1;
    }

    assert!(
        tested_count > 0,
        "Expected at least one PPTX/PPT/XLSX/XLS file in test-files/"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn zero_network_requests_on_external_workbook_links_and_charts() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking listener");
    let addr = listener.local_addr().expect("local addr");
    let trap_url = format!("http://{addr}/exfiltrate");

    // 1. Build an XLSX workbook with externalLinks, web query, and chart externalData pointing to `trap_url`
    let wb_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Forensic" sheetId="1" r:id="rId1"/>
  </sheets>
  <externalReferences>
    <externalReference r:id="rIdExt1"/>
  </externalReferences>
</workbook>"#;

    let wb_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rIdExt1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="{trap_url}/external.xlsx" TargetMode="External"/>
</Relationships>"#
    );

    let sheet_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
           xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheetData>
    <row r="1">
      <c r="A1"><f>[1]Sheet1!$A$1</f><v>1337</v></c>
    </row>
  </sheetData>
  <drawing r:id="rIdDraw1"/>
</worksheet>"#;

    let sheet_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rIdDraw1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/>
</Relationships>"#;

    let drawing_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"
          xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
          xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <xdr:twoCellAnchor>
    <xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
    <xdr:to><xdr:col>6</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>12</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>
    <xdr:graphicFrame>
      <a:graphic>
        <a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart">
          <c:chart r:id="rIdChart1"/>
        </a:graphicData>
      </a:graphic>
    </xdr:graphicFrame>
    <xdr:clientData/>
  </xdr:twoCellAnchor>
</xdr:wsDr>"#;

    let drawing_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rIdChart1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/>
</Relationships>"#;

    let chart_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
              xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
              xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <c:externalData r:id="rIdExtBook"/>
  <c:chart>
    <c:plotArea>
      <c:barChart>
        <c:ser>
          <c:tx><c:v>Q1</c:v></c:tx>
          <c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>North</c:v></c:pt></c:strCache></c:strRef></c:cat>
          <c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>42.5</c:v></c:pt></c:numCache></c:numRef></c:val>
        </c:ser>
      </c:barChart>
    </c:plotArea>
  </c:chart>
</c:chartSpace>"#;

    let xlsx_bytes = build_zip(&[
        ("xl/workbook.xml", wb_xml),
        ("xl/_rels/workbook.xml.rels", wb_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet_xml),
        ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels),
        ("xl/drawings/drawing1.xml", drawing_xml),
        ("xl/drawings/_rels/drawing1.xml.rels", drawing_rels),
        ("xl/charts/chart1.xml", chart_xml),
    ]);

    let wb = parse_xlsx(&xlsx_bytes, DEFAULT_MAX_XLSX_ENTRY_BYTES).expect("parse xlsx");
    assert!(
        wb.forensic
            .external_links
            .iter()
            .any(|u| u.contains(&trap_url)),
        "Expected trap_url to be recorded in WorkbookForensicFacts::external_links"
    );
    let doc = layout_workbook(&wb, &LayoutOptions::default()).expect("layout workbook");
    let svg = render_page_svg(
        &doc.pages[0],
        &doc.fonts,
        &doc.images,
        &LayoutOptions::default(),
    )
    .expect("render svg");
    assert!(svg.contains("<svg"));

    // Render standalone chart object via render_worksheet_drawing_to_doc
    let s0 = &wb.sheets[0];
    assert_eq!(s0.drawings.len(), 1);
    let chart_doc =
        render_worksheet_drawing_to_doc(&wb, s0, &s0.drawings[0]).expect("render chart doc");
    let chart_svg = render_page_svg(
        &chart_doc.pages[0],
        &chart_doc.fonts,
        &chart_doc.images,
        &LayoutOptions::default(),
    )
    .expect("chart svg");
    assert!(chart_svg.contains("<svg"));

    // Verify ZERO network connections were made to our loopback trap listener
    assert!(
        listener.accept().is_err(),
        "Forensic violation: parser or renderer attempted an outbound TCP connection!"
    );
}

#[test]
fn numfmt_and_drawingml_chartex_and_conditional_formatting_end_to_end() {
    // 1. Number format engine checks
    let opts_1900 = FormatOptions::default();
    assert_eq!(
        format_cell_value(
            &CellInputValue::Number(60.0),
            None,
            Some("yyyy-mm-dd"),
            &opts_1900,
        )
        .text,
        "1900-02-29"
    );
    assert!(is_concealed_format(";;;"));

    // 2. Theme color transform pipeline + preset geometry
    let theme = DrawingTheme::default();
    let accent1 = theme.resolve_scheme_color("accent1").unwrap();
    let tinted = apply_color_transforms(accent1, &[ColorTransform::Tint(0.5)]);
    assert_ne!(accent1, tinted);
    let star_cmds = build_preset_geometry_paths("star5", Rect::new(0.0, 0.0, 100.0, 100.0), &[]);
    assert!(!star_cmds.is_empty());

    // 3. Office 2016+ Extended Chart (`cx:chartSpace`) waterfall rendering
    let cx_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<cx:chartSpace xmlns:cx="http://schemas.microsoft.com/office/drawing/2014/chartex">
  <cx:chartData>
    <cx:data id="0">
      <cx:strDim type="cat">
        <cx:lvl ptCount="3">
          <cx:pt idx="0">Revenue</cx:pt>
          <cx:pt idx="1">COGS</cx:pt>
          <cx:pt idx="2">Net</cx:pt>
        </cx:lvl>
      </cx:strDim>
      <cx:numDim type="val">
        <cx:lvl ptCount="3">
          <cx:pt idx="0">100</cx:pt>
          <cx:pt idx="1">-40</cx:pt>
          <cx:pt idx="2">60</cx:pt>
        </cx:lvl>
      </cx:numDim>
    </cx:data>
  </cx:chartData>
  <cx:chart>
    <cx:title><cx:tx><cx:txData><cx:v>P&amp;L Waterfall</cx:v></cx:txData></cx:tx></cx:title>
    <cx:plotArea>
      <cx:plotAreaRegion>
        <cx:series layoutId="waterfall" dataId="0"/>
      </cx:plotAreaRegion>
    </cx:plotArea>
  </cx:chart>
</cx:chartSpace>"#;

    let chart = parse_chart_xml(cx_xml).expect("parse cx:chartSpace");
    let layout_opts = LayoutOptions::default();
    let mut font_sys = FontSystem::new(&layout_opts);
    let mut text_engine = TextLayoutEngine::default();
    let items = render_chart(
        &chart,
        Rect::new(0.0, 0.0, 360.0, 240.0),
        &theme,
        &mut font_sys,
        &mut text_engine,
    );
    assert!(!items.is_empty());

    // 4. Conditional formatting stored-value evaluation (dataBar, colorScale, iconSet, expression recorded)
    let mut sheet = prism_excel::Sheet::default();
    for (r, val) in [(0u32, 10.0), (1, 50.0), (2, 90.0)] {
        sheet.cells.insert(
            (r, 0),
            prism_excel::Cell {
                value: prism_excel::CellValue::Number(val),
                display_text: val.to_string(),
                ..prism_excel::Cell::default()
            },
        );
    }
    sheet
        .conditional_rules
        .push(prism_excel::ConditionalFormatRule {
            ranges: vec![prism_excel::CellRange {
                start_row: 0,
                start_col: 0,
                end_row: 2,
                end_col: 0,
            }],
            priority: 1,
            stop_if_true: false,
            dxf_id: None,
            kind: prism_excel::ConditionalRuleKind::DataBar {
                color: Rgba::rgb(0x3B, 0x82, 0xF6),
                show_value: true,
            },
        });
    let styles = prism_excel::WorkbookStyles::default();
    let conds = evaluate_sheet_conditionals(&sheet, &styles);
    assert!(conds.get(&(0, 0)).and_then(|c| c.data_bar).is_some());
    assert!(conds.get(&(2, 0)).and_then(|c| c.data_bar).is_some());
    let _ = MarkupMode::AllMarkup;
    let _ = SvgViewerOptions::default();
    let _ = render_document_viewer_html;
}
