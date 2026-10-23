// SPDX-License-Identifier: AGPL-3.0-only
//! Phase 1 visual fidelity & determinism harness for `prism-layout` and `prism-render`.
//!
//! Evaluates corpus documents (PPTX, DOCX, PDF, TXT), computes pure-Rust Structural
//! Similarity Index Measure (SSIM) scores on rendered page rasters (both PDF export
//! round-trip and optional Office reference renders in `test-files/reference/`),
//! checks against `fidelity-baseline.json`, and writes an HTML report to
//! `target/fidelity-report/index.html`.

use base64::{Engine as _, engine::general_purpose};
use bytes::Bytes;
use prism_core::parser::{ParseContext, ParseOptions};
use prism_layout::{LayoutDocument, LayoutOptions, interpret_pdf_bytes, layout_document};
use prism_parsers::registry::ParserRegistry;
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_render::raster::{RasterOptions, render_page_png};
use prism_render::svg::render_page_svg;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Deserialize)]
struct FidelityBaseline {
    dpi: f32,
    entries: BTreeMap<String, BaselineEntry>,
}

#[derive(Debug, Deserialize)]
struct BaselineEntry {
    min_pages: usize,
    min_roundtrip_ssim: f64,
    min_reference_ssim: f64,
}

struct EvalResult {
    filename: String,
    page_count: usize,
    layout_ms: f64,
    render_ms: f64,
    roundtrip_ssim: f64,
    reference_ssim: Option<f64>,
    substituted_fonts: usize,
    png_b64: String,
    roundtrip_png_b64: String,
}

/// Pure-Rust 8x8 windowed SSIM on luminance `Y = 0.299 R + 0.587 G + 0.114 B` for two RGBA8 images.
/// If dimensions differ, bilinearly samples `img_b` onto `img_a`'s grid.
fn compute_ssim_rgba(a_rgba: &[u8], a_w: u32, a_h: u32, b_rgba: &[u8], b_w: u32, b_h: u32) -> f64 {
    if a_w == 0 || a_h == 0 || b_w == 0 || b_h == 0 {
        return 0.0;
    }

    let luma = |rgba: &[u8], idx: usize| -> f64 {
        let r = f64::from(rgba[idx]);
        let g = f64::from(rgba[idx + 1]);
        let b = f64::from(rgba[idx + 2]);
        0.299 * r + 0.587 * g + 0.114 * b
    };

    let sample_b = |x: u32, y: u32| -> f64 {
        let bx =
            ((u64::from(x) * u64::from(b_w)) / u64::from(a_w)).min(u64::from(b_w - 1)) as usize;
        let by =
            ((u64::from(y) * u64::from(b_h)) / u64::from(a_h)).min(u64::from(b_h - 1)) as usize;
        let idx = (by * (b_w as usize) + bx) * 4;
        luma(b_rgba, idx)
    };

    let win = 8_u32;
    let c1 = (0.01_f64 * 255.0).powi(2);
    let c2 = (0.03_f64 * 255.0).powi(2);

    let mut ssim_sum = 0.0_f64;
    let mut win_count = 0_u64;

    let mut y = 0;
    while y + win <= a_h {
        let mut x = 0;
        while x + win <= a_w {
            let n = f64::from(win * win);
            let mut mean_a = 0.0;
            let mut mean_b = 0.0;

            for dy in 0..win {
                for dx in 0..win {
                    let px = x + dx;
                    let py = y + dy;
                    let a_idx = ((py as usize) * (a_w as usize) + (px as usize)) * 4;
                    mean_a += luma(a_rgba, a_idx);
                    mean_b += sample_b(px, py);
                }
            }
            mean_a /= n;
            mean_b /= n;

            let mut var_a = 0.0;
            let mut var_b = 0.0;
            let mut cov = 0.0;

            for dy in 0..win {
                for dx in 0..win {
                    let px = x + dx;
                    let py = y + dy;
                    let a_idx = ((py as usize) * (a_w as usize) + (px as usize)) * 4;
                    let da = luma(a_rgba, a_idx) - mean_a;
                    let db = sample_b(px, py) - mean_b;
                    var_a += da * da;
                    var_b += db * db;
                    cov += da * db;
                }
            }
            var_a /= n - 1.0;
            var_b /= n - 1.0;
            cov /= n - 1.0;

            let num = (2.0 * mean_a * mean_b + c1) * (2.0 * cov + c2);
            let den = (mean_a * mean_a + mean_b * mean_b + c1) * (var_a + var_b + c2);
            ssim_sum += num / den;
            win_count += 1;

            x += win;
        }
        y += win;
    }

    if win_count == 0 {
        1.0
    } else {
        ssim_sum / (win_count as f64)
    }
}

fn decode_png_rgba(png_bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(png_bytes)
        .expect("decode PNG")
        .to_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

fn load_and_layout(
    registry: &ParserRegistry,
    path: &Path,
    options: &LayoutOptions,
) -> LayoutDocument {
    let data = std::fs::read(path).expect("read corpus file");
    let filename = path.file_name().unwrap().to_string_lossy().to_string();
    if filename.ends_with(".pdf") {
        return interpret_pdf_bytes(&data, options).expect("interpret PDF");
    }

    let detection =
        prism_core::format::detect_format(&data, Some(&filename)).expect("detect format");
    let parser = registry.get_parser(&detection.format).expect("get parser");
    let context = ParseContext {
        format: detection.format,
        filename: Some(filename),
        size: data.len(),
        options: ParseOptions::default(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let udm = rt
        .block_on(parser.parse(Bytes::from(data), context))
        .expect("parse document");
    layout_document(&udm, options).expect("layout_document")
}

#[test]
fn test_ssim_identical_and_inverted_images() {
    let img_a = vec![240_u8; 64 * 64 * 4];
    let ssim_same = compute_ssim_rgba(&img_a, 64, 64, &img_a, 64, 64);
    assert!(
        (ssim_same - 1.0).abs() < 1e-6,
        "Identical images must have SSIM = 1.0, got {ssim_same}"
    );

    let mut img_b = vec![20_u8; 64 * 64 * 4];
    for px in img_b.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    let ssim_diff = compute_ssim_rgba(&img_a, 64, 64, &img_b, 64, 64);
    assert!(
        ssim_diff < 0.2,
        "Expected low SSIM for inverted image, got {ssim_diff}"
    );
}

#[test]
fn test_phase1_corpus_fidelity_and_generate_report() {
    let baseline_json = std::fs::read_to_string("../../fidelity-baseline.json")
        .expect("read fidelity-baseline.json");
    let baseline: FidelityBaseline =
        serde_json::from_str(&baseline_json).expect("parse fidelity-baseline.json");

    let corpus_dir = Path::new("../../test-files");
    let ref_dir = corpus_dir.join("reference");
    let registry = ParserRegistry::with_default_parsers();
    let layout_opts = LayoutOptions::default();
    let raster_opts = RasterOptions {
        dpi: baseline.dpi,
        layout_options: layout_opts.clone(),
        ..RasterOptions::default()
    };

    let mut results = Vec::new();

    for (filename, entry) in &baseline.entries {
        let file_path = corpus_dir.join(filename);
        assert!(file_path.exists(), "Missing corpus file: {file_path:?}");

        let t0 = Instant::now();
        let layout_doc = load_and_layout(&registry, &file_path, &layout_opts);
        let layout_ms = t0.elapsed().as_secs_f64() * 1000.0;

        assert!(
            layout_doc.pages.len() >= entry.min_pages,
            "{filename}: expected at least {} pages, got {}",
            entry.min_pages,
            layout_doc.pages.len()
        );

        let first_page = &layout_doc.pages[0];

        // Verify text() and char_boxes() stay 1:1 synchronized
        let page_text = first_page.text();
        let char_boxes = first_page.char_boxes();
        let reconstructed: String = char_boxes.iter().map(|b| b.ch).collect();
        assert_eq!(
            page_text, reconstructed,
            "{filename}: page.text() and char_boxes() out of sync"
        );

        // Render page 0 to SVG and PNG and verify determinism
        let t1 = Instant::now();
        let svg1 = render_page_svg(
            first_page,
            &layout_doc.fonts,
            &layout_doc.images,
            &layout_opts,
        )
        .expect("render_page_svg 1");
        let png1 = render_page_png(
            first_page,
            &layout_doc.fonts,
            &layout_doc.images,
            &raster_opts,
        )
        .expect("render_page_png 1");
        let render_ms = t1.elapsed().as_secs_f64() * 1000.0;

        let svg2 = render_page_svg(
            first_page,
            &layout_doc.fonts,
            &layout_doc.images,
            &layout_opts,
        )
        .expect("render_page_svg 2");
        let png2 = render_page_png(
            first_page,
            &layout_doc.fonts,
            &layout_doc.images,
            &raster_opts,
        )
        .expect("render_page_png 2");
        assert_eq!(svg1, svg2, "{filename}: SVG output not deterministic");
        assert_eq!(png1, png2, "{filename}: PNG output not deterministic");

        // Export to PDF, re-interpret via hayro, rasterize page 0, and compute round-trip SSIM
        let exported_pdf = export_document_pdf(&layout_doc, &PdfExportOptions::default())
            .expect("export_document_pdf");
        let reparsed_doc =
            interpret_pdf_bytes(&exported_pdf, &layout_opts).expect("re-interpret exported PDF");
        let roundtrip_png = render_page_png(
            &reparsed_doc.pages[0],
            &reparsed_doc.fonts,
            &reparsed_doc.images,
            &raster_opts,
        )
        .expect("render roundtrip PNG");

        let (rgba_orig, w_orig, h_orig) = decode_png_rgba(&png1);
        let (rgba_rt, w_rt, h_rt) = decode_png_rgba(&roundtrip_png);
        let roundtrip_ssim = compute_ssim_rgba(&rgba_orig, w_orig, h_orig, &rgba_rt, w_rt, h_rt);

        assert!(
            roundtrip_ssim >= entry.min_roundtrip_ssim,
            "{filename}: round-trip SSIM ({roundtrip_ssim:.4}) fell below baseline ({:.4})",
            entry.min_roundtrip_ssim
        );

        // Optional reference PDF comparison if `scripts/make-references.ps1` has populated `test-files/reference/`
        let stem = Path::new(filename)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let ref_candidates = [
            ref_dir.join(format!("{stem}.word.pdf")),
            ref_dir.join(format!("{stem}.pptx.pdf")),
            ref_dir.join(format!("{stem}.pdf")),
        ];
        let mut reference_ssim = None;
        for cand in &ref_candidates {
            if cand.exists()
                && let Ok(ref_bytes) = std::fs::read(cand)
                && let Ok(ref_layout) = interpret_pdf_bytes(&ref_bytes, &layout_opts)
                && !ref_layout.pages.is_empty()
                && let Ok(ref_png) = render_page_png(
                    &ref_layout.pages[0],
                    &ref_layout.fonts,
                    &ref_layout.images,
                    &raster_opts,
                )
            {
                let (rgba_ref, w_ref, h_ref) = decode_png_rgba(&ref_png);
                let ssim = compute_ssim_rgba(&rgba_orig, w_orig, h_orig, &rgba_ref, w_ref, h_ref);
                assert!(
                    ssim >= entry.min_reference_ssim,
                    "{filename}: reference SSIM ({ssim:.4}) fell below baseline ({:.4})",
                    entry.min_reference_ssim
                );
                reference_ssim = Some(ssim);
                break;
            }
        }

        let substituted_fonts = layout_doc
            .font_report
            .entries
            .values()
            .filter(|e| !e.exact_match)
            .count();

        results.push(EvalResult {
            filename: filename.clone(),
            page_count: layout_doc.pages.len(),
            layout_ms,
            render_ms,
            roundtrip_ssim,
            reference_ssim,
            substituted_fonts,
            png_b64: general_purpose::STANDARD.encode(&png1),
            roundtrip_png_b64: general_purpose::STANDARD.encode(&roundtrip_png),
        });
    }

    write_fidelity_html_report(&results);
}

fn write_fidelity_html_report(results: &[EvalResult]) {
    let report_dir = Path::new("../../target/fidelity-report");
    let _ = std::fs::create_dir_all(report_dir);
    let report_path = report_dir.join("index.html");

    let mut html = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Prism Phase 1 Fidelity Report</title>
<style>
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 24px; background: #f8fafc; color: #0f172a; }
h1 { margin-top: 0; }
table { width: 100%; border-collapse: collapse; background: #fff; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
th, td { border: 1px solid #e2e8f0; padding: 10px 12px; text-align: left; vertical-align: top; }
th { background: #1e293b; color: #f8fafc; }
.thumb { max-width: 220px; max-height: 280px; border: 1px solid #cbd5e1; }
.badge { display: inline-block; padding: 2px 8px; border-radius: 999px; font-weight: 600; font-size: 12px; background: #dcfce7; color: #166534; }
</style>
</head>
<body>
<h1>Prism Phase 1 Layout &amp; Render Fidelity Report</h1>
<table>
<thead>
<tr>
  <th>Document</th>
  <th>Pages</th>
  <th>Layout (ms)</th>
  <th>Render (ms)</th>
  <th>Substituted Fonts</th>
  <th>Round-Trip SSIM</th>
  <th>Reference SSIM</th>
  <th>Direct Render (Page 1)</th>
  <th>PDF Export Round-Trip (Page 1)</th>
</tr>
</thead>
<tbody>
"#,
    );

    for r in results {
        let ref_str = r
            .reference_ssim
            .map_or_else(|| "N/A".to_string(), |v| format!("{v:.4}"));
        let _ = writeln!(
            html,
            r#"<tr>
  <td><strong>{}</strong></td>
  <td>{}</td>
  <td>{:.2}</td>
  <td>{:.2}</td>
  <td>{}</td>
  <td><span class="badge">{:.4}</span></td>
  <td>{}</td>
  <td><img class="thumb" src="data:image/png;base64,{}" alt="Direct render"/></td>
  <td><img class="thumb" src="data:image/png;base64,{}" alt="Roundtrip render"/></td>
</tr>"#,
            r.filename,
            r.page_count,
            r.layout_ms,
            r.render_ms,
            r.substituted_fonts,
            r.roundtrip_ssim,
            ref_str,
            r.png_b64,
            r.roundtrip_png_b64
        );
    }

    html.push_str("</tbody>\n</table>\n</body>\n</html>\n");
    std::fs::write(&report_path, html).expect("write target/fidelity-report/index.html");
}
