// SPDX-License-Identifier: AGPL-3.0-only
//! Robustness smoke test: every parser must survive malformed input.
//!
//! Evidence files are routinely truncated, carved or corrupted. A parser may
//! return an error for such input, but it must never panic or hang, because a
//! panic or hang inside the host review application loses the reviewer's work.
//!
//! Each corpus file is mutated deterministically (truncations and byte
//! corruption) and parsed on a worker thread with a time limit.

use prism_core::parser::{ParseContext, ParseOptions};
use prism_layout::{LayoutOptions, interpret_pdf_bytes, layout_document};
use prism_parsers::registry::ParserRegistry;
use prism_render::raster::{RasterOptions, render_page_png};
use prism_render::svg::render_page_svg;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use walkdir::WalkDir;

const MAX_FILE_SIZE: usize = 2 * 1024 * 1024;
const PARSE_TIMEOUT: Duration = Duration::from_secs(20);
const CORRUPTIONS_PER_FILE: u64 = 2;
const BYTES_CORRUPTED: usize = 24;

/// Small deterministic PRNG (`SplitMix64`) so failures are reproducible
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn mutations(data: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![
        ("truncated-50%".to_string(), data[..data.len() / 2].to_vec()),
        (
            "truncated-10%".to_string(),
            data[..data.len() / 10].to_vec(),
        ),
        (
            "truncated-512".to_string(),
            data[..data.len().min(512)].to_vec(),
        ),
    ];
    for seed in 0..CORRUPTIONS_PER_FILE {
        let mut rng = SplitMix64(seed ^ data.len() as u64);
        let mut corrupted = data.to_vec();
        // Keep the first 8 bytes so the format is still detected as the same type
        if corrupted.len() > 8 {
            for _ in 0..BYTES_CORRUPTED {
                let idx = 8 + (rng.next() as usize) % (corrupted.len() - 8);
                corrupted[idx] = rng.next() as u8;
            }
        }
        out.push((format!("corrupted-seed{seed}"), corrupted));
    }
    out
}

enum Outcome {
    Completed,
    Panicked(String),
    TimedOut,
}

fn parse_isolated(
    registry: &Arc<ParserRegistry>,
    data: Vec<u8>,
    filename: &str,
    extension: &str,
) -> Option<Outcome> {
    let detection = prism_core::format::detect_format(&data, Some(extension))?;
    registry.get_parser(&detection.format)?;

    let (tx, rx) = mpsc::channel();
    let registry = Arc::clone(registry);
    let filename = filename.to_string();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let parser = registry
                .get_parser(&detection.format)
                .expect("parser checked above");
            let context = ParseContext {
                format: detection.format.clone(),
                filename: Some(filename),
                size: data.len(),
                options: ParseOptions::default(),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            let layout_opts = LayoutOptions {
                max_pages: 64,
                max_glyphs_per_page: 20_000,
                max_image_pixels: 4_000_000,
                max_decompressed_stream_bytes: 16 * 1024 * 1024,
                max_recursion_depth: 16,
                allow_system_fonts: false,
            };
            if data.starts_with(b"%PDF-")
                && let Ok(pdf_doc) = interpret_pdf_bytes(&data, &layout_opts)
                && let Some(first) = pdf_doc.pages.first()
            {
                let _ = render_page_svg(first, &pdf_doc.fonts, &pdf_doc.images, &layout_opts);
            }
            // Errors are acceptable for malformed input; only panics/hangs fail.
            if let Ok(doc) = runtime.block_on(parser.parse(bytes::Bytes::from(data), context))
                && let Ok(layout_doc) = layout_document(&doc, &layout_opts)
                && let Some(first) = layout_doc.pages.first()
            {
                let _ = render_page_svg(first, &layout_doc.fonts, &layout_doc.images, &layout_opts);
                let raster_opts = RasterOptions {
                    dpi: 72.0,
                    layout_options: layout_opts,
                    ..RasterOptions::default()
                };
                let _ = render_page_png(first, &layout_doc.fonts, &layout_doc.images, &raster_opts);
            }
        }));
        let outcome = match result {
            Ok(()) => Outcome::Completed,
            Err(payload) => Outcome::Panicked(
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "non-string panic payload".to_string()),
            ),
        };
        let _ = tx.send(outcome);
    });

    Some(rx.recv_timeout(PARSE_TIMEOUT).unwrap_or(Outcome::TimedOut))
}

#[test]
fn parsers_survive_malformed_input() {
    let corpus_path = Path::new("../../test-files");
    if !corpus_path.exists() {
        println!("Test corpus not found at {corpus_path:?}, skipping robustness test");
        return;
    }

    // Panics are expected to be caught here; keep the default hook from
    // flooding the output with backtraces.
    std::panic::set_hook(Box::new(|_| {}));

    let registry = Arc::new(ParserRegistry::with_default_parsers());
    let mut cases = 0;
    let mut failures = Vec::new();

    for entry in WalkDir::new(corpus_path).into_iter().flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        if data.is_empty() || data.len() > MAX_FILE_SIZE {
            continue;
        }
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();

        for (mutation, mutated) in mutations(&data) {
            match parse_isolated(&registry, mutated, &filename, &extension) {
                None | Some(Outcome::Completed) => {}
                Some(Outcome::Panicked(message)) => {
                    failures.push(format!("{filename} [{mutation}]: panic: {message}"));
                }
                Some(Outcome::TimedOut) => {
                    failures.push(format!("{filename} [{mutation}]: timed out"));
                }
            }
            cases += 1;
        }
    }

    let _ = std::panic::take_hook();
    println!(
        "Robustness: {cases} mutated inputs, {} failures",
        failures.len()
    );
    for failure in &failures {
        println!("  {failure}");
    }
    assert!(
        failures.is_empty(),
        "{} of {cases} malformed inputs crashed or hung a parser:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
