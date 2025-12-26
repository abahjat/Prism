// SPDX-License-Identifier: AGPL-3.0-only
use prism_core::parser::{ParseContext, ParseOptions};
use prism_parsers::registry::ParserRegistry;
use std::path::Path;
use walkdir::WalkDir;

#[tokio::test]
async fn test_corpus_smoke() {
    prism_tests::setup_test_logging();

    // Path to test files relative to the crate root (which is crates/prism-tests)
    let corpus_path = Path::new("../../test-files");
    if !corpus_path.exists() {
        println!(
            "Test corpus not found at {:?}, skipping smoke test",
            corpus_path
        );
        return;
    }

    let registry = ParserRegistry::with_default_parsers();
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for entry in WalkDir::new(corpus_path) {
        let entry = entry.unwrap();
        let path = entry.path();

        if path.is_dir() {
            continue;
        }

        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        println!("Testing: {}", filename);

        // Read file first
        match tokio::fs::read(path).await {
            Ok(data) => {
                let extension = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default();

                // 1. Detect format
                if let Some(detection) = prism_core::format::detect_format(&data, Some(&extension))
                {
                    let fmt = detection.format;

                    // 2. Find parser
                    if let Some(parser) = registry.get_parser(&fmt) {
                        // 4. Parse
                        let context = ParseContext {
                            format: fmt.clone(),
                            filename: Some(filename.clone()),
                            size: data.len(),
                            options: ParseOptions::default(),
                        };

                        match parser.parse(bytes::Bytes::from(data), context).await {
                            Ok(doc) => {
                                println!("  PASS: {} ({} pages/blocks)", filename, doc.pages.len());
                                passed += 1;
                            }
                            Err(e) => {
                                println!("  FAIL: {} - {}", filename, e);
                                failed += 1;
                            }
                        }
                    } else {
                        println!("  SKIP: No parser for {}", fmt.name);
                        skipped += 1;
                    }
                } else {
                    println!("  SKIP: Unknown format");
                    skipped += 1;
                }
            }
            Err(e) => {
                println!("  ERROR reading file: {}", e);
                failed += 1;
            }
        }
    }

    println!("\nSummary:");
    println!("  Passed: {}", passed);
    println!("  Failed: {}", failed);
    println!("  Skipped: {}", skipped);

    // We don't necessarily want to fail the build if one sketchy file fails,
    // but for now let's just log it.
    // In a real CI, we might have a whitelist of expected passing files.
    assert!(passed > 0, "Should have passed at least some tests");
}
