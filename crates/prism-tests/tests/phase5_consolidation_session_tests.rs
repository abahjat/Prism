// SPDX-License-Identifier: AGPL-3.0-only
//! Phase 5 integration tests:
//! - M5.1: Unified pipeline engine (`prism_parsers::pipeline`) & Office UDM consolidation
//! - M5.2: Paginated layout, search, redaction, and Bates production for Email (`EML`, `MSG`, `MBOX`) & `HTML`/`MHT`
//! - M5.3: Stateful worker sessions (`SandboxManager::open_session`, `SandboxSession`)

use prism_core::review::ReviewRect;
use prism_layout::{LayoutOptions, SearchOptions};
use prism_parsers::pipeline::{default_registry, open_and_layout, parse_udm_bytes};
use prism_production::{
    BatesConfig, ProducedDocumentStatus, ProductionImageFormat, ProductionInput, ProductionOptions,
    ProductionPdfMode, run_production,
};
use prism_review::{
    Annotation, AnnotationAnchor, AnnotationKind, AnnotationSidecar, RedactedExportFormat,
    RedactionTarget, TextAnchor, export_redacted_document,
};
use prism_sandbox::{RenderOutputFormat, SandboxConfig, SandboxManager};
use std::io::{Cursor, Write};

fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::FileOptions::default();
        for (name, content) in files {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap();
    }
    cursor.into_inner()
}

fn build_msg_cfb(subject: &str, sender: &str, to: &str, html_body: &str) -> Vec<u8> {
    fn encode_utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }
    let cursor = Cursor::new(Vec::new());
    let mut comp = cfb::CompoundFile::create(cursor).unwrap();
    comp.create_stream("/__substg1.0_0037001F")
        .unwrap()
        .write_all(&encode_utf16le(subject))
        .unwrap();
    comp.create_stream("/__substg1.0_0C1A001F")
        .unwrap()
        .write_all(&encode_utf16le(sender))
        .unwrap();
    comp.create_stream("/__substg1.0_0E04001F")
        .unwrap()
        .write_all(&encode_utf16le(to))
        .unwrap();
    comp.create_stream("/__substg1.0_1013001F")
        .unwrap()
        .write_all(&encode_utf16le(html_body))
        .unwrap();
    comp.into_inner().into_inner()
}

#[test]
fn m5_1_unified_pipeline_and_office_udm_consolidation() {
    let opts = LayoutOptions::default();

    // 1. Shared registry is a singleton
    let reg1 = default_registry() as *const _;
    let reg2 = default_registry() as *const _;
    assert_eq!(reg1, reg2);

    // 2. PPTX via unified pipeline and UDM
    let pptx_bytes = build_zip(&[
        (
            "ppt/presentation.xml",
            br#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
        ),
        (
            "ppt/slides/slide1.xml",
            br#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree><p:sp><p:spPr><a:xfrm><a:off x="457200" y="457200"/><a:ext cx="4572000" cy="914400"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>Unified Slide Deck</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
        ),
    ]);
    let opened_pptx = open_and_layout(&pptx_bytes, Some("deck.pptx"), &opts).unwrap();
    assert!(opened_pptx.presentation.is_some());
    assert_eq!(opened_pptx.layout.pages.len(), 1);
    assert!(
        opened_pptx.layout.pages[0]
            .text()
            .contains("Unified Slide Deck")
    );

    let (_, pptx_udm) =
        parse_udm_bytes(&pptx_bytes, Some("deck.pptx"), Default::default()).unwrap();
    assert!(pptx_udm.extract_text().contains("Unified Slide Deck"));

    // 3. XLSX via unified pipeline and UDM
    let xlsx_bytes = build_zip(&[
        (
            "xl/workbook.xml",
            br#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Ledger" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            br#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Quarter</t></is></c><c r="B1" t="inlineStr"><is><t>Q4-2026</t></is></c></row></sheetData></worksheet>"#,
        ),
    ]);
    let opened_xlsx = open_and_layout(&xlsx_bytes, Some("ledger.xlsx"), &opts).unwrap();
    assert!(opened_xlsx.workbook.is_some());
    assert!(opened_xlsx.layout.pages[0].text().contains("Q4-2026"));

    let (_, xlsx_udm) =
        parse_udm_bytes(&xlsx_bytes, Some("ledger.xlsx"), Default::default()).unwrap();
    assert!(xlsx_udm.extract_text().contains("Q4-2026"));
}

#[test]
fn m5_2_email_and_html_paginated_layout_redaction_and_production() {
    let opts = LayoutOptions::default();

    // 1. Multi-page EML email with long body + privileged token
    let mut eml = String::from(
        "From: Alice Counsel <alice@firm.example>\r\n\
         To: Bob Executive <bob@corp.example>\r\n\
         Cc: Audit Team <audit@corp.example>\r\n\
         Subject: Privileged M&A Valuation Review\r\n\
         Date: Mon, 20 Oct 2026 10:30:00 +0000\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n\
         PRIVILEGED TOKEN: PROJECT-ORION-9941\r\n\r\n",
    );
    for i in 1..=70 {
        eml.push_str(&format!(
            "Paragraph {i}: This email paragraph discusses the valuation model, regulatory covenants, and closing timeline for the transaction across multiple lines.\r\n\r\n"
        ));
    }
    let eml_bytes = eml.into_bytes();

    let opened_eml = open_and_layout(&eml_bytes, Some("review.eml"), &opts).unwrap();
    assert!(
        opened_eml.layout.pages.len() >= 2,
        "Expected EML to paginate across >= 2 pages, got {}",
        opened_eml.layout.pages.len()
    );
    let eml_hits = opened_eml
        .layout
        .find("PROJECT-ORION-9941", SearchOptions::default());
    assert_eq!(eml_hits.len(), 1);
    assert!(!eml_hits[0].rects.is_empty());

    // 2. Outlook MSG with HTML body and table
    let msg_html = "<html><body>\
        <h2>Board Resolution Summary</h2>\
        <p>Confidential code: <b>MSG-SECRET-4410</b></p>\
        <table><tr><th>Director</th><th>Vote</th></tr><tr><td>Jane Doe</td><td>Approved</td></tr></table>\
        </body></html>";
    let msg_bytes = build_msg_cfb(
        "Board Resolution",
        "chair@corp.example",
        "board@corp.example",
        msg_html,
    );
    let opened_msg = open_and_layout(&msg_bytes, Some("board.msg"), &opts).unwrap();
    assert!(!opened_msg.layout.pages.is_empty());
    let msg_page0_text = opened_msg.layout.pages[0].text();
    assert!(msg_page0_text.contains("Board Resolution"));
    assert!(msg_page0_text.contains("MSG-SECRET-4410"));
    assert!(msg_page0_text.contains("Jane Doe"));

    // Destructively redact MSG-SECRET-4410 from the MSG document and verify
    let (ta, rects) =
        TextAnchor::from_page_quote(&opened_msg.layout, 0, "MSG-SECRET-4410").unwrap();
    let mut msg_sidecar = AnnotationSidecar::new(&msg_bytes);
    msg_sidecar.add(Annotation {
        id: "red-msg-1".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "PRIV".to_string(),
            label: "REDACTED".to_string(),
            fill_color: None,
        },
        anchor: AnnotationAnchor {
            target: RedactionTarget::TextRange,
            page_index: 0,
            rects_pt: rects
                .into_iter()
                .map(|r| ReviewRect::new(r.x, r.y, r.width, r.height))
                .collect(),
            text_anchor: Some(ta),
        },
        author: None,
        created_at: None,
        proposed: false,
    });
    let redacted_msg = export_redacted_document(
        &msg_bytes,
        &opened_msg.layout,
        &opened_msg.metadata,
        &msg_sidecar,
        RedactedExportFormat::Pdf,
        150.0,
    )
    .unwrap();
    assert_eq!(redacted_msg.log.total_applied, 1);

    // 3. MBOX, HTML, and MHT in Bates production (must all complete as paginated images/PDF, not slip-sheeted!)
    let mbox_bytes = b"From sender@example.com Mon Jan 01 00:00:00 2026\n\
From: sender@example.com\n\
To: team@example.com\n\
Subject: Thread Message 1\n\n\
First message body in mailbox.\n\n\
From sender2@example.com Mon Jan 01 01:00:00 2026\n\
From: sender2@example.com\n\
To: team@example.com\n\
Subject: Thread Message 2\n\n\
Second message body in mailbox.\n"
        .to_vec();

    let html_bytes = b"<!DOCTYPE html><html><head><title>Web Exhibit</title></head><body>\
        <h1>Exhibit A: Terms of Service</h1>\
        <p>Section 1: All disputes shall be resolved in arbitration.</p>\
        <hr/>\
        <ul><li>Clause Alpha</li><li>Clause Beta</li></ul>\
        </body></html>"
        .to_vec();

    let mht_bytes = b"From: <Saved by Blink>\r\n\
Subject: Archived Web Page\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"----=_Part_1\"\r\n\r\n\
------=_Part_1\r\n\
Content-Type: text/html; charset=\"utf-8\"\r\n\r\n\
<html><body><h1>Archived Portal Page</h1><p>Portal snapshot content for eDiscovery production.</p></body></html>\r\n\
------=_Part_1--\r\n"
        .to_vec();

    let out_dir = std::env::temp_dir().join("prism_phase5_email_html_prod");
    let _ = std::fs::remove_dir_all(&out_dir);

    let mut eml_input = ProductionInput::new("DOC-EML", "review.eml", eml_bytes);
    eml_input.custodian = Some("Alice".to_string());
    let mut msg_input = ProductionInput::new("DOC-MSG", "board.msg", msg_bytes);
    msg_input.sidecar = Some(msg_sidecar);
    let mbox_input = ProductionInput::new("DOC-MBOX", "archive.mbox", mbox_bytes);
    let html_input = ProductionInput::new("DOC-HTML", "exhibit.html", html_bytes);
    let mht_input = ProductionInput::new("DOC-MHT", "snapshot.mht", mht_bytes);

    let prod_opts = ProductionOptions {
        volume_name: "VOL_PHASE5".to_string(),
        bates: BatesConfig {
            prefix: "P5_".to_string(),
            start_number: 1,
            padding: 6,
            suffix: String::new(),
        },
        image_format: ProductionImageFormat::SinglePageTiff,
        pdf_mode: ProductionPdfMode::PerDocument { pdf_a2b: true },
        dpi: 100.0,
        production_date: "2026-10-24".to_string(),
        ..ProductionOptions::default()
    };

    let manifest = run_production(
        &[eml_input, msg_input, mbox_input, html_input, mht_input],
        &prod_opts,
        &out_dir,
    )
    .expect("run_production on email & HTML formats");

    assert_eq!(manifest.documents.len(), 5);
    for rec in &manifest.documents {
        assert_eq!(
            rec.status,
            ProducedDocumentStatus::Completed,
            "Document {} ({}) should be Completed, got {:?}",
            rec.doc_id,
            rec.file_name,
            rec.status
        );
    }
    assert!(
        manifest.documents[0].page_count >= 2,
        "Multi-page EML should produce >= 2 Bates pages"
    );
    assert!(
        manifest.documents[1].redacted,
        "MSG with redaction sidecar must be marked redacted"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn m5_3_stateful_worker_session_lifecycle() {
    let manager = SandboxManager::new(SandboxConfig {
        max_memory: 256 * 1024 * 1024,
        ..SandboxConfig::default()
    });

    let eml_bytes = b"From: investigator@agency.example\r\n\
To: counsel@firm.example\r\n\
Subject: Stateful Worker Session Test\r\n\r\n\
This email body is parsed once inside prism-worker and queried across multiple stateful session calls. \
Key identifier: SESSION-KEY-8821.\r\n"
        .to_vec();

    let mut session = manager
        .open_session(eml_bytes, Some("session_test.eml".to_string()), None)
        .expect("open_session");

    assert_eq!(session.page_count(), 1);
    let (w, h) = session.page_dimensions(0).expect("page 0 dimensions");
    assert!(w > 100.0 && h > 100.0);

    // Render SVG from cached session
    let svg_out = session
        .render_page(0, RenderOutputFormat::Svg, 96.0)
        .expect("render SVG");
    assert_eq!(svg_out.content_type, "image/svg+xml");
    assert!(String::from_utf8_lossy(&svg_out.data).contains("<svg"));

    // Render PNG from cached session
    let png_out = session
        .render_page(0, RenderOutputFormat::Png, 72.0)
        .expect("render PNG");
    assert_eq!(png_out.content_type, "image/png");
    assert!(png_out.data.starts_with(b"\x89PNG\r\n\x1a\n"));

    // Render ViewerFragment from cached session
    let frag_out = session
        .render_page(0, RenderOutputFormat::ViewerFragment, 96.0)
        .expect("render ViewerFragment");
    assert!(String::from_utf8_lossy(&frag_out.data).contains("prism-page"));

    // Extract page text from cached session
    let text = session.extract_page_text(0).expect("extract_page_text");
    assert!(text.contains("Stateful Worker Session Test"));
    assert!(text.contains("SESSION-KEY-8821"));

    // Search cached session
    let hits = session
        .search("SESSION-KEY-8821", SearchOptions::default())
        .expect("search session");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].page, 0);
    assert!(!hits[0].rects.is_empty());

    session.close().expect("close session");
}
