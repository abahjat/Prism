// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_layout::{LayoutOptions, MarkupMode};
use prism_render::svg::render_page_svg;
use prism_word::{is_doc_bytes, layout_word_document, parse_doc, parse_docx};

fuzz_target!(|data: &[u8]| {
    if data.len() > 512 * 1024 {
        return;
    }
    let word_doc = if is_doc_bytes(data) {
        parse_doc(data, 4 * 1024 * 1024).ok()
    } else {
        parse_docx(data, Some("fuzz.docx"), 4 * 1024 * 1024).ok()
    };
    let Some(doc) = word_doc else {
        return;
    };
    let opts = LayoutOptions {
        max_pages: 16,
        max_glyphs_per_page: 10_000,
        max_image_pixels: 2_000_000,
        max_decompressed_stream_bytes: 4 * 1024 * 1024,
        max_recursion_depth: 12,
        allow_system_fonts: false,
        markup_mode: MarkupMode::AllMarkup,
        show_hidden_text: false,
    };
    if let Ok(layout_doc) = layout_word_document(&doc, &opts)
        && let Some(first) = layout_doc.pages.first()
    {
        let _ = render_page_svg(first, &layout_doc.fonts, &layout_doc.images, &opts);
    }
});
