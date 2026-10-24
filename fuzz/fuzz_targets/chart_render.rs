// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_drawingml::{DrawingTheme, parse_chart_xml, render_chart};
use prism_layout::{FontSystem, LayoutOptions, Rect, TextLayoutEngine};

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    let Ok(xml) = std::str::from_utf8(data) else {
        return;
    };
    let theme = DrawingTheme::default();
    if let Ok(chart) = parse_chart_xml(xml) {
        let opts = LayoutOptions::default();
        let mut font_sys = FontSystem::new(&opts);
        let mut text_engine = TextLayoutEngine::default();
        let _ = render_chart(
            &chart,
            Rect::new(0.0, 0.0, 400.0, 280.0),
            &theme,
            &mut font_sys,
            &mut text_engine,
        );
    }
});
