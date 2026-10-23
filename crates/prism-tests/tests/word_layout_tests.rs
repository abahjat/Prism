// SPDX-License-Identifier: AGPL-3.0-only
//! Phase 2 (`prism-word`) corpus, determinism, markup-mode, and regression tests.

use prism_layout::{LayoutOptions, MarkupMode};
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_render::raster::{RasterOptions, render_page_png};
use prism_render::svg::{SvgViewerOptions, render_document_viewer_html, render_page_svg};
use prism_word::{
    DEFAULT_MAX_DOC_STREAM_BYTES, DEFAULT_MAX_ENTRY_BYTES, is_doc_bytes, is_docx_zip,
    layout_word_document, parse_doc, parse_docx,
};
use std::path::Path;
use walkdir::WalkDir;

#[test]
fn corpus_docx_and_doc_files_layout_deterministically() {
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
        if ext != "docx" && ext != "doc" {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }

        let parsed = if ext == "docx" && is_docx_zip(&bytes) {
            parse_docx(&bytes, None, DEFAULT_MAX_ENTRY_BYTES)
        } else if is_doc_bytes(&bytes) {
            parse_doc(&bytes, DEFAULT_MAX_DOC_STREAM_BYTES)
        } else {
            continue;
        };

        let Ok(word_doc) = parsed else {
            continue;
        };

        let layout_a = layout_word_document(&word_doc, &options).expect("first layout pass");
        let layout_b = layout_word_document(&word_doc, &options).expect("second layout pass");
        assert_eq!(layout_a.pages.len(), layout_b.pages.len());

        if let (Some(p1), Some(p2)) = (layout_a.pages.first(), layout_b.pages.first()) {
            let svg_a =
                render_page_svg(p1, &layout_a.fonts, &layout_a.images, &options).expect("svg a");
            let svg_b =
                render_page_svg(p2, &layout_b.fonts, &layout_b.images, &options).expect("svg b");
            assert_eq!(svg_a, svg_b, "Non-deterministic SVG output for {:?}", path);

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
        assert_eq!(pdf_a, pdf_b, "Non-deterministic PDF output for {:?}", path);

        let viewer_html =
            render_document_viewer_html(&layout_a, &SvgViewerOptions::default()).expect("viewer");
        assert!(viewer_html.contains("prism-markup-select"));

        for mode in [
            MarkupMode::AllMarkup,
            MarkupMode::SimpleMarkup,
            MarkupMode::NoMarkup,
            MarkupMode::Original,
        ] {
            let mut mode_opts = options.clone();
            mode_opts.markup_mode = mode;
            let mode_layout =
                layout_word_document(&word_doc, &mode_opts).expect("markup mode layout");
            assert!(!mode_layout.pages.is_empty());
        }

        tested_count += 1;
    }

    assert!(
        tested_count > 0,
        "Expected at least one DOCX/DOC file in test-files/"
    );
}
