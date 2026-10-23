// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_excel::{layout_workbook, parse_xls, parse_xlsx, workbook_to_udm};
use prism_layout::LayoutOptions;

fuzz_target!(|data: &[u8]| {
    if data.len() > 512 * 1024 {
        return;
    }
    let opts = LayoutOptions::default();
    if let Ok(wb) = parse_xlsx(data, 4 * 1024 * 1024) {
        let _ = workbook_to_udm(&wb, "XLSX");
        let _ = layout_workbook(&wb, &opts);
    }
    if let Ok(wb) = parse_xls(data, 4 * 1024 * 1024) {
        let _ = workbook_to_udm(&wb, "XLS");
        let _ = layout_workbook(&wb, &opts);
    }
});
