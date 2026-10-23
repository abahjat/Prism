// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use prism_core::parser::{ParseContext, ParseOptions};
use prism_layout::{LayoutOptions, layout_document};
use prism_parsers::registry::ParserRegistry;
use prism_render::svg::render_page_svg;
use std::sync::LazyLock;
use tokio::runtime::Runtime;

static REGISTRY: LazyLock<ParserRegistry> = LazyLock::new(ParserRegistry::with_default_parsers);
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
});

fuzz_target!(|data: &[u8]| {
    if data.len() > 512 * 1024 {
        return;
    }
    let Some(detection) = prism_core::format::detect_format(data, None) else {
        return;
    };
    let Some(parser) = REGISTRY.get_parser(&detection.format) else {
        return;
    };
    let context = ParseContext {
        format: detection.format,
        filename: None,
        size: data.len(),
        options: ParseOptions::default(),
    };
    if let Ok(doc) = RUNTIME.block_on(parser.parse(Bytes::copy_from_slice(data), context)) {
        let opts = LayoutOptions {
            max_pages: 16,
            max_glyphs_per_page: 10_000,
            max_image_pixels: 2_000_000,
            max_decompressed_stream_bytes: 4 * 1024 * 1024,
            max_recursion_depth: 12,
            allow_system_fonts: false,
            ..LayoutOptions::default()
        };
        if let Ok(layout_doc) = layout_document(&doc, &opts)
            && let Some(first) = layout_doc.pages.first()
        {
            let _ = render_page_svg(first, &layout_doc.fonts, &layout_doc.images, &opts);
        }
    }
});
