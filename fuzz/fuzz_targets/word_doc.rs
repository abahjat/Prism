// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_word::{parse_doc, word_to_udm};

fuzz_target!(|data: &[u8]| {
    if data.len() > 512 * 1024 {
        return;
    }
    if let Ok(doc) = parse_doc(data, 4 * 1024 * 1024) {
        let _ = word_to_udm(&doc, "DOC");
    }
});
