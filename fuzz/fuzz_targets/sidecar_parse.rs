// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_review::sidecar::AnnotationSidecar;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(sidecar) = AnnotationSidecar::from_json(text) {
            let _ = sidecar.to_json_deterministic();
        }
    }
});
