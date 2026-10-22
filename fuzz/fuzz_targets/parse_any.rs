// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz format detection, parsing and HTML rendering of arbitrary input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_core::parser::{ParseContext, ParseOptions};
use prism_core::render::{RenderContext, Renderer};
use prism_parsers::registry::ParserRegistry;
use std::sync::LazyLock;

static REGISTRY: LazyLock<ParserRegistry> = LazyLock::new(ParserRegistry::with_default_parsers);
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
});

fuzz_target!(|data: &[u8]| {
    let Some(detection) = prism_core::format::detect_format(data, None) else {
        return;
    };
    let Some(parser) = REGISTRY.get_parser(&detection.format) else {
        return;
    };
    let context = ParseContext {
        format: detection.format.clone(),
        filename: None,
        size: data.len(),
        options: ParseOptions::default(),
    };
    RUNTIME.block_on(async {
        if let Ok(document) = parser.parse(bytes::Bytes::copy_from_slice(data), context).await {
            let _ = document.extract_text();
            let renderer = prism_render::html::HtmlRenderer::new();
            let _ = renderer
                .render(
                    &document,
                    RenderContext {
                        options: Default::default(),
                        filename: None,
                    },
                )
                .await;
        }
    });
});
