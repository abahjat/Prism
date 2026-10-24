// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end Phase 4 integration tests:
//! 1. Cross-format hidden-content reports (positive and negative fixtures)
//! 2. Unified container & embedded-object family tree with limits and cycle detection
//! 3. Cross-format corpus redaction & byte-level verification (`verify_redacted_output` + CLI)
//! 4. Deterministic Bates-numbered productions, load-file reference parsing, and crash resumption
//! 5. Full-pipeline determinism check and performance budget assertions

use prism_core::document::{ContentBlock, Dimensions, Document, Page, Rect, TextBlock, TextRun};
use prism_core::review::FindingKind;
use prism_core::tree::{ContainerLimits, ContainerNodeStatus};
use prism_layout::{LayoutOptions, SearchOptions, interpret_pdf_bytes, layout_document};
use prism_parsers::review::generate_hidden_content_report;
use prism_parsers::tree::build_family_tree;
use prism_production::{
    ProductionImageFormat, ProductionInput, ProductionOptions, ProductionPdfMode, SlipSheetSpec,
    parse_concordance_dat, parse_csv_load_file, parse_opticon_opt, run_production,
};
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_review::redaction::{
    RedactedExportFormat, export_redacted_document, verify_redacted_output,
};
use prism_review::sidecar::{
    Annotation, AnnotationAnchor, AnnotationKind, AnnotationSidecar, RedactionTarget, TextAnchor,
};
use prism_sandbox::{SandboxConfig, SandboxJob, SandboxManager};
use std::io::Write;
use std::time::Instant;

fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let cursor = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(cursor);
    let opts =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        zip.start_file(*name, opts).expect("start zip entry");
        zip.write_all(data).expect("write zip entry");
    }
    zip.finish().expect("finish zip").into_inner()
}

#[test]
fn hidden_content_positive_and_negative_fixtures_across_formats() {
    // 1. Negative plain-text fixture -> 0 findings
    let clean_txt = b"Clean business memo with no hidden content or hyperlinks.";
    let clean_report = generate_hidden_content_report(clean_txt, Some("clean.txt"), None);
    assert!(clean_report.items.is_empty());
    assert!(!clean_report.coverage.is_empty());

    // 2. Positive DOCX fixture with tracked changes, comments, hidden text, white-on-white text, macro project, and embedded package
    let docx_bytes = make_zip(&[
        (
            "word/document.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
            <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body>
                <w:p>
                  <w:r><w:t>Visible paragraph</w:t></w:r>
                  <w:ins w:id="1" w:author="Alice"><w:r><w:t>Inserted secret</w:t></w:r></w:ins>
                  <w:r><w:rPr><w:vanish/></w:rPr><w:t>Vanished text</w:t></w:r>
                  <w:r><w:rPr><w:color w:val="FFFFFF"/></w:rPr><w:t>White stealth text</w:t></w:r>
                </w:p>
              </w:body>
            </w:document>"#,
        ),
        (
            "word/comments.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
            <w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:comment w:id="0" w:author="Bob"><w:p><w:r><w:t>Review note</w:t></w:r></w:p></w:comment>
            </w:comments>"#,
        ),
        ("word/vbaProject.bin", b"DummyVBAMacroStream"),
        ("word/embeddings/oleObject1.bin", b"EmbeddedOLEPayload"),
    ]);
    let docx_report = generate_hidden_content_report(&docx_bytes, Some("review.docx"), None);
    assert!(docx_report.has_kind(FindingKind::TrackedInsertion));
    assert!(docx_report.has_kind(FindingKind::Comment));
    assert!(docx_report.has_kind(FindingKind::HiddenText));
    assert!(docx_report.has_kind(FindingKind::WhiteOrLowContrastText));
    assert!(docx_report.has_kind(FindingKind::MacroCode));
    assert!(docx_report.has_kind(FindingKind::EmbeddedOleObject));

    // 3. Positive XLSX fixture with hidden sheet, hidden defined name, and hidden row
    let xlsx_bytes = make_zip(&[
        (
            "xl/workbook.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
            <workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
                      xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
              <sheets>
                <sheet name="Summary" sheetId="1" r:id="rId1"/>
                <sheet name="HiddenCalc" sheetId="2" state="veryHidden" r:id="rId2"/>
              </sheets>
              <definedNames>
                <definedName name="SecretRate" hidden="1">Summary!$B$2</definedName>
              </definedNames>
            </workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?>
            <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
              <Relationship Id="rId1" Target="worksheets/sheet1.xml"/>
              <Relationship Id="rId2" Target="worksheets/sheet2.xml"/>
            </Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
            <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
              <sheetData>
                <row r="1" hidden="1"><c r="A1"><f>SUM(10,20)</f><v>30</v></c></row>
              </sheetData>
            </worksheet>"#,
        ),
        (
            "xl/worksheets/sheet2.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
            <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
              <sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Hidden sheet secret</t></is></c></row></sheetData>
            </worksheet>"#,
        ),
    ]);
    let xlsx_report = generate_hidden_content_report(&xlsx_bytes, Some("budget.xlsx"), None);
    assert!(xlsx_report.has_kind(FindingKind::HiddenSheet));
    assert!(xlsx_report.has_kind(FindingKind::HiddenRowsOrColumns));
    assert!(xlsx_report.has_kind(FindingKind::HiddenDefinedName));
}

#[test]
fn cross_format_corpus_redaction_and_verification() {
    let secret = "OMEGA-CLASSIFIED-9941";
    let mut doc = Document::new();
    doc.metadata.title = Some(format!("Report on {secret}"));
    doc.metadata.author = Some(format!("Agent {secret}"));
    let mut page1 = Page::new(1, Dimensions::LETTER);
    let mut tb1 = TextBlock::new(Rect::default());
    tb1.add_run(TextRun::new(format!(
        "Paragraph 1 contains {secret} in the middle of normal business text."
    )));
    page1.add_content(ContentBlock::Text(tb1));

    let mut tb2 = TextBlock::new(Rect::default());
    tb2.add_run(TextRun::new(format!(
        "Paragraph 2 repeats {secret} to test multi-hit corpus redaction."
    )));
    page1.add_content(ContentBlock::Text(tb2));
    doc.pages.push(page1);

    let source_bytes = format!("Source payload with {secret}").into_bytes();
    let layout = layout_document(&doc, &LayoutOptions::default()).expect("layout");
    let hits = layout.find(secret, SearchOptions::default());
    assert_eq!(hits.len(), 2, "Expected 2 search hits for secret");

    let mut sidecar = AnnotationSidecar::new(&source_bytes);
    for (idx, hit) in hits.iter().enumerate() {
        let (anchor, _) = TextAnchor::from_page_quote(&layout, hit.page, secret).expect("anchor");
        sidecar.add(Annotation {
            id: format!("hit-{idx}"),
            kind: AnnotationKind::Redaction {
                reason_code: "CLASSIFIED".to_string(),
                label: "REDACTED".to_string(),
                fill_color: None,
            },
            anchor: AnnotationAnchor {
                target: RedactionTarget::TextRange,
                page_index: hit.page,
                rects_pt: hit
                    .rects
                    .iter()
                    .map(|r| prism_core::review::ReviewRect {
                        x: r.x,
                        y: r.y,
                        width: r.width,
                        height: r.height,
                    })
                    .collect(),
                text_anchor: Some(anchor),
            },
            author: Some("auditor".to_string()),
            created_at: Some("2026-01-01T00:00:00Z".to_string()),
            proposed: false,
        });
    }

    // Export and verify across PDF, TIFF, PNG, SVG, and Text
    for (fmt, hint) in [
        (RedactedExportFormat::Pdf, "pdf"),
        (RedactedExportFormat::Tiff, "tiff"),
        (RedactedExportFormat::Png { page_index: 0 }, "png"),
        (RedactedExportFormat::Svg { page_index: 0 }, "svg"),
        (RedactedExportFormat::Text, "txt"),
    ] {
        let res =
            export_redacted_document(&source_bytes, &layout, &doc.metadata, &sidecar, fmt, 150.0)
                .unwrap_or_else(|e| panic!("Redacted export for {hint} failed: {e}"));

        // Verify again explicitly with `verify_redacted_output`
        verify_redacted_output(&res.bytes, Some(hint), &[secret.to_string()])
            .unwrap_or_else(|e| panic!("Verification failed for {hint}: {e}"));
    }

    // Also test redacting a generated PDF input end-to-end
    let unredacted_pdf =
        export_document_pdf(&layout, &PdfExportOptions::default()).expect("export unredacted pdf");
    let pdf_layout =
        interpret_pdf_bytes(&unredacted_pdf, &LayoutOptions::default()).expect("interpret pdf");
    let mut pdf_sidecar = AnnotationSidecar::new(&unredacted_pdf);
    let (pdf_anchor, pdf_rects) =
        TextAnchor::from_page_quote(&pdf_layout, 0, secret).expect("pdf text anchor");
    pdf_sidecar.add(Annotation {
        id: "pdf-red-1".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "CLASSIFIED".to_string(),
            label: "REDACTED".to_string(),
            fill_color: None,
        },
        anchor: AnnotationAnchor {
            target: RedactionTarget::TextRange,
            page_index: 0,
            rects_pt: pdf_rects,
            text_anchor: Some(pdf_anchor),
        },
        author: None,
        created_at: None,
        proposed: false,
    });
    let redacted_pdf_res = export_redacted_document(
        &unredacted_pdf,
        &pdf_layout,
        &doc.metadata,
        &pdf_sidecar,
        RedactedExportFormat::Pdf,
        150.0,
    )
    .expect("redact pdf");
    verify_redacted_output(&redacted_pdf_res.bytes, Some("pdf"), &[secret.to_string()])
        .expect("verify redacted pdf");
}

fn one_cell_xlsx(a1: &str, b1: &str) -> Vec<u8> {
    let sheet = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
        <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
          <sheetData><row r="1">
            <c r="A1" t="inlineStr"><is><t>{a1}</t></is></c>
            <c r="B1" t="inlineStr"><is><t>{b1}</t></is></c>
          </row></sheetData>
        </worksheet>"#
    );
    make_zip(&[
        (
            "xl/workbook.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
            <workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
                      xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
              <sheets><sheet name="Accounts" sheetId="1" r:id="rId1"/></sheets>
            </workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?>
            <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
              <Relationship Id="rId1" Target="worksheets/sheet1.xml"/>
            </Relationships>"#,
        ),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

#[test]
fn cell_range_redaction_removes_the_value_from_exported_pages() {
    let secret = "Account-998877";
    let xlsx = one_cell_xlsx(secret, "Public figure");
    let wb = prism_excel::parse_xlsx(&xlsx, 16 << 20).expect("parse xlsx");
    let layout = prism_excel::layout_workbook(&wb, &LayoutOptions::default()).expect("layout");
    assert!(!layout.find(secret, SearchOptions::default()).is_empty());

    let mut sidecar = AnnotationSidecar::new(&xlsx);
    sidecar.add(Annotation {
        id: "cell-1".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "PII".to_string(),
            label: "[REDACTED]".to_string(),
            fill_color: None,
        },
        anchor: prism_review::cell_range_anchor("Accounts", 0, 0, 0, 0),
        author: None,
        created_at: None,
        proposed: false,
    });

    for fmt in [RedactedExportFormat::Pdf, RedactedExportFormat::Text] {
        let res = export_redacted_document(
            &xlsx,
            &layout,
            &prism_core::metadata::Metadata::default(),
            &sidecar,
            fmt,
            72.0,
        )
        .expect("export");
        assert!(res.log.redacted_strings.iter().any(|s| s == secret));
        verify_redacted_output(&res.bytes, None, &[secret.to_string()])
            .expect("cell value must not survive the export");
    }

    // A cell-range redaction on a document that is not a spreadsheet is refused, not skipped.
    let text_layout = layout_document(&Document::new(), &LayoutOptions::default()).expect("layout");
    let text_bytes = b"not a workbook".to_vec();
    let mut text_sidecar = AnnotationSidecar::new(&text_bytes);
    text_sidecar.annotations = sidecar.annotations.clone();
    assert!(
        export_redacted_document(
            &text_bytes,
            &text_layout,
            &prism_core::metadata::Metadata::default(),
            &text_sidecar,
            RedactedExportFormat::Text,
            72.0,
        )
        .is_err()
    );
}

#[test]
fn golden_production_loadfiles_resumption_and_determinism() {
    let dir1 = tempfile::tempdir().expect("dir1");
    let dir2 = tempfile::tempdir().expect("dir2");

    let doc1_bytes = b"Parent Email Body discussing Project Helios and attachment.".to_vec();
    let doc2_bytes = b"Attachment contract containing SECRET-CLAUSE-7788 for redaction.".to_vec();
    let doc3_bytes = b"Privileged legal advice memo.".to_vec();

    let mut doc2_sidecar = AnnotationSidecar::new(&doc2_bytes);
    doc2_sidecar.add(Annotation {
        id: "sc-1".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "PRIV".to_string(),
            label: "REDACTED".to_string(),
            fill_color: None,
        },
        anchor: AnnotationAnchor {
            target: RedactionTarget::TextRange,
            page_index: 0,
            rects_pt: Vec::new(),
            text_anchor: Some(TextAnchor {
                start_source: prism_core::review::SourceRef::default(),
                end_source: prism_core::review::SourceRef::default(),
                quoted_text: "SECRET-CLAUSE-7788".to_string(),
                prefix: String::new(),
                suffix: String::new(),
            }),
        },
        author: None,
        created_at: None,
        proposed: false,
    });

    let mut input1 = ProductionInput::new("DOC-001", "email.txt", doc1_bytes);
    input1.custodian = Some("Smith, Jane".to_string());
    input1.confidentiality = Some("CONFIDENTIAL".to_string());

    let mut input2 = ProductionInput::new("DOC-002", "attachment.txt", doc2_bytes);
    input2.parent_doc_id = Some("DOC-001".to_string());
    input2.custodian = Some("Smith, Jane".to_string());
    input2.confidentiality = Some("HIGHLY CONFIDENTIAL".to_string());
    input2.sidecar = Some(doc2_sidecar);

    let mut input3 = ProductionInput::new("DOC-003", "privileged.txt", doc3_bytes);
    input3.custodian = Some("Legal Dept".to_string());
    input3.slip_sheet = Some(SlipSheetSpec::privileged(
        "Withheld for Attorney-Client Privilege",
    ));

    let inputs = vec![input1, input2, input3];

    let mut options = ProductionOptions::default();
    options.bates.prefix = "PRISM".to_string();
    options.bates.start_number = 1001;
    options.bates.padding = 6;
    options.image_format = ProductionImageFormat::None;
    options.pdf_mode = ProductionPdfMode::PerDocument { pdf_a2b: true };
    options.endorsements.footer_left = Some("{confidentiality} | {date}".to_string());

    // Run 1 (partial 2 docs, then resume with all 3 docs)
    let partial_manifest =
        run_production(&inputs[..2], &options, dir1.path()).expect("partial production");
    assert_eq!(partial_manifest.documents.len(), 2);

    let resumed_manifest =
        run_production(&inputs, &options, dir1.path()).expect("resumed production");
    assert_eq!(resumed_manifest.documents.len(), 3);

    // Run 2 (clean full run in dir2)
    let clean_manifest = run_production(&inputs, &options, dir2.path()).expect("clean production");

    // Determinism check: resumed_manifest and clean_manifest must be identical
    assert_eq!(resumed_manifest, clean_manifest);
    assert_eq!(resumed_manifest.documents[0].begin_bates, "PRISM001001");
    assert_eq!(resumed_manifest.documents[0].begin_attach, "PRISM001001");
    assert_eq!(resumed_manifest.documents[0].end_attach, "PRISM001002");
    assert_eq!(resumed_manifest.documents[1].begin_bates, "PRISM001002");

    // Verify redacted string is absent from produced PDF and text for DOC-002
    let doc2_pdf = std::fs::read(dir2.path().join("IMAGES/PRISM001002.pdf")).unwrap();
    let doc2_txt = std::fs::read(dir2.path().join("TEXT/PRISM001002.txt")).unwrap();
    verify_redacted_output(&doc2_pdf, Some("pdf"), &["SECRET-CLAUSE-7788".to_string()]).unwrap();
    verify_redacted_output(&doc2_txt, Some("txt"), &["SECRET-CLAUSE-7788".to_string()]).unwrap();

    // Verify load files parse in reference readers
    let dat_str =
        std::fs::read_to_string(dir2.path().join("LOADFILES/VOL001.dat")).expect("read dat");
    let opt_str =
        std::fs::read_to_string(dir2.path().join("LOADFILES/VOL001.opt")).expect("read opt");
    let csv_str =
        std::fs::read_to_string(dir2.path().join("LOADFILES/VOL001.csv")).expect("read csv");

    let dat_rows = parse_concordance_dat(&dat_str, '\u{0014}', '\u{00FE}', '\u{00AE}').unwrap();
    let opt_rows = parse_opticon_opt(&opt_str).unwrap();
    let csv_rows = parse_csv_load_file(&csv_str).unwrap();

    assert_eq!(dat_rows.len(), 3);
    assert_eq!(opt_rows.len(), 3);
    assert_eq!(csv_rows.len(), 3);
}

#[test]
fn interrupted_production_resumes_and_redacted_natives_are_withheld() {
    let dir = tempfile::tempdir().expect("dir");
    let options = ProductionOptions {
        image_format: ProductionImageFormat::None,
        pdf_mode: ProductionPdfMode::PerDocument { pdf_a2b: false },
        ..ProductionOptions::default()
    };

    let first = ProductionInput::new("DOC-001", "memo.txt", b"First memo body.".to_vec());

    // The second input carries a cell redaction it cannot honour, so the run stops there.
    let bad_bytes = b"Not a spreadsheet.".to_vec();
    let mut bad_sidecar = AnnotationSidecar::new(&bad_bytes);
    bad_sidecar.add(Annotation {
        id: "cell".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "PII".to_string(),
            label: "REDACTED".to_string(),
            fill_color: None,
        },
        anchor: prism_review::cell_range_anchor("Sheet1", 0, 0, 0, 0),
        author: None,
        created_at: None,
        proposed: false,
    });
    let mut bad = ProductionInput::new("DOC-002", "notes.txt", bad_bytes);
    bad.sidecar = Some(bad_sidecar);

    assert!(run_production(&[first.clone(), bad], &options, dir.path()).is_err());
    let partial: prism_production::ProductionManifest = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("manifest.json")).expect("manifest written"),
    )
    .expect("manifest parses");
    assert_eq!(partial.documents.len(), 1);
    assert_eq!(partial.documents[0].doc_id, "DOC-001");

    // A native-only slip sheet must not ship the native of a document marked for redaction.
    let secret_bytes = b"Contains SECRET-NATIVE-4411.".to_vec();
    let mut secret_sidecar = AnnotationSidecar::new(&secret_bytes);
    secret_sidecar.add(Annotation {
        id: "txt".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "PII".to_string(),
            label: "REDACTED".to_string(),
            fill_color: None,
        },
        anchor: AnnotationAnchor {
            target: RedactionTarget::WholePage { page_index: 0 },
            page_index: 0,
            rects_pt: Vec::new(),
            text_anchor: None,
        },
        author: None,
        created_at: None,
        proposed: false,
    });
    let mut native_only = ProductionInput::new("DOC-002", "secret.txt", secret_bytes);
    native_only.sidecar = Some(secret_sidecar);
    native_only.slip_sheet = Some(SlipSheetSpec::native_only());

    let resumed = run_production(&[first, native_only], &options, dir.path()).expect("resume");
    assert_eq!(resumed.documents.len(), 2);
    assert!(resumed.documents[1].native_path.is_none());
    assert!(!dir.path().join("NATIVES").exists());
}

#[test]
fn container_tree_limits_and_performance_budgets() {
    // 1. Nested ZIP family tree with member-count limit
    let inner_zip = make_zip(&[("inner1.txt", b"Hello 1"), ("inner2.txt", b"Hello 2")]);
    let outer_zip = make_zip(&[("nested.zip", &inner_zip), ("readme.txt", b"Top level")]);

    let limits = ContainerLimits {
        max_depth: 4,
        max_total_bytes: 10 * 1024 * 1024,
        max_members: 2,
    };
    let tree = build_family_tree(&outer_zip, Some("outer.zip"), limits);
    assert!(!tree.root.children.is_empty());
    let mut statuses = Vec::new();
    fn collect_statuses(
        node: &prism_core::tree::ContainerNode,
        out: &mut Vec<ContainerNodeStatus>,
    ) {
        out.push(node.status.clone());
        for c in &node.children {
            collect_statuses(c, out);
        }
    }
    collect_statuses(&tree.root, &mut statuses);
    assert!(
        statuses
            .iter()
            .any(|s| matches!(s, ContainerNodeStatus::MemberLimitReached { .. }))
    );

    // 2. Performance budget: 300-paragraph DOCX hidden-content report < 1.5 s
    let mut body_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for i in 0..300 {
        use std::fmt::Write as _;
        let _ = write!(
            body_xml,
            "<w:p><w:r><w:t>Business paragraph {i} with standard contract prose for performance benchmarking.</w:t></w:r></w:p>"
        );
    }
    body_xml.push_str("</w:body></w:document>");
    let big_docx = make_zip(&[("word/document.xml", body_xml.as_bytes())]);

    let start_report = Instant::now();
    let report = generate_hidden_content_report(&big_docx, Some("perf.docx"), None);
    let report_elapsed = start_report.elapsed();
    assert!(report.items.is_empty());
    assert!(
        report_elapsed.as_millis() < 1500,
        "Hidden-content report took {report_elapsed:?} (budget < 1500ms)"
    );

    // 3. Warm worker pool overhead smoke check
    let pool = SandboxManager::new(SandboxConfig::default());
    // Warm up worker
    let _ = pool
        .run(SandboxJob::Detect {
            filename: Some("warmup.txt".to_string()),
            bytes: b"Warmup payload".to_vec(),
        })
        .expect("warmup job");

    let start_warm = Instant::now();
    let out = pool
        .run(SandboxJob::Detect {
            filename: Some("fast.txt".to_string()),
            bytes: b"Fast warm worker test".to_vec(),
        })
        .expect("warm job");
    let warm_elapsed = start_warm.elapsed();
    assert!(!out.data.is_empty());
    assert!(
        warm_elapsed.as_millis() < 200,
        "Warm worker job took {warm_elapsed:?}"
    );
}
