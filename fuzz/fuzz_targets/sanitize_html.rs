// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz the untrusted-HTML sanitizer and the HTML-to-text converter.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let html = String::from_utf8_lossy(data);
    let clean = prism_render::sanitize::sanitize_html(&html);
    assert!(!clean.to_ascii_lowercase().contains("<script"));
    let _ = prism_core::text::html_to_text(&html);
});
