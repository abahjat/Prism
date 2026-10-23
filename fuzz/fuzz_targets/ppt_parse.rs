// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_layout::LayoutOptions;
use prism_ppt::{layout_presentation, parse_ppt, parse_pptx, presentation_to_udm};

fuzz_target!(|data: &[u8]| {
    if data.len() > 512 * 1024 {
        return;
    }
    let opts = LayoutOptions::default();
    if let Ok(pres) = parse_pptx(data, 4 * 1024 * 1024) {
        let _ = presentation_to_udm(&pres, "PPTX");
        let _ = layout_presentation(&pres, &opts);
    }
    if let Ok(pres) = parse_ppt(data, 4 * 1024 * 1024) {
        let _ = presentation_to_udm(&pres, "PPT");
        let _ = layout_presentation(&pres, &opts);
    }
});
