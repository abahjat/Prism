// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_core::ENGINE_VERSION;
use prism_core::document::{ContentBlock, Dimensions, Document, Page, Rect, TextBlock, TextRun};
use prism_core::review::ReviewRect;
use prism_layout::{LayoutOptions, layout_document};
use prism_review::redaction::{apply_redactions, verify_redacted_output};
use prism_review::sidecar::{
    Annotation, AnnotationAnchor, AnnotationKind, AnnotationSidecar, RedactionTarget,
};

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    let text = String::from_utf8_lossy(data);
    let mut doc = Document::new();
    let mut page = Page::new(1, Dimensions::LETTER);
    let mut tb = TextBlock::new(Rect::default());
    tb.add_run(TextRun::new(text.as_ref()));
    page.add_content(ContentBlock::Text(tb));
    doc.pages.push(page);

    if let Ok(mut layout) = layout_document(&doc, &LayoutOptions::default()) {
        let mut sidecar = AnnotationSidecar::new(data);
        sidecar.add(Annotation {
            id: "fuzz-r1".to_string(),
            kind: AnnotationKind::Redaction {
                reason_code: "FUZZ".to_string(),
                label: "REDACTED".to_string(),
                fill_color: None,
            },
            anchor: AnnotationAnchor {
                target: RedactionTarget::PageRect,
                page_index: 0,
                rects_pt: vec![ReviewRect {
                    x: 50.0,
                    y: 50.0,
                    width: 200.0,
                    height: 80.0,
                }],
                text_anchor: None,
            },
            author: None,
            created_at: None,
            proposed: false,
        });
        if let Ok(resolved) = sidecar.resolve_against(data, ENGINE_VERSION, &layout) {
            if let Ok(log) = apply_redactions(&mut layout, &sidecar.document_sha256, &resolved) {
                let page_text = layout.pages.first().map(|p| p.text()).unwrap_or_default();
                let _ = verify_redacted_output(
                    page_text.as_bytes(),
                    Some("txt"),
                    &log.redacted_strings,
                );
            }
        }
    }
});
