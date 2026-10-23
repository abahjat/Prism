// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_layout::{LayoutOptions, interpret_pdf_bytes};
use prism_render::svg::render_page_svg;

fuzz_target!(|data: &[u8]| {
    if data.len() > 512 * 1024 {
        return;
    }
    let opts = LayoutOptions {
        max_pages: 16,
        max_glyphs_per_page: 10_000,
        max_image_pixels: 2_000_000,
        max_decompressed_stream_bytes: 4 * 1024 * 1024,
        max_recursion_depth: 12,
        allow_system_fonts: false,
    };
    if let Ok(layout_doc) = interpret_pdf_bytes(data, &opts)
        && let Some(first) = layout_doc.pages.first()
    {
        let _ = render_page_svg(first, &layout_doc.fonts, &layout_doc.images, &opts);
    }
});
