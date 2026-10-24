// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_production::{parse_concordance_dat, parse_csv_load_file, parse_opticon_opt};

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = parse_concordance_dat(text, '\u{0014}', '\u{00FE}', '\u{00AE}');
        let _ = parse_opticon_opt(text);
        let _ = parse_csv_load_file(text);
    }
});
