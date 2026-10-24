// SPDX-License-Identifier: AGPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use prism_core::tree::ContainerLimits;
use prism_parsers::tree::build_family_tree;

fuzz_target!(|data: &[u8]| {
    if data.len() > 512 * 1024 {
        return;
    }
    let limits = ContainerLimits {
        max_depth: 3,
        max_total_bytes: 2 * 1024 * 1024,
        max_members: 32,
    };
    let _ = build_family_tree(data, None, limits);
});
