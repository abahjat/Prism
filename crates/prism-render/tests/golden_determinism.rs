// SPDX-License-Identifier: AGPL-3.0-only
//! Golden-file determinism tests for SVG, HTML viewer, and PNG raster output backends.

use prism_layout::{
    Affine, AnchorItem, ClipPath, FillPaint, FillRule, FontSystem, GradientStop, GroupItem,
    ImageItem, ImageStore, Item, LayoutDocument, LayoutOptions, LayoutPage, LineSpacing,
    LinearGradient, ParagraphSpec, PathCommand, PathItem, Point, Rect, Rgba, ScriptPosition, Size,
    SpanSpec, StrokeStyle, TextAlign, TextLayoutEngine,
};
use prism_render::raster::{RasterOptions, render_page_png};
use prism_render::svg::{SvgViewerOptions, render_document_viewer_html, render_page_svg};

fn build_reference_document() -> LayoutDocument {
    let options = LayoutOptions::default();
    let mut font_sys = FontSystem::new(&options);
    let mut engine = TextLayoutEngine::new();
    let mut images = ImageStore::new();

    // Register a deterministic 4x4 RGBA8 checkerboard image
    let mut img_bytes = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4u8 {
        for x in 0..4u8 {
            if (x + y) % 2 == 0 {
                img_bytes.extend_from_slice(&[0x1F, 0x4E, 0x79, 0xFF]);
            } else {
                img_bytes.extend_from_slice(&[0xE8, 0xF1, 0xFA, 0xFF]);
            }
        }
    }
    let img_id = images
        .register("image/x-rgba8", 4, 4, img_bytes, &options)
        .expect("register image");

    let mut page = LayoutPage::new(0, Size::new(400.0, 300.0));

    // 1. Header banner with linear gradient and stroke
    page.items.push(Item::Path(PathItem {
        commands: vec![
            PathCommand::MoveTo(Point::new(20.0, 20.0)),
            PathCommand::LineTo(Point::new(380.0, 20.0)),
            PathCommand::LineTo(Point::new(380.0, 60.0)),
            PathCommand::LineTo(Point::new(20.0, 60.0)),
            PathCommand::Close,
        ],
        fill: Some(FillPaint::LinearGradient(LinearGradient {
            start: Point::new(20.0, 20.0),
            end: Point::new(380.0, 60.0),
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Rgba::rgb(0xF0, 0xF4, 0xF8),
                },
                GradientStop {
                    offset: 1.0,
                    color: Rgba::rgb(0xD9, 0xE2, 0xEC),
                },
            ],
        })),
        fill_rule: FillRule::NonZero,
        stroke: Some(StrokeStyle {
            color: Rgba::rgb(0x33, 0x4E, 0x68),
            width: 1.0,
            ..StrokeStyle::default()
        }),
        transform: Affine::IDENTITY,
    }));

    // 2. Shaped title paragraph (Calibri -> Carlito)
    let title_spec = ParagraphSpec {
        spans: vec![
            SpanSpec {
                text: "Prism Phase 1 ".to_string(),
                family: "Calibri".to_string(),
                size: 16.0,
                bold: true,
                color: Rgba::rgb(0x10, 0x2A, 0x43),
                run_index: 0,
                ..SpanSpec::default()
            },
            SpanSpec {
                text: "Golden Page".to_string(),
                family: "Times New Roman".to_string(),
                size: 16.0,
                italic: true,
                underline: true,
                color: Rgba::rgb(0x24, 0x3B, 0x53),
                run_index: 1,
                ..SpanSpec::default()
            },
        ],
        align: TextAlign::Left,
        line_spacing: LineSpacing::Multiple(1.15),
        max_width: Some(340.0),
        first_line_indent: 0.0,
        origin: Point::new(30.0, 28.0),
        page_index: 0,
        block_index: 0,
    };
    let shaped_title = engine.layout_paragraph(&title_spec, &mut font_sys);
    for run in shaped_title.runs {
        page.items.push(Item::Glyphs(run));
    }

    // 3. Body paragraph with monospace, superscript, strikethrough, and highlight
    let body_spec = ParagraphSpec {
        spans: vec![
            SpanSpec {
                text: "Forensic determinism: ".to_string(),
                family: "Arial".to_string(),
                size: 12.0,
                run_index: 0,
                ..SpanSpec::default()
            },
            SpanSpec {
                text: "SHA-256".to_string(),
                family: "Courier New".to_string(),
                size: 12.0,
                bold: true,
                highlight: Some(Rgba::rgb(0xFF, 0xF3, 0xC4)),
                run_index: 1,
                ..SpanSpec::default()
            },
            SpanSpec {
                text: "[1]".to_string(),
                family: "Arial".to_string(),
                size: 12.0,
                script: ScriptPosition::Superscript,
                run_index: 2,
                ..SpanSpec::default()
            },
            SpanSpec {
                text: " redacted".to_string(),
                family: "Arial".to_string(),
                size: 12.0,
                strikethrough: true,
                color: Rgba::rgb(0x9E, 0x2A, 0x2B),
                run_index: 3,
                ..SpanSpec::default()
            },
        ],
        align: TextAlign::Left,
        line_spacing: LineSpacing::Multiple(1.2),
        max_width: Some(340.0),
        first_line_indent: 0.0,
        origin: Point::new(30.0, 80.0),
        page_index: 0,
        block_index: 1,
    };
    let shaped_body = engine.layout_paragraph(&body_spec, &mut font_sys);
    for run in shaped_body.runs {
        page.items.push(Item::Glyphs(run));
    }

    // 4. Clipped group with image
    page.items.push(Item::Group(GroupItem {
        transform: Affine::translate(30.0, 130.0),
        clip: Some(ClipPath {
            commands: vec![
                PathCommand::MoveTo(Point::new(0.0, 0.0)),
                PathCommand::LineTo(Point::new(64.0, 0.0)),
                PathCommand::LineTo(Point::new(64.0, 64.0)),
                PathCommand::LineTo(Point::new(0.0, 64.0)),
                PathCommand::Close,
            ],
            fill_rule: FillRule::NonZero,
        }),
        opacity: 0.9,
        children: vec![Item::Image(ImageItem {
            image: img_id,
            rect: Rect::new(0.0, 0.0, 64.0, 64.0),
            transform: Affine::IDENTITY,
            opacity: 1.0,
            alt_text: Some("Checkerboard".to_string()),
        })],
    }));

    // 5. Inert link and anchor
    page.items.push(Item::Link(prism_layout::LinkItem {
        rect: Rect::new(30.0, 28.0, 120.0, 20.0),
        target: "https://example.invalid/forensic-ref".to_string(),
    }));
    page.items.push(Item::Anchor(AnchorItem {
        name: "section-1".to_string(),
        rect: Rect::new(30.0, 28.0, 10.0, 10.0),
    }));

    LayoutDocument {
        pages: vec![page],
        fonts: font_sys.store,
        images,
        font_report: font_sys.report,
    }
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[test]
fn test_golden_svg_html_and_png_byte_identical_across_independent_runs() {
    let doc1 = build_reference_document();
    let doc2 = build_reference_document();

    // Verify serde JSON serialization of LayoutDocument is byte-identical
    let json1 = serde_json::to_vec(&doc1).expect("serialize doc1");
    let json2 = serde_json::to_vec(&doc2).expect("serialize doc2");
    assert_eq!(
        json1, json2,
        "Serialized LayoutDocument must be byte-identical"
    );

    // Verify SVG output is byte-identical
    let svg1 = render_page_svg(
        &doc1.pages[0],
        &doc1.fonts,
        &doc1.images,
        &LayoutOptions::default(),
    )
    .expect("svg1");
    let svg2 = render_page_svg(
        &doc2.pages[0],
        &doc2.fonts,
        &doc2.images,
        &LayoutOptions::default(),
    )
    .expect("svg2");
    assert_eq!(svg1, svg2, "SVG output must be byte-identical");

    // Verify HTML viewer output is byte-identical and contains selectable text + inert link
    let viewer_opts = SvgViewerOptions {
        title: Some("Golden Determinism Test".to_string()),
        ..SvgViewerOptions::default()
    };
    let html1 = render_document_viewer_html(&doc1, &viewer_opts).expect("html1");
    let html2 = render_document_viewer_html(&doc2, &viewer_opts).expect("html2");
    assert_eq!(html1, html2, "Viewer HTML must be byte-identical");
    assert!(html1.contains("Prism Phase 1 Golden Page"));
    assert!(html1.contains("Forensic determinism: SHA-256[1] redacted"));
    assert!(html1.contains("data-link-target=\"https://example.invalid/forensic-ref\""));
    assert!(
        !html1.contains("<a href="),
        "Links must be inert metadata, never active anchors"
    );

    // Verify PNG raster output is byte-identical across independent runs
    let raster_opts = RasterOptions {
        dpi: 144.0,
        ..RasterOptions::default()
    };
    let png1 =
        render_page_png(&doc1.pages[0], &doc1.fonts, &doc1.images, &raster_opts).expect("png1");
    let png2 =
        render_page_png(&doc2.pages[0], &doc2.fonts, &doc2.images, &raster_opts).expect("png2");
    assert_eq!(png1, png2, "PNG output must be byte-identical");
    assert_eq!(fnv1a_64(svg1.as_bytes()), fnv1a_64(svg2.as_bytes()));
    assert_eq!(fnv1a_64(&png1), fnv1a_64(&png2));
}
