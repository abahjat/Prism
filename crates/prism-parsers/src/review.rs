// SPDX-License-Identifier: AGPL-3.0-only
//! Cross-format hidden-content report providers for PDF, Email (`EML`, `MSG`, `MBOX`),
//! Archives (`ZIP`, `TAR`, `7z`), and the unified [`generate_hidden_content_report`] entry point.

#![allow(
    clippy::case_sensitive_file_extension_comparisons,
    clippy::chunks_exact_to_as_chunks,
    clippy::collapsible_if,
    clippy::map_unwrap_or,
    clippy::too_many_lines
)]

use lopdf::{Dictionary, Document as LopdfDocument, Object};
use prism_core::format::detect_format;
use prism_core::review::{
    Finding, FindingKind, HiddenContentReport, Location, ReviewRect, Severity,
};
use prism_excel::{
    DEFAULT_MAX_XLS_STREAM_BYTES, DEFAULT_MAX_XLSX_ENTRY_BYTES, is_xls_bytes, is_xlsx_zip,
    parse_xls, parse_xlsx,
};
use prism_layout::{LayoutDocument, LayoutOptions, interpret_pdf_bytes, layout_document};
use prism_ppt::{
    DEFAULT_MAX_PPT_STREAM_BYTES, DEFAULT_MAX_PPTX_ENTRY_BYTES, is_ppt_bytes, is_pptx_zip,
    layout_presentation, parse_ppt, parse_pptx,
};
use prism_word::{
    DEFAULT_MAX_DOC_STREAM_BYTES, DEFAULT_MAX_ENTRY_BYTES, is_doc_bytes, is_docx_zip,
    layout_word_document, parse_doc, parse_docx,
};
use std::collections::BTreeSet;
use std::io::{Cursor, Read};

/// Generate a [`HiddenContentReport`] for arbitrary document `bytes` and an optional `filename_hint`.
///
/// Automatically dispatches to the format-specific provider (`docx`, `doc`, `pptx`, `ppt`,
/// `xlsx`, `xls`, `pdf`, `eml`, `msg`, `mbox`, `zip`, `tar`, `7z`, or generic fallback).
#[must_use]
pub fn generate_hidden_content_report(
    bytes: &[u8],
    filename_hint: Option<&str>,
    precomputed_layout: Option<&LayoutDocument>,
) -> HiddenContentReport {
    let opts = LayoutOptions::default();

    // 1. Word (.docx / .doc)
    if is_docx_zip(bytes) {
        if let Ok(word_doc) = parse_docx(bytes, filename_hint, DEFAULT_MAX_ENTRY_BYTES) {
            let total_blocks: usize = word_doc.sections.iter().map(|s| s.blocks.len()).sum();
            let owned_layout;
            let layout_ref = if let Some(l) = precomputed_layout {
                Some(l)
            } else if total_blocks <= 64 {
                owned_layout = layout_word_document(&word_doc, &opts).ok();
                owned_layout.as_ref()
            } else {
                None
            };
            let mut report = word_doc.hidden_content_report("docx", layout_ref);
            inspect_ooxml_package_extras(bytes, &mut report);
            return report;
        }
    }
    if is_doc_bytes(bytes) {
        if let Ok(word_doc) = parse_doc(bytes, DEFAULT_MAX_DOC_STREAM_BYTES) {
            let owned_layout;
            let layout_ref = if let Some(l) = precomputed_layout {
                Some(l)
            } else {
                owned_layout = layout_word_document(&word_doc, &opts).ok();
                owned_layout.as_ref()
            };
            return word_doc.hidden_content_report("doc", layout_ref);
        }
    }

    // 2. PowerPoint (.pptx / .ppt)
    if is_pptx_zip(bytes) {
        if let Ok(pres) = parse_pptx(bytes, DEFAULT_MAX_PPTX_ENTRY_BYTES) {
            let owned_layout;
            let layout_ref = if let Some(l) = precomputed_layout {
                Some(l)
            } else {
                owned_layout = layout_presentation(&pres, &opts).ok();
                owned_layout.as_ref()
            };
            let mut report = pres.hidden_content_report("pptx", layout_ref);
            inspect_ooxml_package_extras(bytes, &mut report);
            return report;
        }
    }
    if is_ppt_bytes(bytes) {
        if let Ok(pres) = parse_ppt(bytes, DEFAULT_MAX_PPT_STREAM_BYTES) {
            let owned_layout;
            let layout_ref = if let Some(l) = precomputed_layout {
                Some(l)
            } else {
                owned_layout = layout_presentation(&pres, &opts).ok();
                owned_layout.as_ref()
            };
            return pres.hidden_content_report("ppt", layout_ref);
        }
    }

    // 3. Excel (.xlsx / .xls)
    if is_xlsx_zip(bytes) {
        if let Ok(wb) = parse_xlsx(bytes, DEFAULT_MAX_XLSX_ENTRY_BYTES) {
            let mut report = wb.hidden_content_report("xlsx");
            inspect_ooxml_package_extras(bytes, &mut report);
            return report;
        }
    }
    if is_xls_bytes(bytes) {
        if let Ok(wb) = parse_xls(bytes, DEFAULT_MAX_XLS_STREAM_BYTES) {
            return wb.hidden_content_report("xls");
        }
    }

    // 4. PDF
    if bytes.starts_with(b"%PDF-") {
        return pdf_hidden_content_report(bytes, precomputed_layout);
    }

    // 5. Detect remaining formats (Email, Archive, etc.)
    let ext = filename_hint
        .and_then(|n| n.rsplit('.').next())
        .map(str::to_ascii_lowercase);
    let detected = detect_format(bytes, filename_hint);
    let mime = detected
        .as_ref()
        .map(|d| d.format.mime_type.as_str())
        .unwrap_or("");

    if matches!(ext.as_deref(), Some("eml")) || mime == "message/rfc822" {
        return email_hidden_content_report(bytes, "eml");
    }
    if matches!(ext.as_deref(), Some("mbox")) || mime == "application/mbox" {
        return email_hidden_content_report(bytes, "mbox");
    }
    if matches!(ext.as_deref(), Some("msg")) || mime == "application/vnd.ms-outlook" {
        return msg_hidden_content_report(bytes);
    }
    if mime == "application/zip" || matches!(ext.as_deref(), Some("zip")) {
        return archive_hidden_content_report(bytes, "zip");
    }
    if mime == "application/x-tar" || matches!(ext.as_deref(), Some("tar")) {
        return archive_hidden_content_report(bytes, "tar");
    }
    if mime == "application/x-7z-compressed" || matches!(ext.as_deref(), Some("7z")) {
        return archive_hidden_content_report(bytes, "7z");
    }

    // Generic fallback
    let fmt_id = ext.as_deref().unwrap_or("txt");
    let mut report = HiddenContentReport::for_format(fmt_id);
    if let Ok(text) = std::str::from_utf8(bytes)
        && text.chars().any(|c| {
            matches!(
                c,
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}'
            )
        })
    {
        report.push(Finding::new(
            FindingKind::ZeroWidthCharacters,
            Severity::Notable,
            Location::Document,
            None,
            Some(text.to_string()),
            serde_json::json!({}),
        ));
    }
    if let Some(l) = precomputed_layout {
        for f in l.inspect_concealed_text(false) {
            report.push(f);
        }
    }
    report
}

/// Inspect an OOXML ZIP container for additional active/embedded parts (`activeX`, `embeddings`, `customXml`, `rsids`).
fn inspect_ooxml_package_extras(bytes: &[u8], report: &mut HiddenContentReport) {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return;
    };
    for i in 0..zip.len() {
        let Ok(mut file) = zip.by_index(i) else {
            continue;
        };
        let name = file.name().to_string();
        let lower = name.to_ascii_lowercase();
        if lower.contains("activex/") && lower.ends_with(".xml") {
            let duplicate = report.items.iter().any(|f| {
                f.kind == FindingKind::ActiveXControl
                    && matches!(&f.location, Location::PartPath { path } if path == &name)
            });
            if !duplicate {
                report.push(Finding::new(
                    FindingKind::ActiveXControl,
                    Severity::High,
                    Location::PartPath { path: name.clone() },
                    None,
                    Some(format!("ActiveX control part: {name}")),
                    serde_json::json!({ "part": name }),
                ));
            }
        } else if lower.contains("/embeddings/") {
            let duplicate = report.items.iter().any(|f| {
                matches!(
                    f.kind,
                    FindingKind::EmbeddedOleObject | FindingKind::EmbeddedFile
                ) && matches!(&f.location, Location::PartPath { path } if path == &name)
            });
            if !duplicate {
                report.push(Finding::new(
                    FindingKind::EmbeddedOleObject,
                    Severity::Notable,
                    Location::PartPath { path: name.clone() },
                    None,
                    Some(format!("Embedded object: {name}")),
                    serde_json::json!({ "part": name, "size": file.size() }),
                ));
            }
        } else if lower == "word/settings.xml" {
            if let Ok(xml) = crate::limits::read_bounded_string(&mut file, DEFAULT_MAX_ENTRY_BYTES)
            {
                let rsid_count =
                    xml.matches("<w:rsid ").count() + xml.matches("<w:rsidRoot ").count();
                if rsid_count > 0 {
                    report.push(Finding::new(
                        FindingKind::RsidDiversity,
                        Severity::Info,
                        Location::PartPath { path: name },
                        None,
                        Some(format!("{rsid_count} editing session RSID(s)")),
                        serde_json::json!({ "rsid_count": rsid_count }),
                    ));
                }
            }
        }
    }
}

/// Generate a [`HiddenContentReport`] for a PDF document.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn pdf_hidden_content_report(
    bytes: &[u8],
    precomputed_layout: Option<&LayoutDocument>,
) -> HiddenContentReport {
    let mut report = HiddenContentReport::for_format("pdf");

    // 1. Incremental PDF updates (`%%EOF` markers)
    let eof_count = count_subslice(bytes, b"%%EOF");
    if eof_count > 1 {
        report.push(Finding::new(
            FindingKind::IncrementalPdfUpdates,
            Severity::Notable,
            Location::Document,
            None,
            Some(format!(
                "PDF contains {eof_count} incremental %%EOF revisions"
            )),
            serde_json::json!({ "revision_count": eof_count }),
        ));
    }

    // 2. Inspect PDF object structure via lopdf
    if let Ok(mut pdf) = LopdfDocument::load_mem(bytes) {
        if pdf.is_encrypted() {
            let _ = pdf.decrypt("");
        }

        // 2a. Info dictionary metadata
        if let Some(info) = pdf
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|obj| deref_dict(&pdf, obj))
        {
            let author = dict_text(info, b"Author");
            let creator = dict_text(info, b"Creator");
            let producer = dict_text(info, b"Producer");
            let title = dict_text(info, b"Title");
            if author.is_some() || creator.is_some() || producer.is_some() {
                report.push(Finding::new(
                    FindingKind::AuthorAndModificationHistory,
                    Severity::Info,
                    Location::Document,
                    None,
                    author
                        .clone()
                        .or_else(|| creator.clone())
                        .or_else(|| producer.clone()),
                    serde_json::json!({
                        "author": author,
                        "creator": creator,
                        "producer": producer,
                        "title": title,
                    }),
                ));
            }
        }

        // 2b. Page-level annotations (/Annots)
        let pages = pdf.get_pages();
        for (page_num_1based, &page_id) in &pages {
            let page_index = page_num_1based.saturating_sub(1);
            let Ok(page_dict) = pdf.get_dictionary(page_id) else {
                continue;
            };
            let page_height = extract_page_height(&pdf, page_dict).unwrap_or(792.0);

            if let Ok(annots_obj) = page_dict.get(b"Annots")
                && let Some(annots_arr) = deref_array(&pdf, annots_obj)
            {
                for item in annots_arr {
                    let Some(annot_dict) = deref_dict(&pdf, item) else {
                        continue;
                    };
                    let subtype = annot_dict
                        .get(b"Subtype")
                        .and_then(Object::as_name)
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .unwrap_or_default();
                    let rect = extract_pdf_rect(annot_dict, page_height);
                    let rects = rect.into_iter().collect::<Vec<_>>();
                    let loc = Location::PageRects { page_index, rects };

                    if subtype == "Link" {
                        if let Ok(action_obj) = annot_dict.get(b"A")
                            && let Some(action_dict) = deref_dict(&pdf, action_obj)
                        {
                            inspect_pdf_action_dict(&pdf, action_dict, loc, &mut report);
                        }
                    } else if subtype != "Widget" {
                        let contents = dict_text(annot_dict, b"Contents");
                        let author = dict_text(annot_dict, b"T");
                        report.push(Finding::new(
                            FindingKind::PdfAnnotation,
                            Severity::Notable,
                            loc,
                            None,
                            contents
                                .clone()
                                .or_else(|| Some(format!("PDF {subtype} annotation"))),
                            serde_json::json!({
                                "subtype": subtype,
                                "author": author,
                                "contents": contents,
                                "page": page_num_1based,
                            }),
                        ));
                    }
                }
            }
        }

        // 2c. Global scan of objects for OCGs, JavaScript, Launch actions, EmbeddedFiles, and XMP streams
        let mut off_ocg_ids = BTreeSet::new();
        for (&obj_id, obj) in &pdf.objects {
            if let Object::Dictionary(dict) = obj {
                // Collect /OFF array from /OCProperties -> /D
                if let Ok(ocprops_obj) = dict.get(b"OCProperties")
                    && let Some(ocprops) = deref_dict(&pdf, ocprops_obj)
                    && let Ok(d_obj) = ocprops.get(b"D")
                    && let Some(d_dict) = deref_dict(&pdf, d_obj)
                    && let Ok(off_obj) = d_dict.get(b"OFF")
                    && let Some(off_arr) = deref_array(&pdf, off_obj)
                {
                    for o in off_arr {
                        if let Ok(id) = o.as_reference() {
                            off_ocg_ids.insert(id);
                        }
                    }
                }
                let _ = obj_id;
            }
        }

        for (&obj_id, obj) in &pdf.objects {
            match obj {
                Object::Dictionary(dict) => {
                    let obj_type = dict.get(b"Type").and_then(Object::as_name).unwrap_or(b"");
                    if obj_type == b"OCG" {
                        let name =
                            dict_text(dict, b"Name").unwrap_or_else(|| "Unnamed Layer".to_string());
                        let is_off = off_ocg_ids.contains(&obj_id)
                            || dict
                                .get(b"Usage")
                                .ok()
                                .and_then(|u| deref_dict(&pdf, u))
                                .and_then(|u| u.get(b"View").ok())
                                .and_then(|v| deref_dict(&pdf, v))
                                .and_then(|v| v.get(b"ViewState").ok())
                                .and_then(|o| o.as_name().ok())
                                .is_some_and(|s| s == b"OFF");
                        if is_off {
                            report.push(Finding::new(
                                FindingKind::HiddenPdfLayer,
                                Severity::High,
                                Location::Document,
                                None,
                                Some(format!("Hidden PDF layer (OCG): {name}")),
                                serde_json::json!({
                                    "layer_name": name,
                                    "default_state": "OFF",
                                }),
                            ));
                        }
                    } else if obj_type == b"Filespec" || dict.has(b"EF") {
                        let fname = dict_text(dict, b"UF")
                            .or_else(|| dict_text(dict, b"F"))
                            .unwrap_or_else(|| "embedded_file".to_string());
                        report.push(Finding::new(
                            FindingKind::EmbeddedFile,
                            Severity::Notable,
                            Location::PartPath {
                                path: fname.clone(),
                            },
                            None,
                            Some(format!("Embedded PDF file: {fname}")),
                            serde_json::json!({ "filename": fname }),
                        ));
                    }

                    // Action dictionaries (/S /JavaScript or /S /Launch)
                    inspect_pdf_action_dict(&pdf, dict, Location::Document, &mut report);
                }
                Object::Stream(stream) => {
                    let subtype = stream
                        .dict
                        .get(b"Subtype")
                        .and_then(Object::as_name)
                        .unwrap_or(b"");
                    if subtype == b"XML" {
                        let raw = stream
                            .decompressed_content_with_limit(4 * 1024 * 1024)
                            .unwrap_or_else(|_| stream.content.clone());
                        let xml_str = String::from_utf8_lossy(&raw);
                        report.push(Finding::new(
                            FindingKind::XmpHistory,
                            Severity::Info,
                            Location::Document,
                            None,
                            Some(xml_str.to_string()),
                            serde_json::json!({ "byte_length": raw.len() }),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    // 3. Positioned layout concealment inspection (white text, tiny text, off-page text, covered text, zero-width)
    let owned_layout;
    let layout_ref = if let Some(l) = precomputed_layout {
        Some(l)
    } else {
        owned_layout = interpret_pdf_bytes(bytes, &LayoutOptions::default()).ok();
        owned_layout.as_ref()
    };
    if let Some(layout_doc) = layout_ref {
        for lf in layout_doc.inspect_concealed_text(false) {
            report.push(lf);
        }
    }

    report
}

fn inspect_pdf_action_dict(
    pdf: &LopdfDocument,
    dict: &Dictionary,
    loc: Location,
    report: &mut HiddenContentReport,
) {
    let action_subtype = dict.get(b"S").and_then(Object::as_name).unwrap_or(b"");
    if action_subtype == b"JavaScript" || dict.has(b"JS") {
        let js_excerpt = dict
            .get(b"JS")
            .ok()
            .and_then(|o| match o {
                Object::String(b, _) => Some(String::from_utf8_lossy(b).into_owned()),
                Object::Reference(id) => pdf.get_object(*id).ok().and_then(|ro| match ro {
                    Object::String(b, _) => Some(String::from_utf8_lossy(b).into_owned()),
                    Object::Stream(s) => s
                        .decompressed_content_with_limit(256 * 1024)
                        .ok()
                        .map(|b| String::from_utf8_lossy(&b).into_owned()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_else(|| "PDF JavaScript action".to_string());
        let duplicate = report.items.iter().any(|f| {
            f.kind == FindingKind::PdfJavaScript && f.excerpt.as_deref() == Some(js_excerpt.trim())
        });
        if !duplicate {
            report.push(Finding::new(
                FindingKind::PdfJavaScript,
                Severity::High,
                loc,
                None,
                Some(js_excerpt),
                serde_json::json!({ "action": "JavaScript" }),
            ));
        }
    } else if action_subtype == b"Launch" {
        let target = dict_text(dict, b"F").unwrap_or_else(|| "Launch target".to_string());
        report.push(Finding::new(
            FindingKind::PdfLaunchAction,
            Severity::High,
            loc,
            None,
            Some(target.clone()),
            serde_json::json!({ "action": "Launch", "target": target }),
        ));
    } else if action_subtype == b"URI"
        && let Some(uri) = dict_text(dict, b"URI")
    {
        let duplicate = report.items.iter().any(|f| {
            f.kind == FindingKind::ExternalLinkOrConnection
                && f.detail.get("uri").and_then(|v| v.as_str()) == Some(uri.as_str())
        });
        if !duplicate {
            report.push(Finding::new(
                FindingKind::ExternalLinkOrConnection,
                Severity::Notable,
                loc,
                None,
                Some(uri.clone()),
                serde_json::json!({ "uri": uri }),
            ));
        }
    }
}

fn deref_dict<'a>(pdf: &'a LopdfDocument, obj: &'a Object) -> Option<&'a Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(id) => pdf.get_dictionary(*id).ok(),
        _ => None,
    }
}

fn deref_array<'a>(pdf: &'a LopdfDocument, obj: &'a Object) -> Option<&'a [Object]> {
    match obj {
        Object::Array(a) => Some(a),
        Object::Reference(id) => match pdf.get_object(*id) {
            Ok(Object::Array(a)) => Some(a),
            _ => None,
        },
        _ => None,
    }
}

fn dict_text(dict: &Dictionary, key: &[u8]) -> Option<String> {
    let obj = dict.get(key).ok()?;
    if let Ok(s) = lopdf::decode_text_string(obj) {
        let trimmed = s.trim_matches(char::from(0)).trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Object::Name(n) = obj {
        return Some(String::from_utf8_lossy(n).into_owned());
    }
    None
}

fn extract_page_height(pdf: &LopdfDocument, page_dict: &Dictionary) -> Option<f32> {
    let mb = page_dict.get(b"MediaBox").ok()?;
    let arr = deref_array(pdf, mb)?;
    if arr.len() >= 4 {
        let y1 = obj_to_f32(&arr[1])?;
        let y2 = obj_to_f32(&arr[3])?;
        Some((y2 - y1).abs().max(1.0))
    } else {
        None
    }
}

fn extract_pdf_rect(dict: &Dictionary, page_height: f32) -> Option<ReviewRect> {
    let Object::Array(arr) = dict.get(b"Rect").ok()? else {
        return None;
    };
    if arr.len() < 4 {
        return None;
    }
    let x1 = obj_to_f32(&arr[0])?;
    let y1 = obj_to_f32(&arr[1])?;
    let x2 = obj_to_f32(&arr[2])?;
    let y2 = obj_to_f32(&arr[3])?;
    let left = x1.min(x2);
    let width = (x2 - x1).abs().max(1.0);
    let height = (y2 - y1).abs().max(1.0);
    let top = (page_height - y1.max(y2)).max(0.0);
    Some(ReviewRect::new(left, top, width, height))
}

fn obj_to_f32(obj: &Object) -> Option<f32> {
    match *obj {
        #[allow(clippy::cast_precision_loss)]
        Object::Integer(i) => Some(i as f32),
        Object::Real(f) => Some(f),
        _ => None,
    }
}

fn count_subslice(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Generate a [`HiddenContentReport`] for an RFC 5322 `EML` or `MBOX` message.
#[must_use]
pub fn email_hidden_content_report(bytes: &[u8], format_id: &str) -> HiddenContentReport {
    let mut report = HiddenContentReport::for_format(format_id);
    let raw_str = String::from_utf8_lossy(bytes);

    if let Some(msg) = mail_parser::MessageParser::default().parse(bytes) {
        let from_addr = msg
            .from()
            .and_then(|a| a.first())
            .and_then(|a| a.address())
            .unwrap_or("");
        let reply_to_addr = msg
            .reply_to()
            .and_then(|a| a.first())
            .and_then(|a| a.address())
            .unwrap_or("");
        let return_path = msg.return_path().as_text().unwrap_or("");

        // 1. Inconsistent email headers (From vs Reply-To / Return-Path)
        let from_domain = email_domain(from_addr);
        let reply_domain = email_domain(reply_to_addr);
        let return_domain = email_domain(return_path);

        if let Some(fd) = from_domain {
            if let Some(rd) = reply_domain
                && !fd.eq_ignore_ascii_case(rd)
            {
                report.push(Finding::new(
                    FindingKind::InconsistentEmailHeaders,
                    Severity::High,
                    Location::Document,
                    None,
                    Some(format!(
                        "From ({from_addr}) differs from Reply-To ({reply_to_addr})"
                    )),
                    serde_json::json!({
                        "from": from_addr,
                        "reply_to": reply_to_addr,
                    }),
                ));
            }
            if let Some(rpd) = return_domain
                && !fd.eq_ignore_ascii_case(rpd)
            {
                report.push(Finding::new(
                    FindingKind::InconsistentEmailHeaders,
                    Severity::Notable,
                    Location::Document,
                    None,
                    Some(format!(
                        "From ({from_addr}) differs from Return-Path ({return_path})"
                    )),
                    serde_json::json!({
                        "from": from_addr,
                        "return_path": return_path,
                    }),
                ));
            }
        }

        // 2. Author / transport metadata
        if !from_addr.is_empty() || msg.subject().is_some() {
            report.push(Finding::new(
                FindingKind::AuthorAndModificationHistory,
                Severity::Info,
                Location::Document,
                None,
                Some(format!(
                    "From: {} | Subject: {}",
                    from_addr,
                    msg.subject().unwrap_or("")
                )),
                serde_json::json!({
                    "from": from_addr,
                    "subject": msg.subject(),
                    "message_id": msg.message_id(),
                }),
            ));
        }

        // 3. Attachments
        for part in msg.attachments() {
            use mail_parser::MimeHeaders;
            let name = part
                .attachment_name()
                .unwrap_or("attachment.bin")
                .to_string();
            report.push(Finding::new(
                FindingKind::EmbeddedFile,
                Severity::Notable,
                Location::PartPath { path: name.clone() },
                None,
                Some(format!("Email attachment: {name}")),
                serde_json::json!({
                    "filename": name,
                    "size": part.contents().len(),
                }),
            ));
        }

        // 4. HTML bodies: remote images, external links, concealed CSS
        for html_part in msg.html_bodies() {
            let html = html_part.to_string();
            inspect_email_html(&html, &mut report);
        }
    } else {
        inspect_email_html(&raw_str, &mut report);
    }

    if raw_str.chars().any(|c| {
        matches!(
            c,
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}'
        )
    }) {
        report.push(Finding::new(
            FindingKind::ZeroWidthCharacters,
            Severity::Notable,
            Location::Document,
            None,
            Some("Zero-width characters in email content".to_string()),
            serde_json::json!({}),
        ));
    }

    report
}

/// Generate a [`HiddenContentReport`] for an Outlook `MSG` compound file.
#[must_use]
pub fn msg_hidden_content_report(bytes: &[u8]) -> HiddenContentReport {
    let mut report = HiddenContentReport::for_format("msg");
    if let Ok(mut comp) = cfb::CompoundFile::open(Cursor::new(bytes)) {
        let entries: Vec<String> = comp
            .walk()
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();
        for path in entries {
            if path.contains("__attach_version1.0") && !path.ends_with("__attach_version1.0") {
                if path.ends_with("37010102") {
                    report.push(Finding::new(
                        FindingKind::EmbeddedFile,
                        Severity::Notable,
                        Location::PartPath { path: path.clone() },
                        None,
                        Some(format!("MSG attachment stream: {path}")),
                        serde_json::json!({ "stream": path }),
                    ));
                }
            } else if path.ends_with("007D001F") || path.ends_with("007D001E") {
                if let Ok(mut s) = comp.open_stream(&path) {
                    let mut buf = Vec::new();
                    if s.read_to_end(&mut buf).is_ok() {
                        let hdr = decode_msg_string(&buf, path.ends_with("001F"));
                        let sub = email_hidden_content_report(hdr.as_bytes(), "msg");
                        for item in sub.items {
                            report.push(item);
                        }
                    }
                }
            } else if path.ends_with("1013001F")
                || path.ends_with("1013001E")
                || path.ends_with("10130102")
            {
                if let Ok(mut s) = comp.open_stream(&path) {
                    let mut buf = Vec::new();
                    if s.read_to_end(&mut buf).is_ok() {
                        let html = decode_msg_string(&buf, path.ends_with("001F"));
                        inspect_email_html(&html, &mut report);
                    }
                }
            }
        }
    }
    report
}

fn decode_msg_string(bytes: &[u8], utf16: bool) -> String {
    if utf16 {
        let u16s: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn email_domain(addr: &str) -> Option<&str> {
    let trimmed = addr.trim().trim_matches(['<', '>', '"', '\'']);
    let (_, domain) = trimmed.rsplit_once('@')?;
    (!domain.is_empty()).then_some(domain)
}

fn inspect_email_html(html: &str, report: &mut HiddenContentReport) {
    let lower = html.to_ascii_lowercase();

    // Remote images (<img ... src="http(s)://...")
    let mut search_pos = 0;
    while let Some(img_offset) = lower[search_pos..].find("<img") {
        let start = search_pos + img_offset;
        let Some(end_offset) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..=start + end_offset];
        let tag_lower = &lower[start..=start + end_offset];
        if let Some(src_idx) = tag_lower.find("src=") {
            let after_src = tag[src_idx + 4..].trim_start();
            let quote = after_src.chars().next().unwrap_or(' ');
            let url = if quote == '"' || quote == '\'' {
                after_src[1..].split(quote).next().unwrap_or("")
            } else {
                after_src.split_whitespace().next().unwrap_or("")
            };
            if url.starts_with("http://") || url.starts_with("https://") {
                report.push(Finding::new(
                    FindingKind::RemoteEmailImage,
                    Severity::Notable,
                    Location::Document,
                    None,
                    Some(url.to_string()),
                    serde_json::json!({ "src": url }),
                ));
            }
        }
        search_pos = start + end_offset + 1;
    }

    // Concealed HTML text via CSS
    if lower.contains("display:none")
        || lower.contains("display: none")
        || lower.contains("visibility:hidden")
        || lower.contains("visibility: hidden")
        || lower.contains("font-size:0")
        || lower.contains("font-size: 0")
        || lower.contains("color:#fff")
        || lower.contains("color: #fff")
        || lower.contains("color:white")
    {
        report.push(Finding::new(
            FindingKind::WhiteOrLowContrastText,
            Severity::High,
            Location::Document,
            None,
            Some("Concealed HTML/CSS styling detected in email body".to_string()),
            serde_json::json!({ "source": "html_style" }),
        ));
    }
}

/// Generate a [`HiddenContentReport`] for an archive (`ZIP`, `TAR`, `7z`).
#[must_use]
pub fn archive_hidden_content_report(bytes: &[u8], format_id: &str) -> HiddenContentReport {
    let mut report = HiddenContentReport::for_format(format_id);

    match format_id {
        "zip" => {
            if let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes)) {
                for i in 0..zip.len() {
                    let Ok(file) = zip.by_index(i) else {
                        continue;
                    };
                    if file.is_dir() {
                        continue;
                    }
                    let name = file.name().to_string();
                    record_archive_member_finding(&name, file.size(), &mut report);
                }
            }
        }
        "tar" => {
            let mut archive = ::tar::Archive::new(Cursor::new(bytes));
            if let Ok(entries) = archive.entries() {
                for entry in entries.flatten() {
                    let name = entry
                        .path()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let size = entry.header().size().unwrap_or(0);
                    if !name.is_empty() && !name.ends_with('/') {
                        record_archive_member_finding(&name, size, &mut report);
                    }
                }
            }
        }
        "7z" => {
            let mut cursor = Cursor::new(bytes);
            let password = sevenz_rust2::Password::empty();
            if let Ok(archive) = sevenz_rust2::Archive::read(&mut cursor, &password) {
                for entry in &archive.files {
                    if !entry.is_directory() {
                        record_archive_member_finding(entry.name(), entry.size(), &mut report);
                    }
                }
            }
        }
        _ => {}
    }

    report
}

fn record_archive_member_finding(name: &str, size: u64, report: &mut HiddenContentReport) {
    let basename = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let is_hidden_dotfile = basename.starts_with('.');
    let lower = basename.to_ascii_lowercase();
    if lower.ends_with(".xlsm")
        || lower.ends_with(".docm")
        || lower.ends_with(".pptm")
        || lower.ends_with(".vbs")
        || lower.ends_with(".ps1")
        || lower.ends_with(".bat")
    {
        report.push(Finding::new(
            FindingKind::MacroCode,
            Severity::High,
            Location::PartPath {
                path: name.to_string(),
            },
            None,
            Some(format!("Macro/script archive member: {name}")),
            serde_json::json!({ "path": name, "size": size }),
        ));
    }
    report.push(Finding::new(
        FindingKind::EmbeddedFile,
        if is_hidden_dotfile {
            Severity::Notable
        } else {
            Severity::Info
        },
        Location::PartPath {
            path: name.to_string(),
        },
        None,
        Some(name.to_string()),
        serde_json::json!({
            "path": name,
            "size": size,
            "hidden_dotfile": is_hidden_dotfile,
        }),
    ));
}

/// Convenience helper to layout a UDM document when no native layout exists.
#[must_use]
pub fn fallback_udm_layout(doc: &prism_core::Document) -> Option<LayoutDocument> {
    layout_document(doc, &LayoutOptions::default()).ok()
}
