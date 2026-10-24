// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_numfmt::{CellInputValue, FormatOptions, format_cell_value, is_concealed_format};

fuzz_target!(|data: &[u8]| {
    if data.len() < 9 || data.len() > 4096 {
        return;
    }
    let mut num_bytes = [0u8; 8];
    num_bytes.copy_from_slice(&data[0..8]);
    let val = f64::from_le_bytes(num_bytes);
    let Ok(code) = std::str::from_utf8(&data[8..]) else {
        return;
    };
    let _ = is_concealed_format(code);
    let _ = format_cell_value(
        &CellInputValue::Number(val),
        None,
        Some(code),
        &FormatOptions::default(),
    );
});
