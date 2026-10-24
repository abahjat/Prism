// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Review (`prism-review`)
//!
//! External annotation sidecars ([`AnnotationSidecar`]), cross-engine-version anchor resolution
//! ([`AnnotationAnchor`], [`TextAnchor`], [`AnchorResolutionStatus`]), pattern-based proposed
//! redactions ([`propose_pattern_redactions`]), destructive redaction ([`apply_redactions`],
//! [`apply_workbook_redactions`], [`scrub_metadata`]), post-export verification
//! ([`verify_redacted_output`]), and content-addressed artifact caching ([`ArtifactCache`],
//! [`FilesystemCache`]).

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::assert_is_empty,
    clippy::chunks_exact_to_as_chunks,
    clippy::manual_midpoint,
    clippy::module_name_repetitions,
    clippy::question_mark,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

pub mod cache;
pub mod redaction;
pub mod sidecar;

pub use cache::{ArtifactCache, CacheArtifactKind, CacheKey, FilesystemCache};
pub use redaction::{
    RedactedExportFormat, RedactedExportResult, RedactionLog, RedactionLogEntry, apply_redactions,
    apply_workbook_redactions, cell_range_anchor, export_redacted_document,
    has_approved_cell_redactions, scrub_metadata, verify_redacted_output, verify_redacted_regions,
};
pub use sidecar::{
    ANNOTATION_SIDECAR_SCHEMA_VERSION, AnchorResolutionStatus, Annotation, AnnotationAnchor,
    AnnotationKind, AnnotationSidecar, RedactionTarget, ResolvedAnnotation, TextAnchor,
    propose_pattern_redactions, render_page_with_annotations,
};

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::ENGINE_VERSION;
    use prism_core::metadata::Metadata;
    use prism_core::review::{ReviewRect, SourceRef};
    use prism_excel::model::{Cell, CellValue, FormulaInfo, Sheet, SheetComment, Workbook};
    use prism_layout::{
        Affine, Cluster, Decorations, FontSourceKind, GlyphRun, Item, LayoutDocument,
        LayoutOptions, LayoutPage, PositionedGlyph, Rgba, Size,
    };

    const TEST_FONT_BYTES: &[u8] =
        include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");

    fn build_sample_layout(text: &str, baseline_y: f32) -> LayoutDocument {
        let mut doc = LayoutDocument::new();
        let font_id = doc.fonts.register_ttf(
            "Liberation Sans",
            400,
            false,
            FontSourceKind::Bundled,
            TEST_FONT_BYTES.to_vec(),
            0,
        );
        let mut page = LayoutPage::new(0, Size::LETTER);

        let mut glyphs = Vec::new();
        let mut clusters = Vec::new();
        let mut x = 0.0_f32;
        let mut byte_idx = 0u32;

        for (i, ch) in text.chars().enumerate() {
            let adv = 7.0_f32;
            let gid = doc.fonts.map_char_to_glyph(font_id, ch);
            glyphs.push(PositionedGlyph {
                glyph_id: gid,
                x,
                y: 0.0,
                advance: adv,
            });
            #[allow(clippy::cast_possible_truncation)]
            let ch_len = ch.len_utf8() as u32;
            #[allow(clippy::cast_possible_truncation)]
            let gi = i as u32;
            clusters.push(Cluster {
                text_range: byte_idx..(byte_idx + ch_len),
                glyph_range: gi..(gi + 1),
                x,
                advance: adv,
            });
            x += adv;
            byte_idx += ch_len;
        }

        page.items.push(Item::Glyphs(GlyphRun {
            font: font_id,
            size: 12.0,
            color: Rgba::BLACK,
            transform: Affine::translate(72.0, baseline_y),
            glyphs,
            text: text.to_string(),
            clusters,
            decorations: Decorations::default(),
            source: SourceRef::new(0, 0, 0),
        }));

        doc.pages.push(page);
        doc
    }

    #[test]
    fn sidecar_roundtrip_and_cross_version_resolution() {
        let raw_bytes = b"Sample forensic document bytes";
        let layout_v1 = build_sample_layout("Hello SSN-123-45-6789 World", 100.0);

        let (text_anchor, rects) =
            TextAnchor::from_page_quote(&layout_v1, 0, "SSN-123-45-6789").expect("quote anchor");

        let mut sidecar = AnnotationSidecar::new(raw_bytes);
        sidecar.add(Annotation {
            id: "ann-1".to_string(),
            kind: AnnotationKind::Redaction {
                reason_code: "PII".to_string(),
                label: "REDACTED".to_string(),
                fill_color: None,
            },
            anchor: AnnotationAnchor {
                target: RedactionTarget::TextRange,
                page_index: 0,
                rects_pt: rects,
                text_anchor: Some(text_anchor),
            },
            author: Some("reviewer@example.com".to_string()),
            created_at: Some("2026-01-15T12:00:00Z".to_string()),
            proposed: false,
        });

        let json1 = sidecar.to_json_deterministic().expect("json1");
        let parsed = AnnotationSidecar::from_json(&json1).expect("parse");
        let json2 = parsed.to_json_deterministic().expect("json2");
        assert_eq!(json1, json2);

        // Same engine version -> Exact
        let resolved_same = parsed
            .resolve_against(raw_bytes, ENGINE_VERSION, &layout_v1)
            .expect("resolve same");
        assert_eq!(resolved_same[0].status, AnchorResolutionStatus::Exact);

        // Simulated layout shift under a newer engine version -> Moved
        let layout_v2 = build_sample_layout("Hello SSN-123-45-6789 World", 180.0);
        let resolved_moved = parsed
            .resolve_against(raw_bytes, "0.2.0+layout.2", &layout_v2)
            .expect("resolve moved");
        assert!(matches!(
            resolved_moved[0].status,
            AnchorResolutionStatus::Moved {
                old_page: 0,
                new_page: 0,
                ..
            }
        ));

        // Simulated missing text under a newer engine version -> Unresolved (never silently dropped)
        let layout_v3 = build_sample_layout("Completely different page text", 100.0);
        let resolved_unres = parsed
            .resolve_against(raw_bytes, "0.2.0+layout.2", &layout_v3)
            .expect("resolve unresolved");
        assert!(matches!(
            resolved_unres[0].status,
            AnchorResolutionStatus::Unresolved { .. }
        ));
    }

    #[test]
    fn destructive_redaction_removes_text_and_pixels_and_verifies_across_formats() {
        let raw_bytes = b"Document with secret word CLASSIFIED-OMEGA and embedded image";
        let mut layout = build_sample_layout("Public header CLASSIFIED-OMEGA public footer", 120.0);

        // Also register an RGBA image on the page and redact a region over it
        let mut img_rgba = vec![200u8; 40 * 40 * 4];
        for px in img_rgba.chunks_exact_mut(4) {
            px[0] = 250;
            px[1] = 20;
            px[2] = 20;
            px[3] = 255;
        }
        let img_id = layout
            .images
            .register("image/x-rgba8", 40, 40, img_rgba, &LayoutOptions::default())
            .expect("register image");
        layout.pages[0]
            .items
            .push(Item::Image(prism_layout::ImageItem {
                image: img_id,
                rect: prism_layout::Rect::new(72.0, 200.0, 80.0, 80.0),
                transform: Affine::IDENTITY,
                opacity: 1.0,
                alt_text: Some("CLASSIFIED-IMAGE-ALT".to_string()),
            }));

        // Propose pattern redaction for CLASSIFIED-OMEGA
        let mut proposals =
            propose_pattern_redactions(&layout, r"CLASSIFIED-\w+", true, "SECRET", "REDACTED")
                .expect("propose");
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].proposed);

        let mut sidecar = AnnotationSidecar::new(raw_bytes);
        let prop_id = proposals[0].id.clone();
        sidecar.add(proposals.remove(0));

        // Add an approved image region redaction
        sidecar.add(Annotation {
            id: "img-redact-1".to_string(),
            kind: AnnotationKind::Redaction {
                reason_code: "IMG".to_string(),
                label: "REDACTED".to_string(),
                fill_color: Some("#000000".to_string()),
            },
            anchor: AnnotationAnchor {
                target: RedactionTarget::PageRect,
                page_index: 0,
                rects_pt: vec![ReviewRect::new(80.0, 210.0, 40.0, 40.0)],
                text_anchor: None,
            },
            author: None,
            created_at: None,
            proposed: false,
        });

        // Approve the proposed text redaction
        assert!(sidecar.approve(&prop_id));

        let mut meta = Metadata {
            title: Some("Report on CLASSIFIED-OMEGA".to_string()),
            author: Some("Agent Jane".to_string()),
            ..Metadata::default()
        };

        // Export to PDF, TIFF, PNG, SVG, and Text — each runs `verify_redacted_output` automatically
        for fmt in [
            RedactedExportFormat::Pdf,
            RedactedExportFormat::Tiff,
            RedactedExportFormat::Png { page_index: 0 },
            RedactedExportFormat::Svg { page_index: 0 },
            RedactedExportFormat::Text,
        ] {
            let res = export_redacted_document(raw_bytes, &layout, &meta, &sidecar, fmt, 144.0)
                .expect("export and verify redacted output");
            assert!(!res.bytes.is_empty());
            assert!(
                res.log
                    .redacted_strings
                    .iter()
                    .any(|s| s.contains("CLASSIFIED-OMEGA"))
            );
        }

        // Verify metadata scrubbing removes the title containing CLASSIFIED-OMEGA
        let mut strings = vec!["CLASSIFIED-OMEGA".to_string()];
        scrub_metadata(&mut meta, &sidecar, &mut strings);
        assert!(meta.title.is_none());

        // Verify that `verify_redacted_output` fails if a redacted string leaks into output bytes
        let leaked = b"Public header CLASSIFIED-OMEGA public footer";
        assert!(
            verify_redacted_output(leaked, Some("txt"), &["CLASSIFIED-OMEGA".to_string()]).is_err()
        );
    }

    #[test]
    fn workbook_redaction_removes_cell_values_formulas_and_comments() {
        let mut wb = Workbook::default();
        let mut sheet = Sheet {
            name: "Financials".to_string(),
            sheet_id: 1,
            ..Sheet::default()
        };
        sheet.cells.insert(
            (1, 1), // B2
            Cell {
                value: CellValue::Text("Account-998877".to_string()),
                formula: Some(FormulaInfo {
                    expression: "CONCAT(\"Account-\", \"998877\")".to_string(),
                    is_array: false,
                    ref_range: None,
                    has_cached_value: true,
                }),
                style_idx: 0,
                display_text: "Account-998877".to_string(),
                num_fmt_code: "General".to_string(),
                display_color: None,
                is_concealed: false,
                hyperlink: Some("https://internal.example/998877".to_string()),
            },
        );
        sheet.comments.insert(
            (1, 1),
            SheetComment {
                author: "Auditor".to_string(),
                text: "Secret account 998877 note".to_string(),
                replies: Vec::new(),
            },
        );
        wb.sheets.push(sheet);

        let mut sidecar = AnnotationSidecar::new(b"xlsx-bytes");
        sidecar.add(Annotation {
            id: "cell-red-1".to_string(),
            kind: AnnotationKind::Redaction {
                reason_code: "PII".to_string(),
                label: "[REDACTED]".to_string(),
                fill_color: None,
            },
            anchor: cell_range_anchor("Financials", 1, 1, 1, 1),
            author: None,
            created_at: None,
            proposed: false,
        });

        let log = apply_workbook_redactions(&mut wb, &sidecar);
        assert_eq!(log.total_applied, 1);

        let cell = wb.sheets[0].cells.get(&(1, 1)).expect("cell B2");
        assert_eq!(cell.display_text, "[REDACTED]");
        assert_eq!(cell.value.raw_string(), "[REDACTED]");
        assert!(cell.formula.is_none());
        assert!(cell.hyperlink.is_none());
        assert!(!wb.sheets[0].comments.contains_key(&(1, 1)));
    }

    fn approved_rect_redaction(id: &str, rect: ReviewRect) -> Annotation {
        Annotation {
            id: id.to_string(),
            kind: AnnotationKind::Redaction {
                reason_code: "PII".to_string(),
                label: String::new(),
                fill_color: Some("#000000".to_string()),
            },
            anchor: AnnotationAnchor {
                target: RedactionTarget::PageRect,
                page_index: 0,
                rects_pt: vec![rect],
                text_anchor: None,
            },
            author: None,
            created_at: None,
            proposed: false,
        }
    }

    #[test]
    fn flipped_image_redaction_zeroes_the_pixels_under_the_box() {
        let raw = b"flipped image source";
        let mut layout = build_sample_layout("x", 50.0);
        let red = vec![[250u8, 20, 20, 255]; 40 * 40].concat();
        let img = layout
            .images
            .register("image/x-rgba8", 40, 40, red, &LayoutOptions::default())
            .expect("register image");
        // Drawn upside down, as PDF images are: local y 200..280 lands on page y 280..200.
        layout.pages[0]
            .items
            .push(Item::Image(prism_layout::ImageItem {
                image: img,
                rect: prism_layout::Rect::new(72.0, 200.0, 80.0, 80.0),
                transform: Affine {
                    coeffs: [1.0, 0.0, 0.0, -1.0, 0.0, 480.0],
                },
                opacity: 1.0,
                alt_text: None,
            }));

        // Cover the top quarter of the image as it appears on the page.
        let mut sidecar = AnnotationSidecar::new(raw);
        sidecar.add(approved_rect_redaction(
            "img",
            ReviewRect::new(72.0, 200.0, 80.0, 20.0),
        ));
        let resolved = sidecar
            .resolve_against(raw, ENGINE_VERSION, &layout)
            .expect("resolve");
        apply_redactions(&mut layout, &sidecar.document_sha256, &resolved).expect("apply");

        let Some(Item::Image(item)) = layout.pages[0]
            .items
            .iter()
            .find(|i| matches!(i, Item::Image(_)))
        else {
            panic!("partly covered image should be kept");
        };
        let entry = layout.images.get(item.image).expect("scrubbed image");
        let pixels = image::load_from_memory(&entry.data)
            .expect("decode")
            .to_rgba8();
        // The page's top quarter is the bottom ten pixel rows of the flipped image.
        assert_eq!(pixels.get_pixel(20, 35).0, [0, 0, 0, 255]);
        assert_eq!(pixels.get_pixel(20, 5).0, [250, 20, 20, 255]);
    }

    #[test]
    fn region_verification_rejects_glyphs_whose_letters_match_the_label() {
        // "trace" uses only letters found in "REDACTED"; it must still fail when it survives
        // inside a box. Characters are 7pt wide from x = 72, so the word spans x 72..107.
        let layout = build_sample_layout("trace", 120.0);
        let pdf = prism_render::pdf::export_document_pdf(
            &layout,
            &prism_render::pdf::PdfExportOptions::default(),
        )
        .expect("pdf");
        let entry = RedactionLogEntry {
            annotation_id: "leak".to_string(),
            reason_code: "PII".to_string(),
            label: "REDACTED".to_string(),
            target: RedactionTarget::PageRect,
            page_index: 0,
            rects_pt: vec![ReviewRect::new(70.0, 105.0, 40.0, 20.0)],
            glyphs_removed: 0,
            images_scrubbed: 0,
        };
        assert!(verify_redacted_regions(&pdf, Some("pdf"), &[entry]).is_err());

        // The label itself, drawn inside its box, passes.
        let labelled = build_sample_layout("REDACTED", 120.0);
        let pdf = prism_render::pdf::export_document_pdf(
            &labelled,
            &prism_render::pdf::PdfExportOptions::default(),
        )
        .expect("pdf");
        let entry = RedactionLogEntry {
            annotation_id: "label".to_string(),
            reason_code: "PII".to_string(),
            label: "REDACTED".to_string(),
            target: RedactionTarget::PageRect,
            page_index: 0,
            rects_pt: vec![ReviewRect::new(70.0, 105.0, 60.0, 20.0)],
            glyphs_removed: 0,
            images_scrubbed: 0,
        };
        verify_redacted_regions(&pdf, Some("pdf"), &[entry]).expect("label is allowed");
    }

    #[test]
    fn rectangle_only_redaction_records_the_removed_text() {
        let raw = b"rect redaction source";
        // Characters are 7pt wide from x = 72, so "CLASSIFIED-OMEGA" spans x 170..282.
        let mut layout = build_sample_layout("Public header CLASSIFIED-OMEGA public footer", 120.0);
        let mut sidecar = AnnotationSidecar::new(raw);
        sidecar.add(approved_rect_redaction(
            "rect",
            ReviewRect::new(170.0, 105.0, 112.0, 20.0),
        ));
        let resolved = sidecar
            .resolve_against(raw, ENGINE_VERSION, &layout)
            .expect("resolve");
        let log =
            apply_redactions(&mut layout, &sidecar.document_sha256, &resolved).expect("apply");

        assert_eq!(log.redacted_strings, vec!["CLASSIFIED-OMEGA".to_string()]);
        assert_eq!(
            layout.pages[0].text().trim(),
            "Public header  public footer"
        );

        // A leak is caught, and the error does not repeat the redacted text.
        let err = verify_redacted_output(
            b"left over CLASSIFIED-OMEGA",
            Some("txt"),
            &log.redacted_strings,
        )
        .expect_err("leak must fail verification");
        assert!(!err.to_string().contains("CLASSIFIED"));
        // PDF text strings are UTF-16BE.
        let utf16be: Vec<u8> = "CLASSIFIED-OMEGA"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        assert!(verify_redacted_output(&utf16be, None, &log.redacted_strings).is_err());
    }

    #[test]
    fn filesystem_cache_roundtrip_and_eviction() {
        let temp_dir = std::env::temp_dir().join("prism_review_cache_test");
        let _ = std::fs::remove_dir_all(&temp_dir);

        let cache = FilesystemCache::new(&temp_dir, 512).expect("create cache");
        let k1 = CacheKey::new(b"doc-1", "{}", CacheArtifactKind::ExtractedText);
        let k2 = CacheKey::new(b"doc-2", "{}", CacheArtifactKind::ExtractedText);

        let payload1 = vec![b'A'; 180];
        let payload2 = vec![b'B'; 180];

        cache.put(&k1, &payload1).expect("put k1");
        assert_eq!(
            cache.get(&k1).expect("get k1").as_deref(),
            Some(payload1.as_slice())
        );

        // Adding k2 exceeds 512 bytes (2 * (180 + 64) = 488 <= 512, adding k3 will evict k1)
        cache.put(&k2, &payload2).expect("put k2");
        let k3 = CacheKey::new(b"doc-3", "{}", CacheArtifactKind::ExtractedText);
        cache.put(&k3, &payload2).expect("put k3");

        assert!(cache.total_bytes().expect("total_bytes") <= 512);
        assert_eq!(
            cache.get(&k3).expect("get k3").as_deref(),
            Some(payload2.as_slice())
        );

        cache.clear().expect("clear");
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
