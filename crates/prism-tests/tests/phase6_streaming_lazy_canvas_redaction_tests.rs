// SPDX-License-Identifier: AGPL-3.0-only
//! Phase 6 integration tests (`M6.1`–`M6.5`):
//! - `M6.1`: `DocumentSource` (`Read + Seek`) streaming architecture & zero-copy path-based `prism-sandbox` / C ABI sessions
//! - `M6.2`: `LazyWorkbook` on-demand sheet parsing & explicit `ExcelParseLimits` resource caps
//! - `M6.3`: Incremental page-range pagination (`open_and_layout_page_range`) & streaming production (`export_pages_streaming`)
//! - `M6.4`: Virtualized HTML5 `<canvas>` spreadsheet grid (`SpreadsheetCanvasGrid`) in `prism-viewer`
//! - `M6.5`: Sub-path vector clipping (`clip_path_item`) & region-scoped redaction verification (`verify_redacted_regions`)

use prism_core::ENGINE_VERSION;
use prism_core::error::Error;
use prism_core::metadata::Metadata;
use prism_core::review::{ReviewRect, SourceRef};
use prism_core::source::DocumentSource;
use prism_excel::{ExcelParseLimits, LazyWorkbook, parse_xlsx_with_limits};
use prism_layout::{
    Affine, Cluster, Decorations, FillRule, FontSourceKind, GlyphRun, Item, LayoutDocument,
    LayoutOptions, LayoutPage, LineCap, LineJoin, PathCommand, PathItem, Point, PositionedGlyph,
    Rgba, Size, StrokeStyle, interpret_pdf_bytes,
};
use prism_parsers::pipeline::{
    open_and_layout_page_range, open_and_layout_path, open_and_layout_source,
};
use prism_production::{
    BatesConfig, ProductionImageFormat, ProductionOptions, export_pages_streaming,
};
use prism_render::svg::{SvgViewerOptions, render_document_viewer_html};
use prism_review::{
    Annotation, AnnotationAnchor, AnnotationKind, AnnotationSidecar, RedactedExportFormat,
    RedactionTarget, TextAnchor, apply_redactions, export_redacted_document,
    verify_redacted_regions,
};
use prism_sandbox::{RenderOutputFormat, SandboxConfig, SandboxManager};
use std::io::{Cursor, Write};
use zip::ZipWriter;
use zip::write::FileOptions;

const TEST_FONT_BYTES: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");

fn build_two_sheet_xlsx() -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut buf);
        let opts = FileOptions::default();

        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
</Types>"#,
        )
        .unwrap();

        zip.start_file("xl/workbook.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Summary" sheetId="1" r:id="rId1"/>
    <sheet name="Ledger" sheetId="2" r:id="rId2" state="hidden"/>
  </sheets>
</workbook>"#,
        )
        .unwrap();

        zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Target="worksheets/sheet2.xml"/>
</Relationships>"#,
        )
        .unwrap();

        zip.start_file("xl/sharedStrings.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="3" uniqueCount="3">
  <si><t>Summary Header</t></si>
  <si><t>Ledger Secret</t></si>
  <si><t>Third String</t></si>
</sst>"#,
        )
        .unwrap();

        zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1"><v>100</v></c>
    </row>
  </sheetData>
</worksheet>"#,
        )
        .unwrap();

        zip.start_file("xl/worksheets/sheet2.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>1</v></c>
      <c r="B1"><v>999.5</v></c>
    </row>
  </sheetData>
</worksheet>"#,
        )
        .unwrap();

        zip.finish().unwrap();
    }
    buf.into_inner()
}

fn build_multipage_docx() -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut buf);
        let opts = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#,
        )
        .unwrap();

        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Page One Content Alpha</w:t></w:r></w:p>
    <w:p><w:r><w:br w:type="page"/></w:r></w:p>
    <w:p><w:r><w:t>Page Two Content Beta</w:t></w:r></w:p>
    <w:p><w:r><w:br w:type="page"/></w:r></w:p>
    <w:p><w:r><w:t>Page Three Content Gamma</w:t></w:r></w:p>
  </w:body>
</w:document>"#,
        )
        .unwrap();
        zip.finish().unwrap();
    }
    buf.into_inner()
}

#[test]
fn m6_1_document_source_streaming_and_zero_copy_sandbox_path_open() {
    let temp_dir = std::env::temp_dir().join("prism_phase6_m6_1_test");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();

    let docx_path = temp_dir.join("sample.docx");
    let docx_bytes = build_multipage_docx();
    std::fs::write(&docx_path, &docx_bytes).unwrap();

    // 1. DocumentSource random-access prefix + range + format detection
    let src = DocumentSource::from_path(&docx_path).expect("from_path");
    assert_eq!(src.len(), docx_bytes.len() as u64);
    assert_eq!(src.filename(), Some("sample.docx"));
    let prefix = src.read_prefix(4).expect("read_prefix");
    assert_eq!(&prefix, b"PK\x03\x04");
    let range = src.read_range(0, 4).expect("read_range");
    assert_eq!(&range, b"PK\x03\x04");
    let det = src
        .detect_format()
        .expect("detect_format")
        .expect("detected");
    assert_eq!(det.format.extension, "docx");

    // Verify bounded byte cap rejects oversized reads
    let err = src.to_bytes_bounded(16).expect_err("must exceed 16B cap");
    assert!(matches!(err, Error::MemoryLimitExceeded { .. }));

    // 2. open_and_layout_source & open_and_layout_path
    let opened_src =
        open_and_layout_source(&src, &LayoutOptions::default()).expect("open_and_layout_source");
    let opened_path =
        open_and_layout_path(&docx_path, &LayoutOptions::default()).expect("open_and_layout_path");
    assert_eq!(opened_src.layout.pages.len(), 3);
    assert_eq!(opened_path.layout.pages.len(), 3);

    // 3. SandboxManager::open_session_from_path (zero file bytes over IPC pipe)
    let mgr = SandboxManager::new(SandboxConfig::default());
    let mut session = mgr
        .open_session_from_path(&docx_path, None)
        .expect("open_session_from_path");
    assert_eq!(session.page_count(), 3);
    let p2_text = session.extract_page_text(1).expect("page 2 text");
    assert!(p2_text.contains("Page Two Content Beta"));
    let svg_out = session
        .render_page(1, RenderOutputFormat::Svg, 150.0)
        .expect("render page 2");
    assert!(String::from_utf8_lossy(&svg_out.data).contains("<svg"));
    session.close().expect("close session");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn m6_2_lazy_workbook_sheet_loading_and_resource_caps() {
    let xlsx_bytes = build_two_sheet_xlsx();

    // 1. LazyWorkbook opens shared metadata without parsing any worksheet XML
    let mut lazy = LazyWorkbook::open(xlsx_bytes.clone()).expect("LazyWorkbook::open");
    assert_eq!(lazy.descriptors().len(), 2);
    assert_eq!(lazy.descriptors()[0].name, "Summary");
    assert_eq!(lazy.descriptors()[1].name, "Ledger");
    assert!(!lazy.is_sheet_loaded(0));
    assert!(!lazy.is_sheet_loaded(1));

    // 2. Load only sheet 1 ("Ledger") on demand; sheet 0 stays unparsed
    let sheet1 = lazy.load_sheet(1).expect("load_sheet(1)");
    assert_eq!(sheet1.name, "Ledger");
    assert_eq!(
        sheet1.cells.get(&(0, 0)).map(|c| c.display_text.as_str()),
        Some("Ledger Secret")
    );
    assert!(!lazy.is_sheet_loaded(0));
    assert!(lazy.is_sheet_loaded(1));

    // 3. Explicit ExcelParseLimits enforce shared-string and per-sheet cell caps
    let strict_sst_limits = ExcelParseLimits {
        max_shared_strings: 2,
        ..ExcelParseLimits::default()
    };
    let sst_err = parse_xlsx_with_limits(&xlsx_bytes, &strict_sst_limits)
        .expect_err("must reject > 2 shared strings");
    assert!(matches!(sst_err, Error::MemoryLimitExceeded { .. }));

    let strict_cell_limits = ExcelParseLimits {
        max_cells_per_sheet: 1,
        ..ExcelParseLimits::default()
    };
    let cell_err = parse_xlsx_with_limits(&xlsx_bytes, &strict_cell_limits)
        .expect_err("must reject > 1 cell per sheet");
    assert!(matches!(cell_err, Error::MemoryLimitExceeded { .. }));
}

#[test]
fn m6_3_incremental_page_range_layout_and_streaming_production() {
    let docx_bytes = build_multipage_docx();
    let opts = LayoutOptions::default();

    // 1. Lay out only page range 1..2 (the 2nd page) of a 3-page DOCX
    let range_doc = open_and_layout_page_range(&docx_bytes, Some("multi.docx"), &opts, 1..2)
        .expect("open_and_layout_page_range");
    assert_eq!(range_doc.layout.pages.len(), 1);
    assert_eq!(range_doc.layout.pages[0].index, 1);
    assert!(
        range_doc.layout.pages[0]
            .text()
            .contains("Page Two Content Beta")
    );

    // 2. Stream all 3 pages with batch_size = 1 via export_pages_streaming
    let prod_opts = ProductionOptions {
        bates: BatesConfig {
            prefix: "STREAM".to_string(),
            start_number: 10,
            padding: 5,
            suffix: String::new(),
        },
        image_format: ProductionImageFormat::SinglePagePng,
        dpi: 72.0,
        ..ProductionOptions::default()
    };
    let mut streamed_bates = Vec::new();
    let count = export_pages_streaming(
        &docx_bytes,
        Some("multi.docx"),
        &prod_opts,
        1,
        |_idx, bates, bytes| {
            assert!(bytes.starts_with(b"\x89PNG"));
            streamed_bates.push(bates);
            Ok(())
        },
    )
    .expect("export_pages_streaming");
    assert_eq!(count, 3);
    assert_eq!(
        streamed_bates,
        vec![
            "STREAM00010".to_string(),
            "STREAM00011".to_string(),
            "STREAM00012".to_string()
        ]
    );
}

#[test]
fn m6_4_virtualized_html5_canvas_spreadsheet_grid_in_viewer() {
    let docx_bytes = build_multipage_docx();
    let opened = open_and_layout_page_range(
        &docx_bytes,
        Some("multi.docx"),
        &LayoutOptions::default(),
        0..1,
    )
    .unwrap();
    let viewer_html = render_document_viewer_html(&opened.layout, &SvgViewerOptions::default())
        .expect("render_document_viewer_html");

    assert!(viewer_html.contains(r#"id="prism-spreadsheet-canvas""#));
    assert!(viewer_html.contains(r#"id="prism-spreadsheet-grid-container""#));
    assert!(viewer_html.contains(r#"id="prism-btn-sheet-grid-mode""#));
    assert!(viewer_html.contains("class SpreadsheetCanvasGrid"));
    assert!(viewer_html.contains("fetchVisibleTile"));
    assert!(viewer_html.contains("frozenRows"));
}

fn make_glyph_run(
    doc: &mut LayoutDocument,
    font_id: prism_layout::FontId,
    text: &str,
    x0: f32,
    baseline_y: f32,
) -> GlyphRun {
    let mut glyphs = Vec::new();
    let mut clusters = Vec::new();
    let mut x = 0.0_f32;
    let mut byte_idx = 0u32;
    for (i, ch) in text.chars().enumerate() {
        let adv = 7.0_f32;
        let gid = doc.fonts.map_char_to_glyph(font_id, ch);
        glyphs.push(PositionedGlyph {
            glyph_id: gid,
            x,
            y: 0.0,
            advance: adv,
        });
        let ch_len = ch.len_utf8() as u32;
        let gi = i as u32;
        clusters.push(Cluster {
            text_range: byte_idx..(byte_idx + ch_len),
            glyph_range: gi..(gi + 1),
            x,
            advance: adv,
        });
        x += adv;
        byte_idx += ch_len;
    }
    GlyphRun {
        font: font_id,
        size: 12.0,
        color: Rgba::BLACK,
        transform: Affine::translate(x0, baseline_y),
        glyphs,
        text: text.to_string(),
        clusters,
        decorations: Decorations::default(),
        source: SourceRef::new(0, 0, 0),
    }
}

#[test]
fn m6_5_sub_path_vector_clipping_and_region_scoped_redaction_verification() {
    let raw_bytes = b"Document with repeated John Smith and vector line";
    let mut doc = LayoutDocument::new();
    let font_id = doc.fonts.register_ttf(
        "Liberation Sans",
        400,
        false,
        FontSourceKind::Bundled,
        TEST_FONT_BYTES.to_vec(),
        0,
    );
    let mut page = LayoutPage::new(0, Size::LETTER);

    // Occurrence 1 of "John Smith" at y = 100 (will be redacted)
    let run1 = make_glyph_run(&mut doc, font_id, "Redacted party: John Smith", 72.0, 100.0);
    // Occurrence 2 of "John Smith" at y = 240 (outside redaction box, must remain intact!)
    let run2 = make_glyph_run(
        &mut doc,
        font_id,
        "Unredacted witness: John Smith",
        72.0,
        240.0,
    );
    page.items.push(Item::Glyphs(run1));
    page.items.push(Item::Glyphs(run2));

    // Vector line spanning x = 50..300 at y = 96 (crosses right through the redaction box)
    page.items.push(Item::Path(PathItem {
        commands: vec![
            PathCommand::MoveTo(Point::new(50.0, 96.0)),
            PathCommand::LineTo(Point::new(300.0, 96.0)),
        ],
        fill: None,
        fill_rule: FillRule::NonZero,
        stroke: Some(StrokeStyle {
            color: Rgba::rgb(20, 20, 200),
            width: 2.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            dash_array: Vec::new(),
            dash_offset: 0.0,
        }),
        transform: Affine::IDENTITY,
    }));
    doc.pages.push(page);

    // Build a redaction covering ONLY the first "John Smith" at y = 85..108, x = 180..260
    let red_rect = ReviewRect::new(180.0, 85.0, 80.0, 23.0);
    let mut sidecar = AnnotationSidecar::new(raw_bytes);
    sidecar.add(Annotation {
        id: "red-john-1".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "PII".to_string(),
            label: "REDACTED".to_string(),
            fill_color: Some("#000000".to_string()),
        },
        anchor: AnnotationAnchor {
            target: RedactionTarget::PageRect,
            page_index: 0,
            rects_pt: vec![red_rect],
            text_anchor: Some(TextAnchor {
                quoted_text: "John Smith".to_string(),
                prefix: "Redacted party: ".to_string(),
                suffix: String::new(),
                start_source: SourceRef::new(0, 0, 16),
                end_source: SourceRef::new(0, 0, 26),
            }),
        },
        author: None,
        created_at: None,
        proposed: false,
    });

    // 1. Check vector sub-path clipping directly on `doc`
    let resolved = sidecar
        .resolve_against(raw_bytes, ENGINE_VERSION, &doc)
        .expect("resolve");
    let mut clipped_doc = doc.clone();
    let log =
        apply_redactions(&mut clipped_doc, &sidecar.document_sha256, &resolved).expect("apply");
    assert_eq!(log.total_applied, 1);

    // Find the surviving blue stroke PathItem and verify it was split into two exterior segments (50..180 and 260..300)
    let stroke_path = clipped_doc.pages[0]
        .items
        .iter()
        .find_map(|it| match it {
            Item::Path(p) if p.stroke.is_some() => Some(p),
            _ => None,
        })
        .expect("clipped stroke path must survive outside redaction box");
    assert_eq!(
        stroke_path
            .commands
            .iter()
            .filter(|c| matches!(c, PathCommand::LineTo(_)))
            .count(),
        2,
        "single line crossing redaction box must be split into 2 exterior LineTo segments"
    );

    // 2. Export to PDF and verify both region-scoped verification and survival of the unredacted occurrence
    let exported = export_redacted_document(
        raw_bytes,
        &doc,
        &Metadata::default(),
        &sidecar,
        RedactedExportFormat::Pdf,
        150.0,
    )
    .expect("export_redacted_document must succeed when unredacted duplicate exists outside box");

    verify_redacted_regions(&exported.bytes, Some("pdf"), &exported.log.entries)
        .expect("verify_redacted_regions");

    let reparsed =
        interpret_pdf_bytes(&exported.bytes, &LayoutOptions::default()).expect("reparse pdf");
    let page_text_compact: String = reparsed.pages[0]
        .text()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        page_text_compact.contains("Unredactedwitness:JohnSmith"),
        "unredacted occurrence outside redaction box must be preserved, got: {}",
        reparsed.pages[0].text()
    );
}
