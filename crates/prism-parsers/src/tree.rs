// SPDX-License-Identifier: AGPL-3.0-only
//! Unified container and embedded-object family tree walker (`build_family_tree`, `open_child_bytes`).
//!
//! Walks nested archives (`ZIP`, `TAR`, `7z`, `GZIP`, `BZIP2`), emails (`EML`, `MSG`, `MBOX`),
//! OOXML packages (`DOCX`, `PPTX`, `XLSX` embeddings, fonts, and media), OLE2 compound documents,
//! and PDF embedded files / portfolios with bounded depth, total byte size, member count, and
//! SHA-256 cycle/quine detection.

#![allow(
    clippy::assert_is_empty,
    clippy::map_unwrap_or,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines
)]

use crate::limits::read_bounded;
use lopdf::{Document as LopdfDocument, Object};
use prism_core::error::{Error, Result};
use prism_core::format::detect_format;
use prism_core::hash::sha256_hex;
use prism_core::tree::{ContainerLimits, ContainerNode, ContainerNodeStatus, FamilyTree};
use prism_excel::is_xlsx_zip;
use prism_ppt::is_pptx_zip;
use prism_word::is_docx_zip;
use std::collections::BTreeSet;
use std::io::Cursor;

/// Build the unified [`FamilyTree`] for a document given its raw `bytes`, optional `filename_hint`,
/// and [`ContainerLimits`].
#[must_use]
pub fn build_family_tree(
    bytes: &[u8],
    filename_hint: Option<&str>,
    limits: ContainerLimits,
) -> FamilyTree {
    let root_name = filename_hint.unwrap_or("root").to_string();
    let (fmt_id, mime_type) = detect_format_and_mime(bytes, Some(&root_name));
    let mut root = ContainerNode::from_bytes(
        "0",
        &root_name,
        &root_name,
        fmt_id,
        mime_type,
        bytes,
        0,
        ContainerNodeStatus::Parsed,
    );

    let mut state = TreeWalkState {
        limits,
        total_members: 0,
        total_bytes: 0,
    };
    let mut ancestors = BTreeSet::new();
    ancestors.insert(root.sha256.clone());

    expand_node_children(&mut root, bytes, &mut ancestors, &mut state);

    FamilyTree {
        root,
        total_members: state.total_members,
        total_expanded_bytes: state.total_bytes,
        limits,
    }
}

/// Extract the raw bytes and [`ContainerNode`] metadata of a specific child node (by `id` like `"0/0"`
/// or `path`) from the container hierarchy rooted at `bytes`.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if the requested child node does not exist or cannot be extracted.
pub fn open_child_bytes(
    bytes: &[u8],
    filename_hint: Option<&str>,
    child_id_or_path: &str,
    limits: ContainerLimits,
) -> Result<(ContainerNode, Vec<u8>)> {
    let root_name = filename_hint.unwrap_or("root").to_string();
    let (fmt_id, mime_type) = detect_format_and_mime(bytes, Some(&root_name));
    let root = ContainerNode::from_bytes(
        "0",
        &root_name,
        &root_name,
        fmt_id,
        mime_type,
        bytes,
        0,
        ContainerNodeStatus::Parsed,
    );

    if child_id_or_path == "0" || child_id_or_path == root_name {
        return Ok((root, bytes.to_vec()));
    }

    let mut state = TreeWalkState {
        limits,
        total_members: 0,
        total_bytes: 0,
    };
    let mut ancestors = BTreeSet::new();
    ancestors.insert(root.sha256.clone());

    if let Some(found) =
        find_and_extract_child(&root, bytes, child_id_or_path, &mut ancestors, &mut state)
    {
        Ok(found)
    } else {
        Err(Error::InvalidInput(format!(
            "Child container node not found: {child_id_or_path}"
        )))
    }
}

struct TreeWalkState {
    limits: ContainerLimits,
    total_members: usize,
    total_bytes: u64,
}

/// Payload marker for a member that was not extracted because the member limit was reached.
const MEMBER_LIMIT_MARKER: &str = "Member limit reached";
/// Payload marker for a member that did not fit in the remaining byte budget (matched like the
/// `Memory limit exceeded` error that bounded reads return).
const SIZE_LIMIT_MARKER: &str = "Memory limit exceeded: container byte budget exhausted";

/// What one call to [`extract_direct_children`] may still decompress. Members are charged as
/// they are read, so a container never holds more than the tree's remaining byte budget, and
/// once the member budget is spent the rest of the container is not decompressed at all.
struct ExtractBudget {
    remaining_bytes: u64,
    remaining_members: usize,
}

impl ExtractBudget {
    fn from_state(state: &TreeWalkState) -> Self {
        Self {
            remaining_bytes: state
                .limits
                .max_total_bytes
                .saturating_sub(state.total_bytes),
            remaining_members: state.limits.max_members.saturating_sub(state.total_members),
        }
    }

    /// Admit one more member, or push a member-limit stub and return `false`.
    fn admit(&mut self, out: &mut Vec<RawChildEntry>, rel_path: &str, declared_size: u64) -> bool {
        if self.remaining_members == 0 {
            out.push(RawChildEntry {
                rel_path: rel_path.to_string(),
                name: rel_path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(rel_path)
                    .to_string(),
                listed_only: false,
                declared_size,
                payload: Err(MEMBER_LIMIT_MARKER.to_string()),
            });
            return false;
        }
        self.remaining_members -= 1;
        true
    }

    /// Largest member that may still be read.
    fn entry_limit(&self) -> usize {
        usize::try_from(self.remaining_bytes).unwrap_or(usize::MAX)
    }

    /// Charge a read member against the byte budget.
    fn charge(
        &mut self,
        payload: std::result::Result<Vec<u8>, String>,
    ) -> std::result::Result<Vec<u8>, String> {
        match payload {
            Ok(bytes) if bytes.len() as u64 > self.remaining_bytes => {
                Err(SIZE_LIMIT_MARKER.to_string())
            }
            Ok(bytes) => {
                self.remaining_bytes -= bytes.len() as u64;
                Ok(bytes)
            }
            Err(e) => Err(e),
        }
    }
}

struct RawChildEntry {
    rel_path: String,
    name: String,
    listed_only: bool,
    declared_size: u64,
    payload: std::result::Result<Vec<u8>, String>,
}

fn expand_node_children(
    parent: &mut ContainerNode,
    parent_bytes: &[u8],
    ancestors: &mut BTreeSet<String>,
    state: &mut TreeWalkState,
) {
    if parent.depth >= state.limits.max_depth {
        // Only ask whether the node has children; a zero member budget reads none of them.
        let mut probe = ExtractBudget {
            remaining_bytes: 0,
            remaining_members: 0,
        };
        if !extract_direct_children(parent_bytes, &parent.detected_format, &mut probe).is_empty() {
            parent.status = ContainerNodeStatus::DepthLimitReached {
                max_depth: state.limits.max_depth,
            };
        }
        return;
    }

    let mut budget = ExtractBudget::from_state(state);
    let available = budget.remaining_bytes;
    let raw_children = extract_direct_children(parent_bytes, &parent.detected_format, &mut budget);
    // Charge everything this container decompressed now, before recursing into any child, so
    // siblings waiting in memory count against the budget the children are extracted under.
    state.total_bytes = state
        .total_bytes
        .saturating_add(available - budget.remaining_bytes);
    if raw_children.is_empty() {
        return;
    }
    let next_depth = parent.depth + 1;

    for (idx, raw) in raw_children.into_iter().enumerate() {
        let child_id = format!("{}/{idx}", parent.id);
        let full_path = if parent.depth == 0 {
            raw.rel_path.clone()
        } else {
            format!("{}/{}", parent.path, raw.rel_path)
        };

        let member_limit_stub = matches!(&raw.payload, Err(e) if e == MEMBER_LIMIT_MARKER);
        if member_limit_stub || state.total_members >= state.limits.max_members {
            let (fmt, mime) = detect_format_and_mime(&[], Some(&raw.name));
            let mut limit_node = ContainerNode::from_bytes(
                child_id,
                full_path,
                raw.name,
                fmt,
                mime,
                &[],
                next_depth,
                ContainerNodeStatus::MemberLimitReached {
                    max_members: state.limits.max_members,
                },
            );
            limit_node.size = raw.declared_size;
            parent.children.push(limit_node);
            break;
        }

        match raw.payload {
            Err(err_msg) => {
                state.total_members = state.total_members.saturating_add(1);
                let (fmt, mime) = detect_format_and_mime(&[], Some(&raw.name));
                let status = if err_msg.contains("Memory limit exceeded") {
                    ContainerNodeStatus::SizeLimitReached {
                        max_bytes: state.limits.max_total_bytes,
                    }
                } else {
                    ContainerNodeStatus::Error { message: err_msg }
                };
                let mut err_node = ContainerNode::from_bytes(
                    child_id,
                    full_path,
                    raw.name,
                    fmt,
                    mime,
                    &[],
                    next_depth,
                    status,
                );
                err_node.size = raw.declared_size;
                parent.children.push(err_node);
            }
            Ok(child_bytes) => {
                // The bytes were charged when the container was extracted.
                state.total_members = state.total_members.saturating_add(1);

                let (fmt, mime) = detect_format_and_mime(&child_bytes, Some(&raw.name));
                let child_hash = sha256_hex(&child_bytes);

                if raw.listed_only {
                    let node = ContainerNode::from_bytes(
                        child_id,
                        full_path,
                        raw.name,
                        fmt,
                        mime,
                        &child_bytes,
                        next_depth,
                        ContainerNodeStatus::ListedOnly,
                    );
                    parent.children.push(node);
                    continue;
                }

                if ancestors.contains(&child_hash) {
                    let node = ContainerNode::from_bytes(
                        child_id,
                        full_path,
                        raw.name,
                        fmt,
                        mime,
                        &child_bytes,
                        next_depth,
                        ContainerNodeStatus::CycleDetected {
                            ancestor_sha256: child_hash,
                        },
                    );
                    parent.children.push(node);
                    continue;
                }

                let mut child_node = ContainerNode::from_bytes(
                    child_id,
                    full_path,
                    raw.name,
                    fmt,
                    mime,
                    &child_bytes,
                    next_depth,
                    ContainerNodeStatus::Parsed,
                );

                ancestors.insert(child_hash.clone());
                expand_node_children(&mut child_node, &child_bytes, ancestors, state);
                ancestors.remove(&child_hash);

                parent.children.push(child_node);
            }
        }
    }
}

fn find_and_extract_child(
    parent: &ContainerNode,
    parent_bytes: &[u8],
    target: &str,
    ancestors: &mut BTreeSet<String>,
    state: &mut TreeWalkState,
) -> Option<(ContainerNode, Vec<u8>)> {
    if parent.depth >= state.limits.max_depth {
        return None;
    }
    let mut budget = ExtractBudget::from_state(state);
    let raw_children = extract_direct_children(parent_bytes, &parent.detected_format, &mut budget);
    // Charge what this container decompressed, so a search through nested archives is held to
    // the same totals as building the tree.
    state.total_bytes = state
        .limits
        .max_total_bytes
        .saturating_sub(budget.remaining_bytes);
    state.total_members = state
        .limits
        .max_members
        .saturating_sub(budget.remaining_members);
    let next_depth = parent.depth + 1;

    for (idx, raw) in raw_children.into_iter().enumerate() {
        let Ok(child_bytes) = raw.payload else {
            continue;
        };
        let child_id = format!("{}/{idx}", parent.id);
        let full_path = if parent.depth == 0 {
            raw.rel_path.clone()
        } else {
            format!("{}/{}", parent.path, raw.rel_path)
        };
        let (fmt, mime) = detect_format_and_mime(&child_bytes, Some(&raw.name));
        let status = if raw.listed_only {
            ContainerNodeStatus::ListedOnly
        } else {
            ContainerNodeStatus::Parsed
        };
        let child_node = ContainerNode::from_bytes(
            &child_id,
            &full_path,
            &raw.name,
            fmt,
            mime,
            &child_bytes,
            next_depth,
            status,
        );

        if child_id == target || full_path == target || raw.rel_path == target || raw.name == target
        {
            return Some((child_node, child_bytes));
        }

        if !raw.listed_only && !ancestors.contains(&child_node.sha256) {
            let hash = child_node.sha256.clone();
            ancestors.insert(hash.clone());
            if let Some(found) =
                find_and_extract_child(&child_node, &child_bytes, target, ancestors, state)
            {
                ancestors.remove(&hash);
                return Some(found);
            }
            ancestors.remove(&hash);
        }
    }
    None
}

fn detect_format_and_mime(bytes: &[u8], filename_hint: Option<&str>) -> (String, String) {
    if is_docx_zip(bytes) {
        return (
            "docx".to_string(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string(),
        );
    }
    if is_pptx_zip(bytes) {
        return (
            "pptx".to_string(),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string(),
        );
    }
    if is_xlsx_zip(bytes) {
        return (
            "xlsx".to_string(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
        );
    }
    if let Some(det) = detect_format(bytes, filename_hint) {
        let short = det.format.extension.to_ascii_lowercase();
        return (short, det.format.mime_type);
    }
    let ext = filename_hint
        .and_then(|n| n.rsplit('.').next())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "bin".to_string());
    let mime = match ext.as_str() {
        "ttf" | "otf" | "odttf" | "woff" | "woff2" => "font/sfnt",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    };
    (ext, mime.to_string())
}

#[allow(clippy::too_many_lines)]
fn extract_direct_children(
    bytes: &[u8],
    format_id: &str,
    budget: &mut ExtractBudget,
) -> Vec<RawChildEntry> {
    let mut out = Vec::new();

    match format_id {
        "docx" | "pptx" | "xlsx" => {
            if let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes)) {
                for i in 0..zip.len() {
                    let Ok(file) = zip.by_index(i) else {
                        continue;
                    };
                    if file.is_dir() {
                        continue;
                    }
                    let name = file.name().to_string();
                    let lower = name.to_ascii_lowercase();
                    let is_embedding = lower.contains("/embeddings/");
                    let is_media_or_font = lower.contains("/media/") || lower.contains("/fonts/");
                    if !is_embedding && !is_media_or_font {
                        continue;
                    }
                    let declared_size = file.size();
                    if !budget.admit(&mut out, &name, declared_size) {
                        break;
                    }
                    let basename = name.rsplit('/').next().unwrap_or(&name).to_string();
                    let payload = budget.charge(
                        read_bounded(file, budget.entry_limit()).map_err(|e| e.to_string()),
                    );
                    out.push(RawChildEntry {
                        rel_path: name,
                        name: basename,
                        listed_only: is_media_or_font,
                        declared_size,
                        payload,
                    });
                }
            }
        }
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
                    let declared_size = file.size();
                    if !budget.admit(&mut out, &name, declared_size) {
                        break;
                    }
                    let basename = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
                    let payload = budget.charge(
                        read_bounded(file, budget.entry_limit()).map_err(|e| e.to_string()),
                    );
                    out.push(RawChildEntry {
                        rel_path: name,
                        name: basename,
                        listed_only: false,
                        declared_size,
                        payload,
                    });
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
                    if name.is_empty() || name.ends_with('/') {
                        continue;
                    }
                    let declared_size = entry.header().size().unwrap_or(0);
                    if !budget.admit(&mut out, &name, declared_size) {
                        break;
                    }
                    let basename = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
                    let payload = budget.charge(
                        read_bounded(entry, budget.entry_limit()).map_err(|e| e.to_string()),
                    );
                    out.push(RawChildEntry {
                        rel_path: name,
                        name: basename,
                        listed_only: false,
                        declared_size,
                        payload,
                    });
                }
            }
        }
        "7z" => {
            if let Ok(mut reader) = sevenz_rust2::ArchiveReader::new(
                Cursor::new(bytes),
                sevenz_rust2::Password::empty(),
            ) {
                let _ = reader.for_each_entries(|entry, reader_io| {
                    if !entry.is_directory() {
                        let name = entry.name().to_string();
                        let declared_size = entry.size();
                        if !budget.admit(&mut out, &name, declared_size) {
                            return Ok(false);
                        }
                        let basename = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
                        let payload = budget.charge(
                            read_bounded(reader_io, budget.entry_limit())
                                .map_err(|e| e.to_string()),
                        );
                        out.push(RawChildEntry {
                            rel_path: name,
                            name: basename,
                            listed_only: false,
                            declared_size,
                            payload,
                        });
                    }
                    Ok(true)
                });
            }
        }
        "gz" => {
            if !budget.admit(&mut out, "decompressed", 0) {
                return out;
            }
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            let payload = budget
                .charge(read_bounded(decoder, budget.entry_limit()).map_err(|e| e.to_string()));
            let declared_size = payload.as_ref().map_or(0, |b| b.len() as u64);
            out.push(RawChildEntry {
                rel_path: "decompressed".to_string(),
                name: "decompressed".to_string(),
                listed_only: false,
                declared_size,
                payload,
            });
        }
        "bz2" => {
            if !budget.admit(&mut out, "decompressed", 0) {
                return out;
            }
            let decoder = bzip2::read::BzDecoder::new(Cursor::new(bytes));
            let payload = budget
                .charge(read_bounded(decoder, budget.entry_limit()).map_err(|e| e.to_string()));
            let declared_size = payload.as_ref().map_or(0, |b| b.len() as u64);
            out.push(RawChildEntry {
                rel_path: "decompressed".to_string(),
                name: "decompressed".to_string(),
                listed_only: false,
                declared_size,
                payload,
            });
        }
        "eml" | "mbox" => {
            if let Some(msg) = mail_parser::MessageParser::default().parse(bytes) {
                for (i, part) in msg.attachments().enumerate() {
                    use mail_parser::MimeHeaders;
                    let name = part
                        .attachment_name()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("attachment_{i}.bin"));
                    let size = part.contents().len() as u64;
                    if !budget.admit(&mut out, &name, size) {
                        break;
                    }
                    let payload = budget.charge(Ok(part.contents().to_vec()));
                    out.push(RawChildEntry {
                        rel_path: name.clone(),
                        name,
                        listed_only: false,
                        declared_size: size,
                        payload,
                    });
                }
            }
        }
        "msg" | "doc" | "ppt" | "xls" => {
            if let Ok(mut comp) = cfb::CompoundFile::open(Cursor::new(bytes)) {
                let paths: Vec<String> = comp
                    .walk()
                    .filter(|e| e.is_stream())
                    .map(|e| e.path().to_string_lossy().into_owned())
                    .collect();
                for path in paths {
                    let is_msg_attach =
                        path.contains("__attach_version1.0") && path.ends_with("37010102");
                    let is_ole_pool = path.contains("ObjectPool") || path.contains("MBD");
                    if !is_msg_attach && !is_ole_pool {
                        continue;
                    }
                    if let Ok(stream) = comp.open_stream(&path) {
                        let rel_path = path.trim_start_matches('/').to_string();
                        let size = stream.len();
                        if !budget.admit(&mut out, &rel_path, size) {
                            break;
                        }
                        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                        let payload = budget.charge(
                            read_bounded(stream, budget.entry_limit()).map_err(|e| e.to_string()),
                        );
                        out.push(RawChildEntry {
                            rel_path,
                            name,
                            listed_only: false,
                            declared_size: size,
                            payload,
                        });
                    }
                }
            }
        }
        "pdf" => {
            if let Ok(mut pdf) = LopdfDocument::load_mem(bytes) {
                if pdf.is_encrypted() {
                    let _ = pdf.decrypt("");
                }
                for obj in pdf.objects.values() {
                    let Object::Dictionary(dict) = obj else {
                        continue;
                    };
                    let Ok(ef_obj) = dict.get(b"EF") else {
                        continue;
                    };
                    let ef_dict = match ef_obj {
                        Object::Dictionary(d) => Some(d),
                        Object::Reference(id) => pdf.get_dictionary(*id).ok(),
                        _ => None,
                    };
                    let Some(ef_dict) = ef_dict else {
                        continue;
                    };
                    let fname = dict
                        .get(b"UF")
                        .or_else(|_| dict.get(b"F"))
                        .ok()
                        .and_then(|o| lopdf::decode_text_string(o).ok())
                        .unwrap_or_else(|| "embedded_file".to_string());
                    let stream_ref = ef_dict
                        .get(b"UF")
                        .or_else(|_| ef_dict.get(b"F"))
                        .ok()
                        .and_then(|o| o.as_reference().ok());
                    if let Some(sid) = stream_ref
                        && let Ok(Object::Stream(s)) = pdf.get_object(sid)
                    {
                        if !budget.admit(&mut out, &fname, s.content.len() as u64) {
                            break;
                        }
                        let payload = budget.charge(
                            s.decompressed_content_with_limit(budget.entry_limit())
                                .map_err(|e| e.to_string()),
                        );
                        let declared_size = payload
                            .as_ref()
                            .map_or(s.content.len() as u64, |b| b.len() as u64);
                        out.push(RawChildEntry {
                            rel_path: fname.clone(),
                            name: fname,
                            listed_only: false,
                            declared_size,
                            payload,
                        });
                    }
                }
            }
        }
        "pst" | "ost" => {
            if let Ok(archive) = crate::email::pst::PstArchive::parse(bytes) {
                for (idx, msg) in archive.messages.iter().enumerate() {
                    let eml_bytes = msg.to_eml_bytes();
                    let safe_subj: String = msg
                        .subject
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                                c
                            } else {
                                '_'
                            }
                        })
                        .take(40)
                        .collect();
                    let fname = format!("message_{}_{}.eml", idx + 1, safe_subj);
                    let rel_path = format!("{}/{fname}", msg.folder_path);
                    let declared_size = eml_bytes.len() as u64;
                    if !budget.admit(&mut out, &rel_path, declared_size) {
                        break;
                    }
                    let payload = budget.charge(Ok(eml_bytes));
                    out.push(RawChildEntry {
                        rel_path,
                        name: fname,
                        listed_only: false,
                        declared_size,
                        payload,
                    });
                }
            }
        }
        "rsmf" => {
            if let Some(zip_bytes) = crate::email::chat::extract_rsmf_zip_bytes(bytes)
                && let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(&zip_bytes))
            {
                for i in 0..zip.len() {
                    let Ok(file) = zip.by_index(i) else {
                        continue;
                    };
                    if file.is_dir() {
                        continue;
                    }
                    let name = file.name().to_string();
                    let declared_size = file.size();
                    if !budget.admit(&mut out, &name, declared_size) {
                        break;
                    }
                    let basename = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
                    let payload = budget.charge(
                        read_bounded(file, budget.entry_limit()).map_err(|e| e.to_string()),
                    );
                    out.push(RawChildEntry {
                        rel_path: name,
                        name: basename,
                        listed_only: false,
                        declared_size,
                        payload,
                    });
                }
            }
        }
        _ => {}
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::default();
            for (name, data) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn enforces_depth_member_and_size_limits_on_nested_containers() {
        let inner_zip = make_zip(&[
            ("a.txt", b"hello a"),
            ("b.txt", b"hello b"),
            ("c.txt", b"hello c"),
        ]);
        let outer_zip = make_zip(&[("inner.zip", &inner_zip), ("top.txt", b"top")]);

        // 1. Default limits expand everything
        let tree = build_family_tree(&outer_zip, Some("outer.zip"), ContainerLimits::default());
        assert_eq!(tree.total_members, 5); // inner.zip + 3 inner files + top.txt
        let (node, bytes) = open_child_bytes(
            &outer_zip,
            Some("outer.zip"),
            "inner.zip/b.txt",
            ContainerLimits::default(),
        )
        .unwrap();
        assert_eq!(node.name, "b.txt");
        assert_eq!(bytes, b"hello b");

        // 2. Depth limit = 1 stops at inner.zip without expanding its children
        let depth_tree = build_family_tree(
            &outer_zip,
            Some("outer.zip"),
            ContainerLimits {
                max_depth: 1,
                ..ContainerLimits::default()
            },
        );
        let inner_node = depth_tree.find_node("0/0").unwrap();
        assert!(matches!(
            inner_node.status,
            ContainerNodeStatus::DepthLimitReached { max_depth: 1 }
        ));
        assert!(inner_node.children.is_empty());

        // 3. Member limit = 2 stops after 2 members
        let member_tree = build_family_tree(
            &outer_zip,
            Some("outer.zip"),
            ContainerLimits {
                max_members: 2,
                ..ContainerLimits::default()
            },
        );
        assert!(member_tree.flatten().iter().any(|n| matches!(
            n.status,
            ContainerNodeStatus::MemberLimitReached { max_members: 2 }
        )));
    }

    #[test]
    fn highly_compressed_members_stop_at_the_byte_budget() {
        // 40 members of 1 MiB zeros each: a small zip that expands to 40 MiB.
        let zeros = vec![0u8; 1 << 20];
        let names: Vec<String> = (0..40).map(|i| format!("z{i}.bin")).collect();
        let entries: Vec<(&str, &[u8])> = names
            .iter()
            .map(|n| (n.as_str(), zeros.as_slice()))
            .collect();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in &entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        let bomb = cursor.into_inner();
        assert!(bomb.len() < 1 << 20);

        let limits = ContainerLimits {
            max_total_bytes: 3 << 20,
            ..ContainerLimits::default()
        };
        let tree = build_family_tree(&bomb, Some("bomb.zip"), limits);
        assert!(tree.total_expanded_bytes <= 3 << 20);
        let nodes = tree.flatten();
        assert_eq!(
            nodes
                .iter()
                .filter(|n| matches!(n.status, ContainerNodeStatus::Parsed) && n.depth == 1)
                .count(),
            3
        );
        assert!(
            nodes
                .iter()
                .any(|n| matches!(n.status, ContainerNodeStatus::SizeLimitReached { .. }))
        );

        // With a member budget, members past it are listed as cut off, not decompressed.
        let few = ContainerLimits {
            max_members: 5,
            ..ContainerLimits::default()
        };
        let tree = build_family_tree(&bomb, Some("bomb.zip"), few);
        assert_eq!(tree.total_members, 5);
        assert!(tree.total_expanded_bytes <= 5 << 20);
    }
}
