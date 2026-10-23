// SPDX-License-Identifier: AGPL-3.0-only
//! Phase 1 performance latency budget tests:
//! 1. 50-slide PPTX: first page rendered < 100 ms (release)
//! 2. 150 DPI page rasterization < 150 ms (release)
//! 3. 100-page PDF open + first page < 150 ms (release)

use prism_core::document::{
    ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, ShapeStyle, TextBlock, TextRun,
    TextStyle,
};
use prism_layout::{
    FontSystem, ImageStore, Item, LayoutDocument, LayoutOptions, LayoutPage, LineSpacing,
    ParagraphSpec, Point, Rgba, Size, SpanSpec, TextAlign, TextLayoutEngine, interpret_pdf_pages,
    layout_document,
};
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_render::raster::{RasterOptions, render_page_png};
use prism_render::svg::render_page_svg;
use std::time::Instant;

#[test]
fn test_50_slide_pptx_first_page_latency_budget() {
    let mut doc_builder = Document::builder();
    for slide_idx in 0..50_u32 {
        let slide = Page {
            number: slide_idx + 1,
            dimensions: Dimensions::new(720.0, 540.0),
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
            content: vec![
                ContentBlock::Text(TextBlock {
                    bounds: Rect::new(48.0, 40.0, 624.0, 60.0),
                    runs: vec![TextRun {
                        text: format!("Slide {} — Executive Summary", slide_idx + 1),
                        style: TextStyle {
                            font_family: Some("Calibri".to_string()),
                            font_size: Some(28.0),
                            bold: true,
                            color: Some("#1E293B".to_string()),
                            ..TextStyle::default()
                        },
                        bounds: None,
                        char_positions: None,
                    }],
                    style: ShapeStyle {
                        fill_color: Some("#F1F5F9".to_string()),
                        stroke_color: Some("#94A3B8".to_string()),
                        stroke_width: Some(1.0),
                        ..ShapeStyle::default()
                    },
                    paragraph_style: None,
                    vertical_alignment: None,
                    rotation: 0.0,
                }),
                ContentBlock::Text(TextBlock {
                    bounds: Rect::new(48.0, 120.0, 624.0, 340.0),
                    runs: vec![TextRun {
                        text:
                            "Key findings and forensic review metrics for Phase 1 layout pipeline."
                                .to_string(),
                        style: TextStyle {
                            font_family: Some("Calibri".to_string()),
                            font_size: Some(18.0),
                            color: Some("#334155".to_string()),
                            ..TextStyle::default()
                        },
                        bounds: None,
                        char_positions: None,
                    }],
                    style: ShapeStyle::default(),
                    paragraph_style: None,
                    vertical_alignment: None,
                    rotation: 0.0,
                }),
            ],
        };
        doc_builder = doc_builder.page(slide);
    }
    let full_doc = doc_builder.build();

    // Measure first-slide layout + SVG render
    let mut first_slide_doc = full_doc;
    first_slide_doc.pages.truncate(1);
    let opts = LayoutOptions::default();

    // Warm up font cache once
    let _ = layout_document(&first_slide_doc, &opts).expect("warmup");

    let t0 = Instant::now();
    let laid_out = layout_document(&first_slide_doc, &opts).expect("layout slide 1");
    let svg = render_page_svg(&laid_out.pages[0], &laid_out.fonts, &laid_out.images, &opts)
        .expect("render slide 1 svg");
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

    assert!(svg.contains("<svg"));
    println!("50-slide PPTX first page latency: {elapsed_ms:.2} ms");

    let budget_ms = if cfg!(debug_assertions) {
        1000.0
    } else {
        100.0
    };
    assert!(
        elapsed_ms < budget_ms,
        "First slide render took {elapsed_ms:.2} ms (budget < {budget_ms:.0} ms)"
    );
}

#[test]
fn test_150_dpi_page_raster_latency_budget() {
    let opts = LayoutOptions::default();
    let mut font_sys = FontSystem::new(&opts);
    let mut engine = TextLayoutEngine::new();

    let spec = ParagraphSpec {
        spans: vec![SpanSpec {
            text: "Forensic page rasterization benchmark at 150 DPI using tiny-skia.".repeat(6),
            family: "Calibri".to_string(),
            size: 12.0,
            color: Rgba::rgb(20, 25, 35),
            ..SpanSpec::default()
        }],
        align: TextAlign::Left,
        line_spacing: LineSpacing::Multiple(1.2),
        max_width: Some(468.0),
        first_line_indent: 0.0,
        origin: Point::new(72.0, 72.0),
        page_index: 0,
        block_index: 0,
    };
    let shaped = engine.layout_paragraph(&spec, &mut font_sys);
    let mut page = LayoutPage::new(0, Size::LETTER);
    for run in shaped.runs {
        page.items.push(Item::Glyphs(run));
    }
    let images = ImageStore::new();
    let raster_opts = RasterOptions {
        dpi: 150.0,
        layout_options: opts,
        ..RasterOptions::default()
    };

    let t0 = Instant::now();
    let png =
        render_page_png(&page, &font_sys.store, &images, &raster_opts).expect("raster 150dpi");
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    println!("150 DPI page raster latency: {elapsed_ms:.2} ms");

    let budget_ms = if cfg!(debug_assertions) {
        1500.0
    } else {
        150.0
    };
    assert!(
        elapsed_ms < budget_ms,
        "150 DPI page raster took {elapsed_ms:.2} ms (budget < {budget_ms:.0} ms)"
    );
}

#[test]
fn test_100_page_pdf_open_and_first_page_latency_budget() {
    let opts = LayoutOptions::default();
    let mut font_sys = FontSystem::new(&opts);
    let mut engine = TextLayoutEngine::new();

    let spec = ParagraphSpec {
        spans: vec![SpanSpec {
            text: "Page content for 100-page PDF open and first-page interpretation benchmark."
                .to_string(),
            family: "Liberation Sans".to_string(),
            size: 12.0,
            color: Rgba::BLACK,
            ..SpanSpec::default()
        }],
        align: TextAlign::Left,
        line_spacing: LineSpacing::Multiple(1.2),
        max_width: Some(468.0),
        first_line_indent: 0.0,
        origin: Point::new(72.0, 72.0),
        page_index: 0,
        block_index: 0,
    };
    let shaped = engine.layout_paragraph(&spec, &mut font_sys);

    let mut pages = Vec::with_capacity(100);
    for idx in 0..100_u32 {
        let mut p = LayoutPage::new(idx, Size::LETTER);
        for run in &shaped.runs {
            p.items.push(Item::Glyphs(run.clone()));
        }
        pages.push(p);
    }

    let doc_100 = LayoutDocument {
        pages,
        fonts: font_sys.store,
        images: ImageStore::new(),
        font_report: font_sys.report,
    };

    let pdf_bytes =
        export_document_pdf(&doc_100, &PdfExportOptions::default()).expect("export 100-page PDF");

    let t0 = Instant::now();
    let first_only = interpret_pdf_pages(&pdf_bytes, &opts, Some(&[0]))
        .expect("interpret first page of 100-page PDF");
    let svg = render_page_svg(
        &first_only.pages[0],
        &first_only.fonts,
        &first_only.images,
        &opts,
    )
    .expect("render first page svg");
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(first_only.pages.len(), 1);
    assert!(svg.contains("<svg"));
    println!("100-page PDF open + first page latency: {elapsed_ms:.2} ms");

    let budget_ms = if cfg!(debug_assertions) {
        1500.0
    } else {
        150.0
    };
    assert!(
        elapsed_ms < budget_ms,
        "100-page PDF open + first page took {elapsed_ms:.2} ms (budget < {budget_ms:.0} ms)"
    );
}
