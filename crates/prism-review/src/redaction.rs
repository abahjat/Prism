// SPDX-License-Identifier: AGPL-3.0-only
//! Destructive redaction engine (`apply_redactions`, `apply_workbook_redactions`, `scrub_metadata`),
//! audit logging (`RedactionLog`), and post-export byte-level verification (`verify_redacted_output`).
//!
//! Redaction in Prism is **destructive by design**:
//! - Intersecting glyphs and clusters are removed from [`GlyphRun`]s, and partially overlapping runs
//!   are split into disjoint runs with freshly constructed `text`, `glyphs`, and `clusters` so zero
//!   redacted characters remain in text layers or `ToUnicode` maps.
//! - Intersecting regions of [`ImageItem`]s have their underlying pixels overwritten in [`ImageStore`]
//!   and re-encoded before export, and unreferenced original images are purged from [`ImageStore`].
//! - Intersecting [`LinkItem`]s and [`AnchorItem`]s are removed.
//! - Spreadsheet cells have their value, display text, formula, and comments removed.
//! - Document metadata fields matching [`RedactionTarget::MetadataField`] or containing redacted strings
//!   are scrubbed.
//! - [`verify_redacted_output`] re-parses exported bytes (including decompressed PDF streams) and fails
//!   the export if any redacted string survives.

use crate::sidecar::{
    AnchorResolutionStatus, AnnotationAnchor, AnnotationKind, AnnotationSidecar, RedactionTarget,
    ResolvedAnnotation, build_box_label_item, rect_path_item,
};
use image::{GenericImageView, ImageBuffer, RgbaImage};
use prism_core::ENGINE_VERSION;
use prism_core::error::{Error, Result};
use prism_core::metadata::{Metadata, MetadataValue};
use prism_core::review::ReviewRect;
use prism_excel::model::{Cell, CellValue, Sheet, Workbook, col_index_to_letters, parse_a1_range};
use prism_layout::{
    Affine, Cluster, FontSourceKind, FontStore, GlyphRun, GroupItem, ImageEntry, ImageId,
    ImageItem, ImageStore, Item, LayoutDocument, LayoutOptions, Rect, Rgba, interpret_pdf_bytes,
};
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_render::raster::{RasterOptions, export_document_tiff, render_page_png};
use prism_render::svg::render_page_svg;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Cursor;

const BUNDLED_SANS_BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf");

/// Audit log produced when applying destructive redactions to a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactionLog {
    /// Hex SHA-256 of the original unmodified source document.
    pub document_sha256: String,
    /// Prism engine version that applied the redactions.
    pub engine_version: String,
    /// Total number of approved redaction annotations applied.
    pub total_applied: usize,
    /// Per-redaction audit entries.
    pub entries: Vec<RedactionLogEntry>,
    /// Distinct non-empty strings removed during redaction (used by [`verify_redacted_output`]).
    pub redacted_strings: Vec<String>,
}

/// A single applied redaction entry in a [`RedactionLog`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactionLogEntry {
    /// Annotation identifier from [`AnnotationSidecar`].
    pub annotation_id: String,
    /// Structured reason code (e.g. `"PII"`, `"PRIV"`).
    pub reason_code: String,
    /// Visible stamp label.
    pub label: String,
    /// Target descriptor (`TextRange`, `PageRect`, `WholePage`, `CellRange`, `MetadataField`).
    pub target: RedactionTarget,
    /// Effective 0-based page index.
    pub page_index: u32,
    /// Effective bounding rectangles in points.
    pub rects_pt: Vec<ReviewRect>,
    /// Number of glyphs destructively removed by this redaction.
    pub glyphs_removed: usize,
    /// Number of raster images whose pixels were destructively overwritten.
    pub images_scrubbed: usize,
}

/// Output format for [`export_redacted_document`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RedactedExportFormat {
    /// Multi-page searchable PDF 1.7.
    Pdf,
    /// Multi-page TIFF raster image.
    Tiff,
    /// Single-page PNG raster image (`page_index` is 0-based).
    Png {
        /// Zero-based page index.
        page_index: u32,
    },
    /// Single-page SVG vector/text document (`page_index` is 0-based).
    Svg {
        /// Zero-based page index.
        page_index: u32,
    },
    /// Extracted UTF-8 plain text across all pages.
    Text,
}

/// Result of [`export_redacted_document`], verified by [`verify_redacted_output`].
#[derive(Debug, Clone)]
pub struct RedactedExportResult {
    /// Verified exported document bytes.
    pub bytes: Vec<u8>,
    /// Structured redaction audit log.
    pub log: RedactionLog,
}

/// Apply all approved (`proposed == false`) redactions in `resolved` destructively to `doc`.
///
/// Cell-range and metadata targets are only logged here: cells must be scrubbed in the
/// workbook with [`apply_workbook_redactions`] before it is laid out (as
/// [`export_redacted_document`] does), and metadata with [`scrub_metadata`].
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if an approved redaction has status [`AnchorResolutionStatus::Unresolved`]
/// (refusing to export a partially-unresolved redaction set silently).
pub fn apply_redactions(
    doc: &mut LayoutDocument,
    document_sha256: &str,
    resolved: &[ResolvedAnnotation],
) -> Result<RedactionLog> {
    // Refuse to export if any approved redaction failed to resolve
    for res in resolved {
        if !res.annotation.proposed
            && matches!(res.annotation.kind, AnnotationKind::Redaction { .. })
            && let AnchorResolutionStatus::Unresolved { ref reason } = res.status
        {
            return Err(Error::InvalidInput(format!(
                "Cannot apply redaction '{}': anchor unresolved ({reason})",
                res.annotation.id
            )));
        }
    }

    let label_font_id = doc.fonts.register_ttf(
        "Liberation Sans",
        700,
        false,
        FontSourceKind::Bundled,
        BUNDLED_SANS_BOLD.to_vec(),
        0,
    );

    let mut entries = Vec::new();
    let mut redacted_strings_set: BTreeSet<String> = BTreeSet::new();
    let mut labels_set: BTreeSet<String> = BTreeSet::new();

    for res in resolved {
        if res.annotation.proposed {
            continue;
        }
        let AnnotationKind::Redaction {
            ref reason_code,
            ref label,
            ref fill_color,
        } = res.annotation.kind
        else {
            continue;
        };

        if !label.trim().is_empty() {
            labels_set.insert(label.trim().to_lowercase());
        }
        if !reason_code.trim().is_empty() {
            labels_set.insert(reason_code.trim().to_lowercase());
        }

        if let Some(ref ta) = res.annotation.anchor.text_anchor {
            record_redacted_string(&mut redacted_strings_set, &ta.quoted_text);
        }

        let fill = fill_color
            .as_deref()
            .and_then(Rgba::from_css_str)
            .unwrap_or(Rgba::BLACK);

        match &res.annotation.anchor.target {
            RedactionTarget::WholePage { page_index } => {
                let target_page = *page_index;
                let mut glyphs_removed = 0usize;
                let mut images_scrubbed = 0usize;
                let mut page_rect = Vec::new();

                if let Some(page) = doc.pages.iter_mut().find(|p| p.index == target_page) {
                    let page_text = page.text();
                    for line in page_text.lines() {
                        record_redacted_string(&mut redacted_strings_set, line);
                    }
                    glyphs_removed = count_glyphs_in_items(&page.items);
                    images_scrubbed = count_images_in_items(&page.items);
                    let full_rect = ReviewRect::new(0.0, 0.0, page.size.width, page.size.height);
                    page_rect.push(full_rect);

                    page.items.clear();
                    page.items
                        .push(Item::Path(rect_path_item(full_rect, Some(fill), None)));
                    if !label.is_empty() {
                        page.items.push(build_box_label_item(
                            &doc.fonts,
                            label_font_id,
                            full_rect,
                            label,
                            Rgba::WHITE,
                            target_page,
                        ));
                    }
                }

                entries.push(RedactionLogEntry {
                    annotation_id: res.annotation.id.clone(),
                    reason_code: reason_code.clone(),
                    label: label.clone(),
                    target: res.annotation.anchor.target.clone(),
                    page_index: target_page,
                    rects_pt: page_rect,
                    glyphs_removed,
                    images_scrubbed,
                });
            }
            RedactionTarget::TextRange | RedactionTarget::PageRect => {
                let target_page = res.effective_page_index;
                let rects = res.effective_rects_pt.clone();
                let mut glyphs_removed = 0usize;
                let mut images_scrubbed = 0usize;

                if let Some(page) = doc.pages.iter_mut().find(|p| p.index == target_page) {
                    let mut ctx = PageRedactionContext {
                        fonts: &doc.fonts,
                        images: &mut doc.images,
                        rects: &rects,
                        fill,
                        redacted_strings: &mut redacted_strings_set,
                        glyphs_removed: &mut glyphs_removed,
                        images_scrubbed: &mut images_scrubbed,
                    };
                    page.items =
                        redact_items(std::mem::take(&mut page.items), Affine::IDENTITY, &mut ctx);

                    // If a TextAnchor quoted string still appears on the page (e.g. due to slight font metrics shift),
                    // also scrub any glyph run containing the exact quoted text.
                    if let Some(ref ta) = res.annotation.anchor.text_anchor
                        && !ta.quoted_text.trim().is_empty()
                    {
                        scrub_quoted_text_from_items(
                            &mut page.items,
                            &ta.quoted_text,
                            &mut glyphs_removed,
                        );
                    }

                    // Burn in the opaque redaction rectangles and labels
                    for r in &rects {
                        page.items
                            .push(Item::Path(rect_path_item(*r, Some(fill), None)));
                        if !label.is_empty() {
                            page.items.push(build_box_label_item(
                                &doc.fonts,
                                label_font_id,
                                *r,
                                label,
                                Rgba::WHITE,
                                target_page,
                            ));
                        }
                    }
                }

                entries.push(RedactionLogEntry {
                    annotation_id: res.annotation.id.clone(),
                    reason_code: reason_code.clone(),
                    label: label.clone(),
                    target: res.annotation.anchor.target.clone(),
                    page_index: target_page,
                    rects_pt: rects,
                    glyphs_removed,
                    images_scrubbed,
                });
            }
            RedactionTarget::CellRange { .. } | RedactionTarget::MetadataField { .. } => {
                entries.push(RedactionLogEntry {
                    annotation_id: res.annotation.id.clone(),
                    reason_code: reason_code.clone(),
                    label: label.clone(),
                    target: res.annotation.anchor.target.clone(),
                    page_index: res.effective_page_index,
                    rects_pt: res.effective_rects_pt.clone(),
                    glyphs_removed: 0,
                    images_scrubbed: 0,
                });
            }
        }
    }

    // Purge any unreferenced images from `doc.images` so original unredacted image bytes cannot survive
    purge_unreferenced_images(doc);

    // Scrub any remaining LinkItem or AnchorItem whose target/name contains a redacted string
    let redacted_strings: Vec<String> = redacted_strings_set
        .into_iter()
        .filter(|s| !labels_set.contains(&s.to_lowercase()))
        .collect();
    for page in &mut doc.pages {
        scrub_links_and_anchors(&mut page.items, &redacted_strings);
    }

    Ok(RedactionLog {
        document_sha256: document_sha256.to_string(),
        engine_version: ENGINE_VERSION.to_string(),
        total_applied: entries.len(),
        entries,
        redacted_strings,
    })
}

/// Apply all approved [`RedactionTarget::CellRange`] and text redactions in `sidecar` destructively
/// to an Excel [`Workbook`], removing cell values, display strings, formulas, and comments.
pub fn apply_workbook_redactions(
    workbook: &mut Workbook,
    sidecar: &AnnotationSidecar,
) -> RedactionLog {
    let mut entries = Vec::new();
    let mut redacted_strings_set = BTreeSet::new();
    let mut labels_set = BTreeSet::new();

    for ann in &sidecar.annotations {
        if ann.proposed {
            continue;
        }
        let AnnotationKind::Redaction {
            ref reason_code,
            ref label,
            ..
        } = ann.kind
        else {
            continue;
        };

        let stamp = if label.trim().is_empty() {
            "[REDACTED]".to_string()
        } else {
            label.clone()
        };
        labels_set.insert(stamp.trim().to_lowercase());

        match &ann.anchor.target {
            RedactionTarget::CellRange {
                sheet_name,
                cell_range,
            } => {
                let mut cells_scrubbed = 0usize;
                if let Some(range) = parse_a1_range(cell_range) {
                    for sheet in &mut workbook.sheets {
                        if sheet_name != "*" && !sheet.name.eq_ignore_ascii_case(sheet_name) {
                            continue;
                        }
                        cells_scrubbed += redact_sheet_range(
                            sheet,
                            range.start_row,
                            range.start_col,
                            range.end_row,
                            range.end_col,
                            &stamp,
                            &mut redacted_strings_set,
                        );
                    }
                }
                entries.push(RedactionLogEntry {
                    annotation_id: ann.id.clone(),
                    reason_code: reason_code.clone(),
                    label: stamp,
                    target: ann.anchor.target.clone(),
                    page_index: ann.anchor.page_index,
                    rects_pt: ann.anchor.rects_pt.clone(),
                    glyphs_removed: cells_scrubbed,
                    images_scrubbed: 0,
                });
            }
            RedactionTarget::MetadataField { field_name } => {
                scrub_workbook_metadata_field(workbook, field_name, &mut redacted_strings_set);
                entries.push(RedactionLogEntry {
                    annotation_id: ann.id.clone(),
                    reason_code: reason_code.clone(),
                    label: stamp,
                    target: ann.anchor.target.clone(),
                    page_index: 0,
                    rects_pt: Vec::new(),
                    glyphs_removed: 0,
                    images_scrubbed: 0,
                });
            }
            _ => {
                if let Some(ref ta) = ann.anchor.text_anchor
                    && !ta.quoted_text.trim().is_empty()
                {
                    record_redacted_string(&mut redacted_strings_set, &ta.quoted_text);
                    let mut cells_scrubbed = 0usize;
                    for sheet in &mut workbook.sheets {
                        for cell in sheet.cells.values_mut() {
                            let raw = cell.value.raw_string();
                            if contains_case_insensitive(&cell.display_text, &ta.quoted_text)
                                || contains_case_insensitive(&raw, &ta.quoted_text)
                                || cell.formula.as_ref().is_some_and(|f| {
                                    contains_case_insensitive(&f.expression, &ta.quoted_text)
                                })
                            {
                                record_redacted_string(
                                    &mut redacted_strings_set,
                                    &cell.display_text,
                                );
                                record_redacted_string(&mut redacted_strings_set, &raw);
                                if let Some(ref f) = cell.formula {
                                    record_redacted_string(
                                        &mut redacted_strings_set,
                                        &f.expression,
                                    );
                                }
                                cell.value = CellValue::Text(stamp.clone());
                                cell.display_text.clone_from(&stamp);
                                cell.formula = None;
                                cells_scrubbed += 1;
                            }
                        }
                        sheet.comments.retain(|_, c| {
                            if contains_case_insensitive(&c.text, &ta.quoted_text)
                                || contains_case_insensitive(&c.author, &ta.quoted_text)
                            {
                                record_redacted_string(&mut redacted_strings_set, &c.text);
                                false
                            } else {
                                true
                            }
                        });
                    }
                    entries.push(RedactionLogEntry {
                        annotation_id: ann.id.clone(),
                        reason_code: reason_code.clone(),
                        label: stamp,
                        target: ann.anchor.target.clone(),
                        page_index: ann.anchor.page_index,
                        rects_pt: ann.anchor.rects_pt.clone(),
                        glyphs_removed: cells_scrubbed,
                        images_scrubbed: 0,
                    });
                }
            }
        }
    }

    // Scrub defined names referencing any redacted string
    let redacted_strings: Vec<String> = redacted_strings_set
        .into_iter()
        .filter(|s| !labels_set.contains(&s.to_lowercase()))
        .collect();
    workbook.defined_names.retain(|dn| {
        !redacted_strings.iter().any(|rs| {
            contains_case_insensitive(&dn.name, rs) || contains_case_insensitive(&dn.formula, rs)
        })
    });

    RedactionLog {
        document_sha256: sidecar.document_sha256.clone(),
        engine_version: ENGINE_VERSION.to_string(),
        total_applied: entries.len(),
        entries,
        redacted_strings,
    }
}

/// Helper to create a cell-range redaction [`AnnotationAnchor`].
#[must_use]
pub fn cell_range_anchor(
    sheet_name: &str,
    start_row: u32,
    start_col: u32,
    end_row: u32,
    end_col: u32,
) -> AnnotationAnchor {
    let a1 = if start_row == end_row && start_col == end_col {
        format!("{}{}", col_index_to_letters(start_col), start_row + 1)
    } else {
        format!(
            "{}{}:{}{}",
            col_index_to_letters(start_col),
            start_row + 1,
            col_index_to_letters(end_col),
            end_row + 1
        )
    };
    AnnotationAnchor {
        target: RedactionTarget::CellRange {
            sheet_name: sheet_name.to_string(),
            cell_range: a1,
        },
        page_index: 0,
        rects_pt: Vec::new(),
        text_anchor: None,
    }
}

/// Scrub metadata fields targeted by [`RedactionTarget::MetadataField`] or containing any string
/// in `redacted_strings`, recording any newly removed field values in `redacted_strings`.
pub fn scrub_metadata(
    meta: &mut Metadata,
    sidecar: &AnnotationSidecar,
    redacted_strings: &mut Vec<String>,
) {
    let mut newly_removed = BTreeSet::new();

    for ann in &sidecar.annotations {
        if ann.proposed {
            continue;
        }
        if !matches!(ann.kind, AnnotationKind::Redaction { .. }) {
            continue;
        }
        if let RedactionTarget::MetadataField { ref field_name } = ann.anchor.target {
            match field_name.to_ascii_lowercase().as_str() {
                "title" => {
                    if let Some(v) = meta.title.take() {
                        record_redacted_string(&mut newly_removed, &v);
                    }
                }
                "author" | "creator" => {
                    if let Some(v) = meta.author.take() {
                        record_redacted_string(&mut newly_removed, &v);
                    }
                }
                "subject" => {
                    if let Some(v) = meta.subject.take() {
                        record_redacted_string(&mut newly_removed, &v);
                    }
                }
                "keywords" => {
                    for kw in std::mem::take(&mut meta.keywords) {
                        record_redacted_string(&mut newly_removed, &kw);
                    }
                }
                "language" => {
                    if let Some(v) = meta.language.take() {
                        record_redacted_string(&mut newly_removed, &v);
                    }
                }
                "all" | "*" => {
                    if let Some(v) = meta.title.take() {
                        record_redacted_string(&mut newly_removed, &v);
                    }
                    if let Some(v) = meta.author.take() {
                        record_redacted_string(&mut newly_removed, &v);
                    }
                    if let Some(v) = meta.subject.take() {
                        record_redacted_string(&mut newly_removed, &v);
                    }
                    for kw in std::mem::take(&mut meta.keywords) {
                        record_redacted_string(&mut newly_removed, &kw);
                    }
                    for (_, v) in std::mem::take(&mut meta.custom) {
                        if let MetadataValue::String(ref s) = v {
                            record_redacted_string(&mut newly_removed, s);
                        }
                    }
                }
                other => {
                    let keys: Vec<String> = meta
                        .custom
                        .keys()
                        .filter(|k| k.eq_ignore_ascii_case(other))
                        .cloned()
                        .collect();
                    for k in keys {
                        if let Some(MetadataValue::String(s)) = meta.custom.remove(&k) {
                            record_redacted_string(&mut newly_removed, &s);
                        }
                    }
                }
            }
        }
    }

    for s in newly_removed {
        if !redacted_strings
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&s))
        {
            redacted_strings.push(s);
        }
    }

    // Also scrub any metadata field whose value contains any redacted string
    let check_list = redacted_strings.clone();
    if let Some(ref title) = meta.title
        && check_list
            .iter()
            .any(|rs| contains_case_insensitive(title, rs))
    {
        meta.title = None;
    }
    if let Some(ref author) = meta.author
        && check_list
            .iter()
            .any(|rs| contains_case_insensitive(author, rs))
    {
        meta.author = None;
    }
    if let Some(ref subject) = meta.subject
        && check_list
            .iter()
            .any(|rs| contains_case_insensitive(subject, rs))
    {
        meta.subject = None;
    }
    meta.keywords.retain(|kw| {
        !check_list
            .iter()
            .any(|rs| contains_case_insensitive(kw, rs))
    });
    meta.custom.retain(|_, v| {
        let text = match v {
            MetadataValue::String(s) => s.clone(),
            MetadataValue::Integer(i) => i.to_string(),
            MetadataValue::Float(f) => f.to_string(),
            MetadataValue::Boolean(b) => b.to_string(),
            MetadataValue::DateTime(dt) => dt.to_rfc3339(),
        };
        !check_list
            .iter()
            .any(|rs| contains_case_insensitive(&text, rs))
    });
}

/// Automatically re-parse and scan `exported_bytes` (including raw bytes, UTF-16 bytes, decompressed
/// PDF streams, and Prism's own PDF/text parser) to verify that every string in `redacted_strings`
/// is completely absent.
///
/// # Errors
///
/// Returns [`Error::Internal`] if any redacted string is found in `exported_bytes`.
pub fn verify_redacted_output(
    exported_bytes: &[u8],
    format_hint: Option<&str>,
    redacted_strings: &[String],
) -> Result<()> {
    let needles: Vec<String> = redacted_strings
        .iter()
        .map(|s| normalize_whitespace(s))
        .filter(|s| s.len() >= 2)
        .collect();

    if needles.is_empty() {
        return Ok(());
    }

    // 1. Raw byte scan (case-insensitive UTF-8/ASCII and UTF-16LE/BE)
    let raw_lossy = String::from_utf8_lossy(exported_bytes);
    let raw_norm = normalize_whitespace(&raw_lossy);
    // Error messages name the string by its position, never its text: they reach logs and
    // HTTP responses, and must not carry the content that was just removed.
    for (i, needle) in needles.iter().enumerate() {
        if contains_case_insensitive(&raw_norm, needle) {
            return Err(Error::Internal(format!(
                "Redaction verification failed: found {} in raw output bytes",
                describe_needle(i, needle)
            )));
        }
        // UTF-16 checks (UTF-16BE is how PDF text strings are stored)
        let utf16le: Vec<u8> = needle.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let utf16be: Vec<u8> = needle.encode_utf16().flat_map(u16::to_be_bytes).collect();
        if utf16le.len() >= 4
            && (find_subslice(exported_bytes, &utf16le) || find_subslice(exported_bytes, &utf16be))
        {
            return Err(Error::Internal(format!(
                "Redaction verification failed: found UTF-16 {} in raw output bytes",
                describe_needle(i, needle)
            )));
        }
    }

    // 2. Format-specific deep verification
    let is_pdf = exported_bytes.starts_with(b"%PDF-")
        || format_hint.is_some_and(|h| h.eq_ignore_ascii_case("pdf"));

    if is_pdf {
        // 2a. Inspect every decompressed stream and dictionary in lopdf
        if let Ok(lopdf_doc) = lopdf::Document::load_mem(exported_bytes) {
            for (&obj_id, obj) in &lopdf_doc.objects {
                if let lopdf::Object::Stream(stream) = obj {
                    let decompressed = stream
                        .decompressed_content()
                        .unwrap_or_else(|_| stream.content.clone());
                    let text = String::from_utf8_lossy(&decompressed);
                    let norm = normalize_whitespace(&text);
                    for (i, needle) in needles.iter().enumerate() {
                        if contains_case_insensitive(&norm, needle) {
                            return Err(Error::Internal(format!(
                                "Redaction verification failed: found {} in decompressed PDF stream {obj_id:?}",
                                describe_needle(i, needle)
                            )));
                        }
                    }
                } else {
                    let debug_str = format!("{obj:?}");
                    let norm = normalize_whitespace(&debug_str);
                    for (i, needle) in needles.iter().enumerate() {
                        if contains_case_insensitive(&norm, needle) {
                            return Err(Error::Internal(format!(
                                "Redaction verification failed: found {} in PDF object {obj_id:?}",
                                describe_needle(i, needle)
                            )));
                        }
                    }
                }
            }
        }

        // 2b. Re-interpret the exported PDF with `interpret_pdf_bytes` and check extracted text on every page
        let layout = interpret_pdf_bytes(exported_bytes, &LayoutOptions::default())?;
        for page in &layout.pages {
            let page_text = normalize_whitespace(&page.text());
            for (i, needle) in needles.iter().enumerate() {
                if contains_case_insensitive(&page_text, needle) {
                    return Err(Error::Internal(format!(
                        "Redaction verification failed: Prism PDF interpreter extracted {} on page {}",
                        describe_needle(i, needle),
                        page.index + 1
                    )));
                }
            }
        }
    }

    Ok(())
}

/// Whether `sidecar` holds an approved redaction of a spreadsheet cell range.
#[must_use]
pub fn has_approved_cell_redactions(sidecar: &AnnotationSidecar) -> bool {
    sidecar.annotations.iter().any(|a| {
        !a.proposed
            && matches!(a.kind, AnnotationKind::Redaction { .. })
            && matches!(a.anchor.target, RedactionTarget::CellRange { .. })
    })
}

fn parse_workbook(bytes: &[u8]) -> Result<Workbook> {
    let limit = LayoutOptions::default().max_decompressed_stream_bytes;
    if prism_excel::is_xlsx_zip(bytes) {
        prism_excel::parse_xlsx(bytes, limit)
    } else if prism_excel::is_xls_bytes(bytes) {
        prism_excel::parse_xls(bytes, limit)
    } else {
        Err(Error::InvalidInput(
            "Cell-range redactions need a spreadsheet (XLSX or XLS) document".to_string(),
        ))
    }
}

fn describe_needle(index: usize, needle: &str) -> String {
    format!(
        "redacted string #{} ({} characters)",
        index + 1,
        needle.chars().count()
    )
}

/// Apply all approved redactions in `sidecar` to `layout` and `metadata`, export to `format`,
/// and run [`verify_redacted_output`] before returning.
///
/// # Errors
///
/// Returns an [`Error`] if `sidecar` fails to resolve against `document_bytes`, if rendering fails,
/// or if [`verify_redacted_output`] detects any surviving redacted content.
pub fn export_redacted_document(
    document_bytes: &[u8],
    layout: &LayoutDocument,
    metadata: &Metadata,
    sidecar: &AnnotationSidecar,
    format: RedactedExportFormat,
    dpi: f32,
) -> Result<RedactedExportResult> {
    // Cell-range redactions act on the workbook model, so the cells must be scrubbed before
    // layout; the page model alone cannot remove them.
    let (relaid, workbook_strings) = if has_approved_cell_redactions(sidecar) {
        let mut workbook = parse_workbook(document_bytes)?;
        let wb_log = apply_workbook_redactions(&mut workbook, sidecar);
        let relaid = prism_excel::layout_workbook(&workbook, &LayoutOptions::default())?;
        (Some(relaid), wb_log.redacted_strings)
    } else {
        (None, Vec::new())
    };
    let layout = relaid.as_ref().unwrap_or(layout);

    let resolved = sidecar.resolve_against(document_bytes, ENGINE_VERSION, layout)?;
    let mut redacted_doc = layout.clone();
    let mut redacted_meta = metadata.clone();

    let mut log = apply_redactions(&mut redacted_doc, &sidecar.document_sha256, &resolved)?;
    for s in workbook_strings {
        if !log.redacted_strings.contains(&s) {
            log.redacted_strings.push(s);
        }
    }
    scrub_metadata(&mut redacted_meta, sidecar, &mut log.redacted_strings);

    let (bytes, hint) = match format {
        RedactedExportFormat::Pdf => {
            let pdf_bytes = export_document_pdf(&redacted_doc, &PdfExportOptions::default())?;
            (pdf_bytes, "pdf")
        }
        RedactedExportFormat::Tiff => {
            let opts = RasterOptions {
                dpi,
                ..RasterOptions::default()
            };
            let tiff_bytes = export_document_tiff(&redacted_doc, &opts)?;
            (tiff_bytes, "tiff")
        }
        RedactedExportFormat::Png { page_index } => {
            let page = redacted_doc
                .pages
                .iter()
                .find(|p| p.index == page_index)
                .ok_or_else(|| Error::InvalidInput(format!("Page {page_index} out of range")))?;
            let opts = RasterOptions {
                dpi,
                ..RasterOptions::default()
            };
            let png_bytes =
                render_page_png(page, &redacted_doc.fonts, &redacted_doc.images, &opts)?;
            (png_bytes, "png")
        }
        RedactedExportFormat::Svg { page_index } => {
            let page = redacted_doc
                .pages
                .iter()
                .find(|p| p.index == page_index)
                .ok_or_else(|| Error::InvalidInput(format!("Page {page_index} out of range")))?;
            let svg = render_page_svg(
                page,
                &redacted_doc.fonts,
                &redacted_doc.images,
                &LayoutOptions::default(),
            )?;
            (svg.into_bytes(), "svg")
        }
        RedactedExportFormat::Text => {
            let mut text = String::new();
            for (idx, page) in redacted_doc.pages.iter().enumerate() {
                if idx > 0 {
                    text.push_str("\n\x0C\n");
                }
                text.push_str(&page.text());
            }
            (text.into_bytes(), "txt")
        }
    };

    verify_redacted_output(&bytes, Some(hint), &log.redacted_strings)?;

    Ok(RedactedExportResult { bytes, log })
}

struct PageRedactionContext<'a> {
    fonts: &'a FontStore,
    images: &'a mut ImageStore,
    rects: &'a [ReviewRect],
    fill: Rgba,
    redacted_strings: &'a mut BTreeSet<String>,
    glyphs_removed: &'a mut usize,
    images_scrubbed: &'a mut usize,
}

fn redact_items(
    items: Vec<Item>,
    parent_transform: Affine,
    ctx: &mut PageRedactionContext<'_>,
) -> Vec<Item> {
    let mut out = Vec::with_capacity(items.len());

    for item in items {
        match item {
            Item::Glyphs(run) => {
                let split_runs = redact_glyph_run(run, parent_transform, ctx);
                for r in split_runs {
                    out.push(Item::Glyphs(r));
                }
            }
            Item::Image(img) => {
                if let Some(kept_img) = redact_image_item(img, parent_transform, ctx) {
                    out.push(Item::Image(kept_img));
                }
            }
            Item::Path(path) => {
                // Drop vector paths that lie completely inside a redaction rectangle
                let path_world = parent_transform.then(path.transform);
                if let Some(bbox) = path_bbox(&path.commands, path_world)
                    && ctx
                        .rects
                        .iter()
                        .any(|r| rect_contains_rect(to_layout_rect(*r), bbox))
                {
                    continue;
                }
                out.push(Item::Path(path));
            }
            Item::Group(mut group) => {
                let group_world = parent_transform.then(group.transform);
                group.children = redact_items(group.children, group_world, ctx);
                if !group.children.is_empty() {
                    out.push(Item::Group(group));
                }
            }
            Item::Link(link) => {
                let link_bbox = parent_transform.transform_rect_bbox(link.rect);
                if ctx
                    .rects
                    .iter()
                    .any(|r| rects_intersect(to_layout_rect(*r), link_bbox))
                {
                    record_redacted_string(ctx.redacted_strings, &link.target);
                    continue;
                }
                out.push(Item::Link(link));
            }
            Item::Anchor(anchor) => {
                let anchor_bbox = parent_transform.transform_rect_bbox(anchor.rect);
                if ctx
                    .rects
                    .iter()
                    .any(|r| rects_intersect(to_layout_rect(*r), anchor_bbox))
                {
                    continue;
                }
                out.push(Item::Anchor(anchor));
            }
        }
    }

    out
}

fn redact_glyph_run(
    run: GlyphRun,
    parent_transform: Affine,
    ctx: &mut PageRedactionContext<'_>,
) -> Vec<GlyphRun> {
    if run.glyphs.is_empty() && run.text.is_empty() {
        return Vec::new();
    }

    let run_world = parent_transform.then(run.transform);
    let (ascent_ratio, descent_ratio) = ctx
        .fonts
        .get(run.font)
        .map_or((0.8, 0.2), |f| (f.ascent, f.descent));
    let font_size = run.size.max(1.0);
    let ascent = font_size * ascent_ratio;
    let height = font_size * (ascent_ratio + descent_ratio);

    // If the run has clusters, filter at cluster granularity and split contiguous surviving clusters
    if !run.clusters.is_empty() {
        let mut kept_segments: Vec<Vec<&Cluster>> = Vec::new();
        let mut current_seg: Vec<&Cluster> = Vec::new();
        let mut removed_any = false;
        // Text of the current stretch of removed clusters. It is recorded for verification as
        // one string: single characters are too short to search for, so recording cluster by
        // cluster would leave a rectangle-only redaction unverified.
        let mut removed_text = String::new();

        for cluster in &run.clusters {
            let local_rect = Rect::new(cluster.x, -ascent, cluster.advance.max(0.5), height);
            let page_rect = run_world.transform_rect_bbox(local_rect);
            let hits = ctx
                .rects
                .iter()
                .any(|r| rects_intersect_shrunk(to_layout_rect(*r), page_rect));

            if hits {
                removed_any = true;
                let g_start = cluster.glyph_range.start as usize;
                let g_end = (cluster.glyph_range.end as usize).min(run.glyphs.len());
                *ctx.glyphs_removed += g_end.saturating_sub(g_start);

                let t_start = (cluster.text_range.start as usize).min(run.text.len());
                let t_end = (cluster.text_range.end as usize).min(run.text.len());
                if t_start < t_end
                    && let Some(slice) = run.text.get(t_start..t_end)
                {
                    removed_text.push_str(slice);
                }

                if !current_seg.is_empty() {
                    kept_segments.push(std::mem::take(&mut current_seg));
                }
            } else {
                record_removed_fragment(ctx.redacted_strings, &std::mem::take(&mut removed_text));
                current_seg.push(cluster);
            }
        }
        record_removed_fragment(ctx.redacted_strings, &removed_text);

        if !removed_any {
            return vec![run];
        }
        if !current_seg.is_empty() {
            kept_segments.push(current_seg);
        }

        let mut out_runs = Vec::with_capacity(kept_segments.len());
        for seg in kept_segments {
            let mut new_text = String::new();
            let mut new_glyphs = Vec::new();
            let mut new_clusters = Vec::new();

            for cl in seg {
                let t_start = (cl.text_range.start as usize).min(run.text.len());
                let t_end = (cl.text_range.end as usize).min(run.text.len());
                let sub_text = run.text.get(t_start..t_end).unwrap_or("");

                #[allow(clippy::cast_possible_truncation)]
                let new_t_start = new_text.len() as u32;
                new_text.push_str(sub_text);
                #[allow(clippy::cast_possible_truncation)]
                let new_t_end = new_text.len() as u32;

                #[allow(clippy::cast_possible_truncation)]
                let new_g_start = new_glyphs.len() as u32;
                let g_start = (cl.glyph_range.start as usize).min(run.glyphs.len());
                let g_end = (cl.glyph_range.end as usize).min(run.glyphs.len());
                if g_start < g_end {
                    new_glyphs.extend_from_slice(&run.glyphs[g_start..g_end]);
                }
                #[allow(clippy::cast_possible_truncation)]
                let new_g_end = new_glyphs.len() as u32;

                new_clusters.push(Cluster {
                    text_range: new_t_start..new_t_end,
                    glyph_range: new_g_start..new_g_end,
                    x: cl.x,
                    advance: cl.advance,
                });
            }

            if !new_glyphs.is_empty() {
                out_runs.push(GlyphRun {
                    font: run.font,
                    size: run.size,
                    color: run.color,
                    transform: run.transform,
                    glyphs: new_glyphs,
                    text: new_text,
                    clusters: new_clusters,
                    decorations: run.decorations.clone(),
                    source: run.source,
                });
            }
        }

        return out_runs;
    }

    // Fallback when a GlyphRun has glyphs but empty `clusters`: check each glyph
    let mut surviving_glyphs = Vec::new();
    let mut removed_any = false;
    for g in &run.glyphs {
        let local_rect = Rect::new(g.x, g.y - ascent, g.advance.max(0.5), height);
        let page_rect = run_world.transform_rect_bbox(local_rect);
        if ctx
            .rects
            .iter()
            .any(|r| rects_intersect_shrunk(to_layout_rect(*r), page_rect))
        {
            removed_any = true;
            *ctx.glyphs_removed += 1;
        } else {
            surviving_glyphs.push(*g);
        }
    }

    if !removed_any {
        return vec![run];
    }
    record_redacted_string(ctx.redacted_strings, &run.text);
    if surviving_glyphs.is_empty() {
        return Vec::new();
    }

    vec![GlyphRun {
        font: run.font,
        size: run.size,
        color: run.color,
        transform: run.transform,
        glyphs: surviving_glyphs,
        text: String::new(),
        clusters: Vec::new(),
        decorations: run.decorations,
        source: run.source,
    }]
}

fn redact_image_item(
    mut img_item: ImageItem,
    parent_transform: Affine,
    ctx: &mut PageRedactionContext<'_>,
) -> Option<ImageItem> {
    let img_world = parent_transform.then(img_item.transform);
    let page_bbox = img_world.transform_rect_bbox(img_item.rect);

    let intersecting: Vec<Rect> = ctx
        .rects
        .iter()
        .map(|r| to_layout_rect(*r))
        .filter(|r| rects_intersect(*r, page_bbox))
        .collect();

    if intersecting.is_empty() {
        return Some(img_item);
    }

    *ctx.images_scrubbed += 1;
    if let Some(ref alt) = img_item.alt_text {
        record_redacted_string(ctx.redacted_strings, alt);
    }
    img_item.alt_text = None;

    // If the entire image bbox is covered by a single redaction rect, remove the ImageItem completely
    if intersecting
        .iter()
        .any(|r| rect_contains_rect(*r, page_bbox))
    {
        return None;
    }

    // Partial intersection: decode image pixels, overwrite the intersecting pixel regions with opaque fill,
    // re-encode as PNG, and register a new ImageEntry in `ctx.images`.
    let Some(entry) = ctx.images.get(img_item.image).cloned() else {
        return None;
    };

    let Some(mut rgba_img) = decode_image_entry(&entry) else {
        // If the image cannot be decoded, drop it entirely rather than risk leaking pixels
        return None;
    };

    let img_w = rgba_img.width().max(1);
    let img_h = rgba_img.height().max(1);

    // Map each redaction rectangle back through the image's own transform (which may flip or
    // rotate it, as PDF images usually are) into pixel space, the inverse of how renderers draw
    // it: pixel (px, py) lands at `img_world * (rect.x + px * sx, rect.y + py * sy)`.
    let Some(inverse) = invert_affine(img_world) else {
        // A degenerate transform draws nothing useful; drop the image rather than guess.
        return None;
    };
    let local = img_item.rect;
    if local.width.abs() < f32::EPSILON || local.height.abs() < f32::EPSILON {
        return None;
    }

    for r in intersecting {
        let corners = [
            inverse.transform_point(prism_layout::Point::new(r.x, r.y)),
            inverse.transform_point(prism_layout::Point::new(r.right(), r.y)),
            inverse.transform_point(prism_layout::Point::new(r.x, r.bottom())),
            inverse.transform_point(prism_layout::Point::new(r.right(), r.bottom())),
        ];
        let us = corners.map(|p| ((p.x - local.x) / local.width).clamp(0.0, 1.0));
        let vs = corners.map(|p| ((p.y - local.y) / local.height).clamp(0.0, 1.0));
        let rel_min_x = us.iter().copied().fold(f32::INFINITY, f32::min);
        let rel_max_x = us.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let rel_min_y = vs.iter().copied().fold(f32::INFINITY, f32::min);
        let rel_max_y = vs.iter().copied().fold(f32::NEG_INFINITY, f32::max);

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let px_x0 = (rel_min_x * img_w as f32).floor() as u32;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let px_x1 = ((rel_max_x * img_w as f32).ceil() as u32).min(img_w);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let px_y0 = (rel_min_y * img_h as f32).floor() as u32;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let px_y1 = ((rel_max_y * img_h as f32).ceil() as u32).min(img_h);

        let pixel = image::Rgba([ctx.fill.r, ctx.fill.g, ctx.fill.b, 255]);
        for py in px_y0..px_y1 {
            for px in px_x0..px_x1 {
                rgba_img.put_pixel(px, py, pixel);
            }
        }
    }

    let mut png_bytes = Vec::new();
    if rgba_img
        .write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .is_err()
    {
        return None;
    }

    #[allow(clippy::cast_possible_truncation)]
    let next_id = ImageId(
        ctx.images
            .images
            .keys()
            .next_back()
            .map_or(0, |id| id.0 + 1),
    );
    ctx.images.images.insert(
        next_id,
        ImageEntry {
            id: next_id,
            mime_type: "image/png".to_string(),
            width: img_w,
            height: img_h,
            data: png_bytes,
        },
    );
    img_item.image = next_id;
    Some(img_item)
}

fn decode_image_entry(entry: &ImageEntry) -> Option<RgbaImage> {
    if entry.mime_type == "image/x-rgba8" {
        let expected = (entry.width as usize)
            .checked_mul(entry.height as usize)?
            .checked_mul(4)?;
        if entry.data.len() == expected {
            return ImageBuffer::from_raw(entry.width, entry.height, entry.data.clone());
        }
    }
    let dyn_img = image::load_from_memory(&entry.data).ok()?;
    let (w, h) = dyn_img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some(dyn_img.to_rgba8())
}

fn purge_unreferenced_images(doc: &mut LayoutDocument) {
    let mut referenced = BTreeSet::new();
    for page in &doc.pages {
        collect_referenced_images(&page.items, &mut referenced);
    }
    doc.images.images.retain(|id, _| referenced.contains(id));
}

fn collect_referenced_images(items: &[Item], out: &mut BTreeSet<ImageId>) {
    for item in items {
        match item {
            Item::Image(img) => {
                out.insert(img.image);
            }
            Item::Group(g) => collect_referenced_images(&g.children, out),
            _ => {}
        }
    }
}

fn scrub_quoted_text_from_items(items: &mut Vec<Item>, quoted: &str, glyphs_removed: &mut usize) {
    for item in items.iter_mut() {
        match item {
            Item::Glyphs(run) => {
                if contains_case_insensitive(&run.text, quoted) {
                    // Remove any cluster overlapping the quoted substring in `run.text`
                    let lower_run = run.text.to_lowercase();
                    let lower_q = quoted.to_lowercase();
                    if lower_run.len() == run.text.len()
                        && let Some(pos) = lower_run.find(&lower_q)
                    {
                        let end = pos + lower_q.len();
                        let mut new_text = String::new();
                        let mut new_glyphs = Vec::new();
                        let mut new_clusters = Vec::new();
                        for cl in &run.clusters {
                            let c_start = cl.text_range.start as usize;
                            let c_end = cl.text_range.end as usize;
                            if c_end > pos && c_start < end {
                                *glyphs_removed +=
                                    (cl.glyph_range.end - cl.glyph_range.start) as usize;
                                continue;
                            }
                            let sub = run.text.get(c_start..c_end).unwrap_or("");
                            #[allow(clippy::cast_possible_truncation)]
                            let nt_s = new_text.len() as u32;
                            new_text.push_str(sub);
                            #[allow(clippy::cast_possible_truncation)]
                            let nt_e = new_text.len() as u32;
                            #[allow(clippy::cast_possible_truncation)]
                            let ng_s = new_glyphs.len() as u32;
                            let gs = (cl.glyph_range.start as usize).min(run.glyphs.len());
                            let ge = (cl.glyph_range.end as usize).min(run.glyphs.len());
                            if gs < ge {
                                new_glyphs.extend_from_slice(&run.glyphs[gs..ge]);
                            }
                            #[allow(clippy::cast_possible_truncation)]
                            let ng_e = new_glyphs.len() as u32;
                            new_clusters.push(Cluster {
                                text_range: nt_s..nt_e,
                                glyph_range: ng_s..ng_e,
                                x: cl.x,
                                advance: cl.advance,
                            });
                        }
                        run.text = new_text;
                        run.glyphs = new_glyphs;
                        run.clusters = new_clusters;
                    } else {
                        *glyphs_removed += run.glyphs.len();
                        run.text.clear();
                        run.glyphs.clear();
                        run.clusters.clear();
                    }
                }
            }
            Item::Group(GroupItem { children, .. }) => {
                scrub_quoted_text_from_items(children, quoted, glyphs_removed);
            }
            _ => {}
        }
    }
    items.retain(|it| !matches!(it, Item::Glyphs(r) if r.glyphs.is_empty() && r.text.is_empty()));
}

fn scrub_links_and_anchors(items: &mut Vec<Item>, redacted_strings: &[String]) {
    items.retain_mut(|item| match item {
        Item::Link(link) => !redacted_strings
            .iter()
            .any(|rs| contains_case_insensitive(&link.target, rs)),
        Item::Anchor(anchor) => !redacted_strings
            .iter()
            .any(|rs| contains_case_insensitive(&anchor.name, rs)),
        Item::Image(img) => {
            if let Some(ref alt) = img.alt_text
                && redacted_strings
                    .iter()
                    .any(|rs| contains_case_insensitive(alt, rs))
            {
                img.alt_text = None;
            }
            true
        }
        Item::Group(g) => {
            scrub_links_and_anchors(&mut g.children, redacted_strings);
            !g.children.is_empty()
        }
        _ => true,
    });
}

fn redact_sheet_range(
    sheet: &mut Sheet,
    start_row: u32,
    start_col: u32,
    end_row: u32,
    end_col: u32,
    stamp: &str,
    redacted_strings: &mut BTreeSet<String>,
) -> usize {
    let mut scrubbed = 0usize;
    let keys: Vec<(u32, u32)> = sheet
        .cells
        .range((start_row, start_col)..=(end_row, end_col))
        .filter(|&(&(_, c), _)| c >= start_col && c <= end_col)
        .map(|(&k, _)| k)
        .collect();

    if keys.is_empty() && start_row == end_row && start_col == end_col {
        sheet.cells.insert(
            (start_row, start_col),
            Cell {
                value: CellValue::Text(stamp.to_string()),
                formula: None,
                style_idx: 0,
                display_text: stamp.to_string(),
                num_fmt_code: "General".to_string(),
                display_color: None,
                is_concealed: false,
                hyperlink: None,
            },
        );
        scrubbed += 1;
    } else {
        for key in keys {
            if let Some(cell) = sheet.cells.get_mut(&key) {
                record_redacted_string(redacted_strings, &cell.display_text);
                record_redacted_string(redacted_strings, &cell.value.raw_string());
                if let Some(ref f) = cell.formula {
                    record_redacted_string(redacted_strings, &f.expression);
                }
                if let Some(ref h) = cell.hyperlink {
                    record_redacted_string(redacted_strings, h);
                }
                cell.value = CellValue::Text(stamp.to_string());
                cell.display_text = stamp.to_string();
                cell.formula = None;
                cell.hyperlink = None;
                scrubbed += 1;
            }
        }
    }

    sheet.comments.retain(|&(r, c), comment| {
        if r >= start_row && r <= end_row && c >= start_col && c <= end_col {
            record_redacted_string(redacted_strings, &comment.text);
            record_redacted_string(redacted_strings, &comment.author);
            false
        } else {
            true
        }
    });

    scrubbed
}

fn scrub_workbook_metadata_field(
    workbook: &mut Workbook,
    field_name: &str,
    redacted_strings: &mut BTreeSet<String>,
) {
    match field_name.to_ascii_lowercase().as_str() {
        "title" => {
            if let Some(v) = workbook.title.take() {
                record_redacted_string(redacted_strings, &v);
            }
        }
        "author" | "creator" => {
            if let Some(v) = workbook.author.take() {
                record_redacted_string(redacted_strings, &v);
            }
        }
        "all" | "*" => {
            if let Some(v) = workbook.title.take() {
                record_redacted_string(redacted_strings, &v);
            }
            if let Some(v) = workbook.author.take() {
                record_redacted_string(redacted_strings, &v);
            }
        }
        _ => {}
    }
}

fn count_glyphs_in_items(items: &[Item]) -> usize {
    let mut total = 0usize;
    for item in items {
        match item {
            Item::Glyphs(r) => total += r.glyphs.len(),
            Item::Group(g) => total += count_glyphs_in_items(&g.children),
            _ => {}
        }
    }
    total
}

fn count_images_in_items(items: &[Item]) -> usize {
    let mut total = 0usize;
    for item in items {
        match item {
            Item::Image(_) => total += 1,
            Item::Group(g) => total += count_images_in_items(&g.children),
            _ => {}
        }
    }
    total
}

fn path_bbox(commands: &[prism_layout::PathCommand], transform: Affine) -> Option<Rect> {
    let mut pts = Vec::new();
    for cmd in commands {
        match *cmd {
            prism_layout::PathCommand::MoveTo(p) | prism_layout::PathCommand::LineTo(p) => {
                pts.push(transform.transform_point(p));
            }
            prism_layout::PathCommand::QuadTo { ctrl, to } => {
                pts.push(transform.transform_point(ctrl));
                pts.push(transform.transform_point(to));
            }
            prism_layout::PathCommand::CurveTo { ctrl1, ctrl2, to } => {
                pts.push(transform.transform_point(ctrl1));
                pts.push(transform.transform_point(ctrl2));
                pts.push(transform.transform_point(to));
            }
            prism_layout::PathCommand::Close => {}
        }
    }
    let first = *pts.first()?;
    let mut min_x = first.x;
    let mut max_x = first.x;
    let mut min_y = first.y;
    let mut max_y = first.y;
    for p in &pts[1..] {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
    }
    Some(Rect::new(
        min_x,
        min_y,
        (max_x - min_x).max(0.0),
        (max_y - min_y).max(0.0),
    ))
}

/// Inverse of an affine transform, or `None` when it is singular or not finite.
#[allow(clippy::many_single_char_names)]
fn invert_affine(t: Affine) -> Option<Affine> {
    let [a, b, c, d, tx, ty] = t.coeffs;
    let det = a * d - b * c;
    if !det.is_finite() || det.abs() < 1e-9 {
        return None;
    }
    let inv = Affine {
        coeffs: [
            d / det,
            -b / det,
            -c / det,
            a / det,
            (c * ty - d * tx) / det,
            (b * tx - a * ty) / det,
        ],
    };
    inv.coeffs.iter().all(|v| v.is_finite()).then_some(inv)
}

fn to_layout_rect(r: ReviewRect) -> Rect {
    Rect::new(r.x, r.y, r.width, r.height)
}

fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y
}

fn rects_intersect_shrunk(redaction_rect: Rect, glyph_rect: Rect) -> bool {
    // Shrink glyph_rect slightly by 10% on each axis so adjacent characters touching edge-to-edge
    // aren't accidentally clipped when redacting a tight word box.
    let pad_x = (glyph_rect.width * 0.15).min(1.0);
    let pad_y = (glyph_rect.height * 0.15).min(1.5);
    let shrunk = Rect::new(
        glyph_rect.x + pad_x,
        glyph_rect.y + pad_y,
        (glyph_rect.width - 2.0 * pad_x).max(0.1),
        (glyph_rect.height - 2.0 * pad_y).max(0.1),
    );
    rects_intersect(redaction_rect, shrunk)
}

fn rect_contains_rect(outer: Rect, inner: Rect) -> bool {
    outer.x <= inner.x + 0.5
        && outer.y <= inner.y + 0.5
        && outer.right() >= inner.right() - 0.5
        && outer.bottom() >= inner.bottom() - 0.5
}

fn record_redacted_string(set: &mut BTreeSet<String>, raw: &str) {
    let trimmed = normalize_whitespace(raw);
    if trimmed.len() >= 2 {
        set.insert(trimmed);
    }
}

/// Record text removed because it fell inside a redaction rectangle. Edges of a box can clip a
/// letter or two of a neighbouring word, and searching the whole output for such a fragment
/// would fail the export on any other occurrence, so fragments shorter than four characters are
/// not recorded. Quoted text anchors are recorded in full by [`record_redacted_string`].
fn record_removed_fragment(set: &mut BTreeSet<String>, raw: &str) {
    let normalized = normalize_whitespace(raw);
    if normalized.chars().count() >= 4 {
        set.insert(normalized);
    }
}

fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}
