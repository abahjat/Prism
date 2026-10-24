// SPDX-License-Identifier: AGPL-3.0-only
//! OOXML `PowerPoint` (`.pptx`, `.ppsx`, `.potx`) parser producing a [`Presentation`].

use crate::model::{
    AutofitMode, BulletKind, NotesSlide, PlaceholderRef, PptTextAlign, PptTextRun, Presentation,
    ShapeFill, ShapeKind, ShapeStroke, Slide, SlideComment, SlideLayout, SlideMaster, SlideShape,
    SlideTable, SlideTableCell, SlideTableRow, TextBody, TextLevelStyle, TextListStyle,
    TextParagraph, TextRunStyle, TextVerticalAnchor,
};
use prism_core::document::{FontResource, ImageResource};
use prism_core::error::{Error, Result};
use prism_drawingml::{
    ColorTransform, CustomGeometry, CustomPath, CustomPathCmd, DrawingTheme, GuideDef,
    apply_color_transforms, parse_chart_xml, parse_drawing_theme, parse_hex_rgba,
};
use prism_layout::{Rgba, deobfuscate_odttf};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use zip::ZipArchive;

/// Default maximum decompressed size per ZIP entry (`64 MiB`).
pub const DEFAULT_MAX_PPTX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

/// English Metric Units per typographic point (`914400 / 72 = 12700`).
const EMU_PER_POINT: f32 = 12_700.0;

/// Check whether `bytes` is a ZIP archive containing `ppt/presentation.xml`.
#[must_use]
pub fn is_pptx_zip(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || &bytes[0..4] != b"PK\x03\x04" {
        return false;
    }
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    archive.by_name("ppt/presentation.xml").is_ok()
}

/// Parse a `.pptx`, `.ppsx`, or `.potx` archive from `bytes` into a [`Presentation`].
///
/// # Errors
///
/// Returns [`Error::Parse`] if the archive is malformed or [`Error::ResourceLimit`] if a
/// decompressed stream exceeds `max_entry_bytes`.
#[allow(clippy::too_many_lines)]
pub fn parse_pptx(bytes: &[u8], max_entry_bytes: usize) -> Result<Presentation> {
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|e| Error::ParseError(format!("Invalid PPTX ZIP archive: {e}")))?;

    let mut pres = Presentation::default();

    // Check for VBA project
    if archive.by_name("ppt/vbaProject.bin").is_ok() {
        pres.forensic.vba_project_present = true;
    }

    // 1. Parse docProps/core.xml
    if let Ok(core_xml) = read_zip_string(&mut archive, "docProps/core.xml", max_entry_bytes) {
        let root = XmlNode::parse(&core_xml);
        if let Some(t) = find_node_by_local(&root, "title") {
            let txt = t.text_content().trim().to_string();
            if !txt.is_empty() {
                pres.title = Some(txt);
            }
        }
        if let Some(c) = find_node_by_local(&root, "creator") {
            let txt = c.text_content().trim().to_string();
            if !txt.is_empty() {
                pres.author = Some(txt);
            }
        }
    }

    // 2. Parse all themes in ppt/theme/theme*.xml
    let entry_names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    for name in &entry_names {
        if name.starts_with("ppt/theme/")
            && Path::extension_is_xml(name)
            && let Ok(xml) = read_zip_string(&mut archive, name, max_entry_bytes)
        {
            pres.themes.insert(name.clone(), parse_drawing_theme(&xml));
        }
    }

    // 3. Parse commentAuthors.xml if present
    let mut comment_authors_map: BTreeMap<String, (String, String)> = BTreeMap::new();
    if let Ok(ca_xml) = read_zip_string(&mut archive, "ppt/commentAuthors.xml", max_entry_bytes) {
        let root = XmlNode::parse(&ca_xml);
        for c in root.children_elements() {
            if c.local_name() == "cmAuthor"
                && let Some(id) = c.attr("id")
            {
                let name = c.attr("name").unwrap_or("Reviewer").to_string();
                let initials = c.attr("initials").unwrap_or("R").to_string();
                comment_authors_map.insert(id.to_string(), (name, initials));
            }
        }
    }

    // 4. Parse ppt/presentation.xml and its relationships
    let pres_xml = read_zip_string(&mut archive, "ppt/presentation.xml", max_entry_bytes)
        .map_err(|_| Error::ParseError("Missing ppt/presentation.xml".to_string()))?;
    let pres_rels = parse_part_rels(&mut archive, "ppt/presentation.xml", max_entry_bytes)?;
    record_external_rels(&pres_rels, &mut pres);

    let pres_root = XmlNode::parse(&pres_xml);
    if let Some(sld_sz) = pres_root.child_by_local("sldSz") {
        if let Some(cx) = sld_sz.attr("cx").and_then(|v| v.parse::<f32>().ok()) {
            pres.slide_width_pt = (cx / EMU_PER_POINT).clamp(72.0, 5000.0);
        }
        if let Some(cy) = sld_sz.attr("cy").and_then(|v| v.parse::<f32>().ok()) {
            pres.slide_height_pt = (cy / EMU_PER_POINT).clamp(72.0, 5000.0);
        }
    }
    if let Some(notes_sz) = pres_root.child_by_local("notesSz") {
        if let Some(cx) = notes_sz.attr("cx").and_then(|v| v.parse::<f32>().ok()) {
            pres.notes_width_pt = (cx / EMU_PER_POINT).clamp(72.0, 5000.0);
        }
        if let Some(cy) = notes_sz.attr("cy").and_then(|v| v.parse::<f32>().ok()) {
            pres.notes_height_pt = (cy / EMU_PER_POINT).clamp(72.0, 5000.0);
        }
    }

    // Parse embedded fonts (`p:embeddedFontLst`)
    if let Some(ef_lst) = pres_root.child_by_local("embeddedFontLst") {
        parse_embedded_fonts(ef_lst, &pres_rels, &mut archive, max_entry_bytes, &mut pres)?;
    }

    // 5. Parse all slideMasters
    for name in &entry_names {
        if name.starts_with("ppt/slideMasters/")
            && Path::extension_is_xml(name)
            && !name.contains("_rels")
            && let Ok(master) = parse_slide_master(name, &mut archive, max_entry_bytes, &mut pres)
        {
            pres.masters.insert(name.clone(), master);
        }
    }

    // 6. Parse all slideLayouts
    for name in &entry_names {
        if name.starts_with("ppt/slideLayouts/")
            && Path::extension_is_xml(name)
            && !name.contains("_rels")
            && let Ok(layout) = parse_slide_layout(name, &mut archive, max_entry_bytes, &mut pres)
        {
            pres.layouts.insert(name.clone(), layout);
        }
    }

    // 7. Determine ordered slide paths from `p:sldIdLst` (or fallback to sorted `ppt/slides/slide*.xml`)
    let mut slide_paths: Vec<String> = Vec::new();
    if let Some(sld_lst) = pres_root.child_by_local("sldIdLst") {
        for sld_id in sld_lst.children_elements() {
            if sld_id.local_name() == "sldId"
                && let Some(rid) = sld_id.attr("r:id").or_else(|| sld_id.attr("id"))
                && let Some(rel) = pres_rels.get(rid)
                && !rel.external
            {
                slide_paths.push(resolve_zip_path("ppt/presentation.xml", &rel.target));
            }
        }
    }
    if slide_paths.is_empty() {
        let mut fallback: Vec<String> = entry_names
            .iter()
            .filter(|n| {
                n.starts_with("ppt/slides/slide")
                    && Path::extension_is_xml(n)
                    && !n.contains("_rels")
            })
            .cloned()
            .collect();
        fallback.sort_by(|a, b| nat_cmp(a, b));
        slide_paths = fallback;
    }

    // 8. Parse each slide in presentation order
    let mut distinct_authors = BTreeSet::new();
    for (idx, slide_path) in slide_paths.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let slide_num = (idx + 1) as u32;
        if let Ok(slide) = parse_slide(
            slide_path,
            slide_num,
            &comment_authors_map,
            &mut distinct_authors,
            &mut archive,
            max_entry_bytes,
            &mut pres,
        ) {
            if slide.hidden {
                pres.forensic.hidden_slide_numbers.push(slide_num);
            }
            pres.slides.push(slide);
        }
    }
    pres.forensic.comment_authors = distinct_authors.into_iter().collect();

    Ok(pres)
}

struct Path;
impl Path {
    fn extension_is_xml(name: &str) -> bool {
        std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
    }
}

fn nat_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let num = |s: &str| -> u32 {
        s.chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or(0)
    };
    num(a).cmp(&num(b)).then_with(|| a.cmp(b))
}

#[derive(Debug, Clone)]
struct RelEntry {
    rel_type: String,
    target: String,
    external: bool,
}

#[allow(clippy::unnecessary_wraps)]
fn parse_part_rels(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    part_path: &str,
    max_entry_bytes: usize,
) -> Result<BTreeMap<String, RelEntry>> {
    let rels_path = if let Some((dir, file)) = part_path.rsplit_once('/') {
        format!("{dir}/_rels/{file}.rels")
    } else {
        format!("_rels/{part_path}.rels")
    };
    let Ok(xml) = read_zip_string(archive, &rels_path, max_entry_bytes) else {
        return Ok(BTreeMap::new());
    };
    let root = XmlNode::parse(&xml);
    let mut map = BTreeMap::new();
    for c in root.children_elements() {
        if c.local_name() == "Relationship"
            && let (Some(id), Some(target)) = (c.attr("Id"), c.attr("Target"))
        {
            let external = c
                .attr("TargetMode")
                .is_some_and(|m| m.eq_ignore_ascii_case("External"));
            let rel_type = c.attr("Type").unwrap_or("").to_string();
            map.insert(
                id.to_string(),
                RelEntry {
                    rel_type,
                    target: target.to_string(),
                    external,
                },
            );
        }
    }
    Ok(map)
}

fn record_external_rels(rels: &BTreeMap<String, RelEntry>, pres: &mut Presentation) {
    for rel in rels.values() {
        if rel.external && !pres.forensic.external_links.contains(&rel.target) {
            pres.forensic.external_links.push(rel.target.clone());
        }
        if (rel.rel_type.contains("/audio")
            || rel.rel_type.contains("/video")
            || rel.rel_type.contains("/media"))
            && !pres.forensic.media_objects.contains(&rel.target)
        {
            pres.forensic.media_objects.push(rel.target.clone());
        }
    }
}

fn resolve_zip_path(base_part: &str, target: &str) -> String {
    let t = target.trim_start_matches('/');
    if target.starts_with('/') || t.starts_with("ppt/") {
        return normalize_path_segments(t);
    }
    let base_dir = base_part.rsplit_once('/').map_or("", |(d, _)| d);
    let combined = if base_dir.is_empty() {
        t.to_string()
    } else {
        format!("{base_dir}/{t}")
    };
    normalize_path_segments(&combined)
}

fn normalize_path_segments(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn read_zip_bytes(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut file = archive
        .by_name(name)
        .map_err(|e| Error::ParseError(format!("Missing ZIP entry {name}: {e}")))?;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| Error::ParseError(format!("Read error in {name}: {e}")))?;
        if n == 0 {
            break;
        }
        if out.len().saturating_add(n) > max_bytes {
            return Err(Error::MemoryLimitExceeded {
                used: out.len().saturating_add(n),
                limit: max_bytes,
            });
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

fn read_zip_string(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    max_bytes: usize,
) -> Result<String> {
    let bytes = read_zip_bytes(archive, name, max_bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[allow(
    clippy::unnecessary_wraps,
    clippy::case_sensitive_file_extension_comparisons
)]
fn parse_embedded_fonts(
    ef_lst: &XmlNode,
    rels: &BTreeMap<String, RelEntry>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<()> {
    for ef in ef_lst.children_elements() {
        if ef.local_name() != "embeddedFont" {
            continue;
        }
        let Some(font_node) = ef.child_by_local("font") else {
            continue;
        };
        let Some(typeface) = font_node.attr("typeface") else {
            continue;
        };
        if let Some(reg) = ef.child_by_local("regular")
            && let Some(rid) = reg.attr("r:id").or_else(|| reg.attr("id"))
            && let Some(rel) = rels.get(rid)
            && !rel.external
        {
            let font_path = resolve_zip_path("ppt/presentation.xml", &rel.target);
            if let Ok(raw) = read_zip_bytes(archive, &font_path, max_bytes) {
                let decoded = if font_path.ends_with(".odttf") {
                    let guid = font_path
                        .rsplit_once('/')
                        .map_or(font_path.as_str(), |(_, f)| f);
                    deobfuscate_odttf(&raw, guid).unwrap_or(raw)
                } else {
                    raw
                };
                pres.fonts.insert(
                    typeface.to_string(),
                    FontResource {
                        family: typeface.to_string(),
                        style: "Regular".to_string(),
                        embedded: true,
                        data: Some(decoded),
                    },
                );
            }
        }
    }
    Ok(())
}

fn parse_slide_master(
    part_path: &str,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<SlideMaster> {
    let xml = read_zip_string(archive, part_path, max_bytes)?;
    let rels = parse_part_rels(archive, part_path, max_bytes)?;
    record_external_rels(&rels, pres);

    let mut theme_id = None;
    for rel in rels.values() {
        if rel.rel_type.ends_with("/theme") && !rel.external {
            theme_id = Some(resolve_zip_path(part_path, &rel.target));
            break;
        }
    }

    let default_theme = DrawingTheme::default();
    let theme = theme_id
        .as_deref()
        .and_then(|tid| pres.themes.get(tid))
        .unwrap_or(&default_theme)
        .clone();

    let root = XmlNode::parse(&xml);
    let mut color_map = BTreeMap::new();
    if let Some(cm) = root.child_by_local("clrMap") {
        for (k, v) in &cm.attrs {
            color_map.insert(local_name(k).to_string(), v.clone());
        }
    }

    let mut background = ShapeFill::Inherited;
    let mut shapes = Vec::new();
    if let Some(csld) = root.child_by_local("cSld") {
        if let Some(bg) = csld.child_by_local("bg") {
            background = parse_background(
                bg, part_path, &rels, &theme, &color_map, archive, max_bytes, pres,
            )?;
        }
        if let Some(sp_tree) = csld.child_by_local("spTree") {
            shapes = parse_shape_tree(
                sp_tree, part_path, &rels, &theme, &color_map, archive, max_bytes, pres, false,
            )?;
        }
    }

    let mut title_style = TextListStyle::default();
    let mut body_style = TextListStyle::default();
    let mut other_style = TextListStyle::default();
    if let Some(tx_styles) = root.child_by_local("txStyles") {
        if let Some(ts) = tx_styles.child_by_local("titleStyle") {
            title_style = parse_text_list_style(ts, &theme, &color_map);
        }
        if let Some(bs) = tx_styles.child_by_local("bodyStyle") {
            body_style = parse_text_list_style(bs, &theme, &color_map);
        }
        if let Some(os) = tx_styles.child_by_local("otherStyle") {
            other_style = parse_text_list_style(os, &theme, &color_map);
        }
    }

    Ok(SlideMaster {
        id: part_path.to_string(),
        theme_id,
        background,
        color_map,
        shapes,
        title_style,
        body_style,
        other_style,
    })
}

fn parse_slide_layout(
    part_path: &str,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<SlideLayout> {
    let xml = read_zip_string(archive, part_path, max_bytes)?;
    let rels = parse_part_rels(archive, part_path, max_bytes)?;
    record_external_rels(&rels, pres);

    let mut master_id = None;
    for rel in rels.values() {
        if rel.rel_type.ends_with("/slideMaster") && !rel.external {
            master_id = Some(resolve_zip_path(part_path, &rel.target));
            break;
        }
    }

    let default_theme = DrawingTheme::default();
    let (theme, color_map) = if let Some(ref mid) = master_id
        && let Some(m) = pres.masters.get(mid)
    {
        let th = m
            .theme_id
            .as_deref()
            .and_then(|tid| pres.themes.get(tid))
            .unwrap_or(&default_theme)
            .clone();
        (th, m.color_map.clone())
    } else {
        (default_theme, BTreeMap::new())
    };

    let root = XmlNode::parse(&xml);
    let show_master_shapes = root
        .attr("showMasterSp")
        .is_none_or(|v| !matches!(v, "0" | "false"));

    let mut background = ShapeFill::Inherited;
    let mut shapes = Vec::new();
    if let Some(csld) = root.child_by_local("cSld") {
        if let Some(bg) = csld.child_by_local("bg") {
            background = parse_background(
                bg, part_path, &rels, &theme, &color_map, archive, max_bytes, pres,
            )?;
        }
        if let Some(sp_tree) = csld.child_by_local("spTree") {
            shapes = parse_shape_tree(
                sp_tree, part_path, &rels, &theme, &color_map, archive, max_bytes, pres, false,
            )?;
        }
    }

    Ok(SlideLayout {
        id: part_path.to_string(),
        master_id,
        show_master_shapes,
        background,
        shapes,
    })
}

#[allow(clippy::too_many_lines)]
fn parse_slide(
    part_path: &str,
    slide_num: u32,
    comment_authors_map: &BTreeMap<String, (String, String)>,
    distinct_authors: &mut BTreeSet<String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<Slide> {
    let xml = read_zip_string(archive, part_path, max_bytes)?;
    let rels = parse_part_rels(archive, part_path, max_bytes)?;
    record_external_rels(&rels, pres);

    let mut layout_id = None;
    let mut notes_path = None;
    let mut comment_paths = Vec::new();

    for rel in rels.values() {
        if rel.external {
            continue;
        }
        if rel.rel_type.ends_with("/slideLayout") {
            layout_id = Some(resolve_zip_path(part_path, &rel.target));
        } else if rel.rel_type.ends_with("/notesSlide") {
            notes_path = Some(resolve_zip_path(part_path, &rel.target));
        } else if rel.rel_type.ends_with("/comments") {
            comment_paths.push(resolve_zip_path(part_path, &rel.target));
        }
    }

    let default_theme = DrawingTheme::default();
    let (theme, color_map) = if let Some(ref lid) = layout_id
        && let Some(l) = pres.layouts.get(lid)
        && let Some(ref mid) = l.master_id
        && let Some(m) = pres.masters.get(mid)
    {
        let th = m
            .theme_id
            .as_deref()
            .and_then(|tid| pres.themes.get(tid))
            .unwrap_or(&default_theme)
            .clone();
        (th, m.color_map.clone())
    } else if let Some(m) = pres.masters.values().next() {
        let th = m
            .theme_id
            .as_deref()
            .and_then(|tid| pres.themes.get(tid))
            .unwrap_or(&default_theme)
            .clone();
        (th, m.color_map.clone())
    } else {
        (default_theme, BTreeMap::new())
    };

    let root = XmlNode::parse(&xml);
    let hidden = root
        .attr("show")
        .is_some_and(|v| matches!(v, "0" | "false"));
    let show_master_shapes = root
        .attr("showMasterSp")
        .is_none_or(|v| !matches!(v, "0" | "false"));

    let mut background = ShapeFill::Inherited;
    let mut shapes = Vec::new();
    if let Some(csld) = root.child_by_local("cSld") {
        if let Some(bg) = csld.child_by_local("bg") {
            background = parse_background(
                bg, part_path, &rels, &theme, &color_map, archive, max_bytes, pres,
            )?;
        }
        if let Some(sp_tree) = csld.child_by_local("spTree") {
            shapes = parse_shape_tree(
                sp_tree, part_path, &rels, &theme, &color_map, archive, max_bytes, pres, true,
            )?;
        }
    }

    let notes = if let Some(np) = notes_path {
        parse_notes_slide(&np, &theme, &color_map, archive, max_bytes, pres).ok()
    } else {
        None
    };

    let mut comments = Vec::new();
    for cp in &comment_paths {
        if let Ok(cm_xml) = read_zip_string(archive, cp, max_bytes) {
            let cm_root = XmlNode::parse(&cm_xml);
            for cm in cm_root.children_elements() {
                if cm.local_name() == "cm" {
                    let author_id = cm.attr("authorId").unwrap_or("0");
                    let (author, initials) = comment_authors_map
                        .get(author_id)
                        .cloned()
                        .unwrap_or_else(|| ("Reviewer".to_string(), "R".to_string()));
                    distinct_authors.insert(author.clone());
                    let mut x_pt = 24.0;
                    let mut y_pt = 24.0;
                    if let Some(pos) = cm.child_by_local("pos") {
                        // In OOXML Slide comments, pos/@x and pos/@y are in 1/8ths of a point (576 units/inch) or EMUs
                        if let Some(x) = pos.attr("x").and_then(|v| v.parse::<f32>().ok()) {
                            x_pt = if x > 10_000.0 {
                                x / EMU_PER_POINT
                            } else {
                                x / 8.0
                            };
                        }
                        if let Some(y) = pos.attr("y").and_then(|v| v.parse::<f32>().ok()) {
                            y_pt = if y > 10_000.0 {
                                y / EMU_PER_POINT
                            } else {
                                y / 8.0
                            };
                        }
                    }
                    let text = cm
                        .child_by_local("text")
                        .map(XmlNode::text_content)
                        .unwrap_or_default();
                    let date = cm.attr("dt").map(str::to_string);
                    comments.push(SlideComment {
                        author,
                        initials,
                        x_pt,
                        y_pt,
                        text,
                        date,
                    });
                }
            }
        }
    }

    Ok(Slide {
        number: slide_num,
        id: part_path.to_string(),
        layout_id,
        master_id: None,
        show_master_shapes,
        hidden,
        background,
        shapes,
        notes,
        comments,
    })
}

fn parse_notes_slide(
    part_path: &str,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<NotesSlide> {
    let xml = read_zip_string(archive, part_path, max_bytes)?;
    let rels = parse_part_rels(archive, part_path, max_bytes)?;
    let root = XmlNode::parse(&xml);
    let mut shapes = Vec::new();
    if let Some(csld) = root.child_by_local("cSld")
        && let Some(sp_tree) = csld.child_by_local("spTree")
    {
        shapes = parse_shape_tree(
            sp_tree, part_path, &rels, theme, color_map, archive, max_bytes, pres, false,
        )?;
    }
    let mut plain_text = String::new();
    for s in &shapes {
        let is_body_ph = s
            .placeholder
            .as_ref()
            .and_then(|p| p.ph_type.as_deref())
            .is_some_and(|t| t == "body");
        if is_body_ph
            && let ShapeKind::AutoShape {
                text_body: Some(ref tb),
            } = s.kind
        {
            for p in &tb.paragraphs {
                for r in &p.runs {
                    plain_text.push_str(&r.text);
                }
                plain_text.push('\n');
            }
        }
    }
    plain_text = plain_text.trim().to_string();
    Ok(NotesSlide { shapes, plain_text })
}

#[allow(clippy::too_many_arguments)]
fn parse_background(
    bg_node: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<ShapeFill> {
    if let Some(bg_pr) = bg_node.child_by_local("bgPr") {
        let fill = parse_fill_children(
            bg_pr, base_part, rels, theme, color_map, archive, max_bytes, pres,
        )?;
        if fill != ShapeFill::Inherited {
            return Ok(fill);
        }
    }
    if let Some(bg_ref) = bg_node.child_by_local("bgRef")
        && let Some(c) = resolve_color_from_node(bg_ref, theme, color_map)
    {
        return Ok(ShapeFill::Solid(c));
    }
    Ok(ShapeFill::Inherited)
}

#[allow(clippy::too_many_arguments)]
fn parse_shape_tree(
    sp_tree: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
    count_forensics: bool,
) -> Result<Vec<SlideShape>> {
    let mut shapes = Vec::new();
    for child in sp_tree.children_elements() {
        match child.local_name() {
            "sp" => {
                if let Some(s) = parse_sp_element(
                    child,
                    base_part,
                    rels,
                    theme,
                    color_map,
                    archive,
                    max_bytes,
                    pres,
                    count_forensics,
                )? {
                    shapes.push(s);
                }
            }
            "pic" => {
                if let Some(s) = parse_pic_element(
                    child,
                    base_part,
                    rels,
                    theme,
                    color_map,
                    archive,
                    max_bytes,
                    pres,
                    count_forensics,
                )? {
                    shapes.push(s);
                }
            }
            "grpSp" => {
                if let Some(s) = parse_grp_element(
                    child,
                    base_part,
                    rels,
                    theme,
                    color_map,
                    archive,
                    max_bytes,
                    pres,
                    count_forensics,
                )? {
                    shapes.push(s);
                }
            }
            "cxnSp" => {
                if let Some(s) = parse_cxn_element(
                    child, base_part, rels, theme, color_map, archive, max_bytes, pres,
                )? {
                    shapes.push(s);
                }
            }
            "graphicFrame" => {
                if let Some(s) = parse_graphic_frame(
                    child, base_part, rels, theme, color_map, archive, max_bytes, pres,
                )? {
                    shapes.push(s);
                }
            }
            "AlternateContent" => {
                // Prefer Choice if it has a graphicFrame or sp, otherwise Fallback
                let branch = child
                    .child_by_local("Fallback")
                    .or_else(|| child.child_by_local("Choice"));
                if let Some(b) = branch {
                    let sub = parse_shape_tree(
                        b,
                        base_part,
                        rels,
                        theme,
                        color_map,
                        archive,
                        max_bytes,
                        pres,
                        count_forensics,
                    )?;
                    shapes.extend(sub);
                }
            }
            _ => {}
        }
    }
    Ok(shapes)
}

#[allow(clippy::too_many_arguments)]
fn parse_sp_element(
    node: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
    count_forensics: bool,
) -> Result<Option<SlideShape>> {
    let mut shape = SlideShape::default();
    if let Some(nv) = node.child_by_local("nvSpPr") {
        parse_non_visual_props(nv, rels, &mut shape);
    }
    if let Some(sp_pr) = node.child_by_local("spPr") {
        parse_sp_pr(
            sp_pr, base_part, rels, theme, color_map, archive, max_bytes, pres, &mut shape,
        )?;
    }
    if let Some(style_node) = node.child_by_local("style") {
        apply_shape_style_ref(style_node, theme, color_map, &mut shape);
    }

    let text_body = node
        .child_by_local("txBody")
        .map(|tb| parse_text_body(tb, rels, theme, color_map));
    shape.kind = ShapeKind::AutoShape { text_body };

    if count_forensics {
        check_shape_forensics(&shape, pres);
    }
    Ok(Some(shape))
}

#[allow(clippy::too_many_arguments)]
fn parse_pic_element(
    node: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
    count_forensics: bool,
) -> Result<Option<SlideShape>> {
    let mut shape = SlideShape::default();
    let mut media_target = None;
    if let Some(nv) = node.child_by_local("nvPicPr") {
        parse_non_visual_props(nv, rels, &mut shape);
        if let Some(nv_pr) = nv.child_by_local("nvPr") {
            for c in nv_pr.children_elements() {
                if matches!(c.local_name(), "videoFile" | "audioFile" | "media")
                    && let Some(rid) = c.attr("r:link").or_else(|| c.attr("r:embed"))
                    && let Some(rel) = rels.get(rid)
                {
                    media_target = Some(rel.target.clone());
                }
            }
        }
    }
    if let Some(sp_pr) = node.child_by_local("spPr") {
        parse_sp_pr(
            sp_pr, base_part, rels, theme, color_map, archive, max_bytes, pres, &mut shape,
        )?;
    }

    let mut resource_id = None;
    let mut crop = None;
    let mut opacity = 1.0_f32;
    if let Some(bf) = node.child_by_local("blipFill") {
        if let Some(blip) = bf.child_by_local("blip")
            && let Some(rid) = blip.attr("r:embed")
        {
            if let Some(res_key) = load_pptx_image(rid, base_part, rels, archive, max_bytes, pres)?
            {
                resource_id = Some(res_key);
            }
            if let Some(alpha_mod) = blip.child_by_local("alphaModFix")
                && let Some(amt) = alpha_mod.attr("amt").and_then(|v| v.parse::<f32>().ok())
            {
                opacity = (amt / 100_000.0).clamp(0.0, 1.0);
            }
        }
        if let Some(sr) = bf.child_by_local("srcRect") {
            let pct = |k: &str| {
                sr.attr(k)
                    .and_then(|v| v.parse::<f32>().ok())
                    .map_or(0.0, |v| (v / 100_000.0).clamp(0.0, 1.0))
            };
            crop = Some((pct("l"), pct("t"), pct("r"), pct("b")));
        }
    }

    if let Some(target) = media_target {
        let label = format!(
            "Media: {}",
            target.rsplit_once('/').map_or(target.as_str(), |(_, f)| f)
        );
        shape.kind = ShapeKind::MediaPoster {
            poster_resource_id: resource_id,
            media_target: target,
            label,
        };
    } else if let Some(res_id) = resource_id {
        shape.kind = ShapeKind::Picture {
            resource_id: res_id,
            crop,
            opacity,
        };
    } else {
        return Ok(None);
    }

    if count_forensics {
        check_shape_forensics(&shape, pres);
    }
    Ok(Some(shape))
}

#[allow(clippy::too_many_arguments, clippy::similar_names)]
fn parse_grp_element(
    node: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
    count_forensics: bool,
) -> Result<Option<SlideShape>> {
    let mut shape = SlideShape::default();
    if let Some(nv) = node.child_by_local("nvGrpSpPr") {
        parse_non_visual_props(nv, rels, &mut shape);
    }

    let mut ch_off_x_pt = 0.0;
    let mut ch_off_y_pt = 0.0;
    let mut ch_ext_w_pt = 0.0;
    let mut ch_ext_h_pt = 0.0;

    if let Some(grp_pr) = node.child_by_local("grpSpPr")
        && let Some(xfrm) = grp_pr.child_by_local("xfrm")
    {
        parse_xfrm(xfrm, &mut shape);
        if let Some(ch_off) = xfrm.child_by_local("chOff") {
            ch_off_x_pt = ch_off
                .attr("x")
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(0.0, |v| v / EMU_PER_POINT);
            ch_off_y_pt = ch_off
                .attr("y")
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(0.0, |v| v / EMU_PER_POINT);
        }
        if let Some(ch_ext) = xfrm.child_by_local("chExt") {
            ch_ext_w_pt = ch_ext
                .attr("cx")
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(shape.width_pt, |v| v / EMU_PER_POINT);
            ch_ext_h_pt = ch_ext
                .attr("cy")
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(shape.height_pt, |v| v / EMU_PER_POINT);
        }
    }

    let children = parse_shape_tree(
        node,
        base_part,
        rels,
        theme,
        color_map,
        archive,
        max_bytes,
        pres,
        count_forensics,
    )?;

    shape.kind = ShapeKind::Group {
        ch_off_x_pt,
        ch_off_y_pt,
        ch_ext_w_pt,
        ch_ext_h_pt,
        children,
    };
    Ok(Some(shape))
}

#[allow(clippy::too_many_arguments)]
fn parse_cxn_element(
    node: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<Option<SlideShape>> {
    let mut shape = SlideShape::default();
    if let Some(nv) = node.child_by_local("nvCxnSpPr") {
        parse_non_visual_props(nv, rels, &mut shape);
    }
    let mut head_arrow = false;
    let mut tail_arrow = false;
    if let Some(sp_pr) = node.child_by_local("spPr") {
        parse_sp_pr(
            sp_pr, base_part, rels, theme, color_map, archive, max_bytes, pres, &mut shape,
        )?;
        if let Some(ln) = sp_pr.child_by_local("ln") {
            head_arrow = ln
                .child_by_local("headEnd")
                .and_then(|e| e.attr("type"))
                .is_some_and(|t| t != "none");
            tail_arrow = ln
                .child_by_local("tailEnd")
                .and_then(|e| e.attr("type"))
                .is_some_and(|t| t != "none");
        }
    }
    if let Some(style_node) = node.child_by_local("style") {
        apply_shape_style_ref(style_node, theme, color_map, &mut shape);
    }
    shape.kind = ShapeKind::Connector {
        head_arrow,
        tail_arrow,
    };
    Ok(Some(shape))
}

#[allow(clippy::too_many_arguments)]
fn parse_graphic_frame(
    node: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<Option<SlideShape>> {
    let mut shape = SlideShape::default();
    if let Some(nv) = node.child_by_local("nvGraphicFramePr") {
        parse_non_visual_props(nv, rels, &mut shape);
    }
    if let Some(xfrm) = node.child_by_local("xfrm") {
        parse_xfrm(xfrm, &mut shape);
    }

    // 1. Check for Table (`a:tbl`)
    if let Some(tbl_node) = find_node_by_local(node, "tbl") {
        let tbl = parse_slide_table(
            tbl_node, base_part, rels, theme, color_map, archive, max_bytes, pres,
        )?;
        shape.kind = ShapeKind::Table(tbl);
        return Ok(Some(shape));
    }

    // 2. Check for Chart (`c:chart` or `cx:chart`)
    if let Some(chart_node) = find_node_by_local(node, "chart")
        && let Some(rid) = chart_node.attr("r:id").or_else(|| chart_node.attr("id"))
        && let Some(rel) = rels.get(rid)
        && !rel.external
    {
        let chart_path = resolve_zip_path(base_part, &rel.target);
        if let Ok(chart_xml) = read_zip_string(archive, &chart_path, max_bytes)
            && let Ok(chart_space) = parse_chart_xml(&chart_xml)
        {
            shape.kind = ShapeKind::Chart(Box::new(chart_space));
            return Ok(Some(shape));
        }
    }

    // 3. Check for SmartArt (`dgm:relIds`)
    if let Some(rel_ids) = find_node_by_local(node, "relIds") {
        let (shapes, fallback_items) = parse_smartart(
            rel_ids, base_part, rels, theme, color_map, archive, max_bytes, pres,
        )?;
        shape.kind = ShapeKind::SmartArt {
            shapes,
            fallback_items,
        };
        return Ok(Some(shape));
    }

    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn parse_smartart(
    rel_ids: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<(Vec<SlideShape>, Vec<String>)> {
    let mut shapes = Vec::new();
    let mut fallback_items = Vec::new();

    // First look for pre-rendered diagram drawing (`dsp:drawing`) in slide relationships
    for rel in rels.values() {
        if !rel.external && rel.rel_type.contains("diagramDrawing") {
            let drawing_path = resolve_zip_path(base_part, &rel.target);
            if let Ok(d_xml) = read_zip_string(archive, &drawing_path, max_bytes) {
                let d_rels = parse_part_rels(archive, &drawing_path, max_bytes)?;
                let d_root = XmlNode::parse(&d_xml);
                if let Some(sp_tree) = find_node_by_local(&d_root, "spTree") {
                    shapes = parse_shape_tree(
                        sp_tree,
                        &drawing_path,
                        &d_rels,
                        theme,
                        color_map,
                        archive,
                        max_bytes,
                        pres,
                        false,
                    )?;
                }
            }
        }
    }

    // Also parse `r:dm` (`dgm:dataModel`) for fallback text items
    if let Some(dm_rid) = rel_ids.attr("r:dm").or_else(|| rel_ids.attr("dm"))
        && let Some(rel) = rels.get(dm_rid)
        && !rel.external
    {
        let dm_path = resolve_zip_path(base_part, &rel.target);
        if let Ok(dm_xml) = read_zip_string(archive, &dm_path, max_bytes) {
            let dm_root = XmlNode::parse(&dm_xml);
            if let Some(pt_lst) = find_node_by_local(&dm_root, "ptLst") {
                for pt in pt_lst.children_elements() {
                    if pt.local_name() == "pt"
                        && let Some(t) = pt.child_by_local("t")
                    {
                        let txt = t.text_content().trim().to_string();
                        if !txt.is_empty() {
                            fallback_items.push(txt);
                        }
                    }
                }
            }
        }
    }

    Ok((shapes, fallback_items))
}

#[allow(clippy::too_many_arguments)]
fn parse_slide_table(
    tbl_node: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<SlideTable> {
    let mut table = SlideTable::default();
    if let Some(pr) = tbl_node.child_by_local("tblPr") {
        table.first_row = pr
            .attr("firstRow")
            .is_some_and(|v| matches!(v, "1" | "true"));
        table.band_row = pr
            .attr("bandRow")
            .is_some_and(|v| matches!(v, "1" | "true"));
        table.style_id = pr
            .child_by_local("tableStyleId")
            .map(|n| n.text_content().trim().to_string());
    }
    if let Some(grid) = tbl_node.child_by_local("tblGrid") {
        for col in grid.children_elements() {
            if col.local_name() == "gridCol"
                && let Some(w) = col.attr("w").and_then(|v| v.parse::<f32>().ok())
            {
                table.col_widths_pt.push((w / EMU_PER_POINT).max(6.0));
            }
        }
    }
    for tr in tbl_node.children_elements() {
        if tr.local_name() != "tr" {
            continue;
        }
        let height_pt = tr
            .attr("h")
            .and_then(|v| v.parse::<f32>().ok())
            .map_or(18.0, |v| (v / EMU_PER_POINT).max(6.0));
        let mut cells = Vec::new();
        for tc in tr.children_elements() {
            if tc.local_name() != "tc" {
                continue;
            }
            let grid_span = tc
                .attr("gridSpan")
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            let row_span = tc
                .attr("rowSpan")
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            let h_merge = tc.attr("hMerge").is_some_and(|v| matches!(v, "1" | "true"));
            let v_merge = tc.attr("vMerge").is_some_and(|v| matches!(v, "1" | "true"));
            let text_body = tc
                .child_by_local("txBody")
                .map(|tb| parse_text_body(tb, rels, theme, color_map));

            let mut fill = ShapeFill::Inherited;
            let mut border_top = None;
            let mut border_bottom = None;
            let mut border_left = None;
            let mut border_right = None;
            if let Some(tc_pr) = tc.child_by_local("tcPr") {
                fill = parse_fill_children(
                    tc_pr, base_part, rels, theme, color_map, archive, max_bytes, pres,
                )?;
                border_top = tc_pr
                    .child_by_local("lnT")
                    .and_then(|ln| parse_stroke(ln, theme, color_map));
                border_bottom = tc_pr
                    .child_by_local("lnB")
                    .and_then(|ln| parse_stroke(ln, theme, color_map));
                border_left = tc_pr
                    .child_by_local("lnL")
                    .and_then(|ln| parse_stroke(ln, theme, color_map));
                border_right = tc_pr
                    .child_by_local("lnR")
                    .and_then(|ln| parse_stroke(ln, theme, color_map));
            }
            cells.push(SlideTableCell {
                text_body,
                grid_span,
                row_span,
                h_merge,
                v_merge,
                fill,
                border_top,
                border_bottom,
                border_left,
                border_right,
            });
        }
        table.rows.push(SlideTableRow { height_pt, cells });
    }
    Ok(table)
}

fn parse_non_visual_props(
    nv_container: &XmlNode,
    rels: &BTreeMap<String, RelEntry>,
    shape: &mut SlideShape,
) {
    if let Some(c_nv_pr) = nv_container.child_by_local("cNvPr") {
        shape.id = c_nv_pr.attr("id").unwrap_or("").to_string();
        shape.name = c_nv_pr.attr("name").unwrap_or("").to_string();
        shape.hidden = c_nv_pr
            .attr("hidden")
            .is_some_and(|v| matches!(v, "1" | "true"));
        shape.alt_text = c_nv_pr
            .attr("descr")
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(hl) = c_nv_pr.child_by_local("hlinkClick")
            && let Some(rid) = hl.attr("r:id").or_else(|| hl.attr("id"))
            && let Some(rel) = rels.get(rid)
        {
            shape.hyperlink = Some(rel.target.clone());
        }
    }
    if let Some(nv_pr) = nv_container.child_by_local("nvPr")
        && let Some(ph) = nv_pr.child_by_local("ph")
    {
        shape.placeholder = Some(PlaceholderRef {
            ph_type: ph.attr("type").map(str::to_string),
            idx: ph.attr("idx").and_then(|v| v.parse::<u32>().ok()),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_sp_pr(
    sp_pr: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
    shape: &mut SlideShape,
) -> Result<()> {
    if let Some(xfrm) = sp_pr.child_by_local("xfrm") {
        parse_xfrm(xfrm, shape);
    }
    if let Some(prst) = sp_pr.child_by_local("prstGeom") {
        shape.preset_geom = prst.attr("prst").map(str::to_string);
        if let Some(av_lst) = prst.child_by_local("avLst") {
            for gd in av_lst.children_elements() {
                if gd.local_name() == "gd"
                    && let (Some(name), Some(fmla)) = (gd.attr("name"), gd.attr("fmla"))
                    && let Some(val_str) = fmla.strip_prefix("val ")
                    && let Ok(v) = val_str.trim().parse::<i64>()
                {
                    shape.adjust_values.insert(name.to_string(), v);
                }
            }
        }
    }
    if let Some(cust) = sp_pr.child_by_local("custGeom") {
        shape.custom_geom = Some(parse_custom_geom(cust));
    }

    shape.fill = parse_fill_children(
        sp_pr, base_part, rels, theme, color_map, archive, max_bytes, pres,
    )?;
    if let Some(ln) = sp_pr.child_by_local("ln") {
        shape.stroke = parse_stroke(ln, theme, color_map);
    }
    Ok(())
}

fn parse_xfrm(xfrm: &XmlNode, shape: &mut SlideShape) {
    if let Some(rot) = xfrm.attr("rot").and_then(|v| v.parse::<f32>().ok()) {
        shape.rotation_deg = rot / 60_000.0;
    }
    shape.flip_h = xfrm
        .attr("flipH")
        .is_some_and(|v| matches!(v, "1" | "true"));
    shape.flip_v = xfrm
        .attr("flipV")
        .is_some_and(|v| matches!(v, "1" | "true"));
    if let Some(off) = xfrm.child_by_local("off") {
        if let Some(x) = off.attr("x").and_then(|v| v.parse::<f32>().ok()) {
            shape.x_pt = x / EMU_PER_POINT;
            shape.has_explicit_xfrm = true;
        }
        if let Some(y) = off.attr("y").and_then(|v| v.parse::<f32>().ok()) {
            shape.y_pt = y / EMU_PER_POINT;
            shape.has_explicit_xfrm = true;
        }
    }
    if let Some(ext) = xfrm.child_by_local("ext") {
        if let Some(cx) = ext.attr("cx").and_then(|v| v.parse::<f32>().ok()) {
            shape.width_pt = cx / EMU_PER_POINT;
            shape.has_explicit_xfrm = true;
        }
        if let Some(cy) = ext.attr("cy").and_then(|v| v.parse::<f32>().ok()) {
            shape.height_pt = cy / EMU_PER_POINT;
            shape.has_explicit_xfrm = true;
        }
    }
}

fn parse_custom_geom(cust: &XmlNode) -> CustomGeometry {
    let mut cg = CustomGeometry::default();
    if let Some(av_lst) = cust.child_by_local("avLst") {
        for gd in av_lst.children_elements() {
            if gd.local_name() == "gd"
                && let (Some(name), Some(fmla)) = (gd.attr("name"), gd.attr("fmla"))
            {
                cg.adjust_values.push(GuideDef {
                    name: name.to_string(),
                    fmla: fmla.to_string(),
                });
            }
        }
    }
    if let Some(gd_lst) = cust.child_by_local("gdLst") {
        for gd in gd_lst.children_elements() {
            if gd.local_name() == "gd"
                && let (Some(name), Some(fmla)) = (gd.attr("name"), gd.attr("fmla"))
            {
                cg.guides.push(GuideDef {
                    name: name.to_string(),
                    fmla: fmla.to_string(),
                });
            }
        }
    }
    if let Some(path_lst) = cust.child_by_local("pathLst") {
        for p in path_lst.children_elements() {
            if p.local_name() != "path" {
                continue;
            }
            let width = p.attr("w").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let height = p.attr("h").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let mut commands = Vec::new();
            for cmd in p.children_elements() {
                match cmd.local_name() {
                    "moveTo" => {
                        if let Some(pt) = cmd.child_by_local("pt") {
                            let (x, y) = parse_custom_pt(pt);
                            commands.push(CustomPathCmd::MoveTo { x, y });
                        }
                    }
                    "lnTo" => {
                        if let Some(pt) = cmd.child_by_local("pt") {
                            let (x, y) = parse_custom_pt(pt);
                            commands.push(CustomPathCmd::LineTo { x, y });
                        }
                    }
                    "cubicBezTo" => {
                        let pts: Vec<(String, String)> = cmd
                            .children_elements()
                            .filter(|c| c.local_name() == "pt")
                            .map(parse_custom_pt)
                            .collect();
                        if pts.len() == 3 {
                            commands.push(CustomPathCmd::CubicBezTo {
                                pt1: pts[0].clone(),
                                pt2: pts[1].clone(),
                                pt3: pts[2].clone(),
                            });
                        }
                    }
                    "quadBezTo" => {
                        let pts: Vec<(String, String)> = cmd
                            .children_elements()
                            .filter(|c| c.local_name() == "pt")
                            .map(parse_custom_pt)
                            .collect();
                        if pts.len() == 2 {
                            commands.push(CustomPathCmd::QuadBezTo {
                                pt1: pts[0].clone(),
                                pt2: pts[1].clone(),
                            });
                        }
                    }
                    "arcTo" => {
                        commands.push(CustomPathCmd::ArcTo {
                            wr: cmd.attr("wR").unwrap_or("0").to_string(),
                            hr: cmd.attr("hR").unwrap_or("0").to_string(),
                            st_ang: cmd.attr("stAng").unwrap_or("0").to_string(),
                            sw_ang: cmd.attr("swAng").unwrap_or("0").to_string(),
                        });
                    }
                    "close" => commands.push(CustomPathCmd::Close),
                    _ => {}
                }
            }
            cg.paths.push(CustomPath {
                width,
                height,
                commands,
            });
        }
    }
    cg
}

fn parse_custom_pt(pt: &XmlNode) -> (String, String) {
    (
        pt.attr("x").unwrap_or("0").to_string(),
        pt.attr("y").unwrap_or("0").to_string(),
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_fill_children(
    container: &XmlNode,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<ShapeFill> {
    for c in container.children_elements() {
        match c.local_name() {
            "noFill" => return Ok(ShapeFill::NoFill),
            "solidFill" => {
                if let Some(rgba) = resolve_color_from_node(c, theme, color_map) {
                    return Ok(ShapeFill::Solid(rgba));
                }
            }
            "gradFill" => {
                let mut stops = Vec::new();
                if let Some(gs_lst) = c.child_by_local("gsLst") {
                    for gs in gs_lst.children_elements() {
                        if gs.local_name() == "gs" {
                            let pos = gs
                                .attr("pos")
                                .and_then(|v| v.parse::<f32>().ok())
                                .map_or(0.0, |v| (v / 100_000.0).clamp(0.0, 1.0));
                            if let Some(col) = resolve_color_from_node(gs, theme, color_map) {
                                stops.push((pos, col));
                            }
                        }
                    }
                }
                let angle_deg = c
                    .child_by_local("lin")
                    .and_then(|l| l.attr("ang"))
                    .and_then(|v| v.parse::<f32>().ok())
                    .map_or(90.0, |v| v / 60_000.0);
                if !stops.is_empty() {
                    return Ok(ShapeFill::LinearGradient { angle_deg, stops });
                }
            }
            "blipFill" => {
                if let Some(blip) = c.child_by_local("blip")
                    && let Some(rid) = blip.attr("r:embed")
                    && let Some(res_id) =
                        load_pptx_image(rid, base_part, rels, archive, max_bytes, pres)?
                {
                    return Ok(ShapeFill::Picture {
                        resource_id: res_id,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(ShapeFill::Inherited)
}

fn parse_stroke(
    ln: &XmlNode,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
) -> Option<ShapeStroke> {
    if ln.child_by_local("noFill").is_some() {
        return None;
    }
    let width_pt = ln
        .attr("w")
        .and_then(|v| v.parse::<f32>().ok())
        .map_or(0.75, |v| (v / EMU_PER_POINT).max(0.25));
    let color = ln
        .child_by_local("solidFill")
        .and_then(|sf| resolve_color_from_node(sf, theme, color_map))
        .unwrap_or(Rgba::BLACK);
    let dash = ln
        .child_by_local("prstDash")
        .and_then(|d| d.attr("val"))
        .map(str::to_string);
    Some(ShapeStroke {
        color,
        width_pt,
        dash,
    })
}

fn apply_shape_style_ref(
    style_node: &XmlNode,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
    shape: &mut SlideShape,
) {
    if shape.fill == ShapeFill::Inherited
        && let Some(fill_ref) = style_node.child_by_local("fillRef")
    {
        let idx = fill_ref
            .attr("idx")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        if idx > 0
            && let Some(col) = resolve_color_from_node(fill_ref, theme, color_map)
        {
            shape.fill = ShapeFill::Solid(col);
        }
    }
    if shape.stroke.is_none()
        && let Some(ln_ref) = style_node.child_by_local("lnRef")
    {
        let idx = ln_ref
            .attr("idx")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        if idx > 0
            && let Some(col) = resolve_color_from_node(ln_ref, theme, color_map)
        {
            shape.stroke = Some(ShapeStroke {
                color: col,
                width_pt: 0.75,
                dash: None,
            });
        }
    }
}

fn parse_text_body(
    tb_node: &XmlNode,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
) -> TextBody {
    let mut tb = TextBody::default();
    if let Some(bpr) = tb_node.child_by_local("bodyPr") {
        if let Some(v) = bpr.attr("lIns").and_then(|v| v.parse::<f32>().ok()) {
            tb.inset_left_pt = v / EMU_PER_POINT;
        }
        if let Some(v) = bpr.attr("tIns").and_then(|v| v.parse::<f32>().ok()) {
            tb.inset_top_pt = v / EMU_PER_POINT;
        }
        if let Some(v) = bpr.attr("rIns").and_then(|v| v.parse::<f32>().ok()) {
            tb.inset_right_pt = v / EMU_PER_POINT;
        }
        if let Some(v) = bpr.attr("bIns").and_then(|v| v.parse::<f32>().ok()) {
            tb.inset_bottom_pt = v / EMU_PER_POINT;
        }
        if let Some(wrap) = bpr.attr("wrap") {
            tb.word_wrap = wrap != "none";
        }
        if let Some(anc) = bpr.attr("anchor") {
            tb.vertical_anchor = match anc {
                "ctr" => TextVerticalAnchor::Middle,
                "b" => TextVerticalAnchor::Bottom,
                _ => TextVerticalAnchor::Top,
            };
        }
        if let Some(nc) = bpr.attr("numCol").and_then(|v| v.parse::<u8>().ok()) {
            tb.num_cols = nc.clamp(1, 16);
        }
        if let Some(sc) = bpr.attr("spcCol").and_then(|v| v.parse::<f32>().ok()) {
            tb.col_spacing_pt = sc / EMU_PER_POINT;
        }
        for c in bpr.children_elements() {
            match c.local_name() {
                "noAutofit" => tb.autofit = AutofitMode::None,
                "spAutoFit" => tb.autofit = AutofitMode::Shape,
                "normAutofit" => {
                    let font_scale_pct = c
                        .attr("fontScale")
                        .and_then(|v| v.parse::<f32>().ok())
                        .map_or(100.0, |v| (v / 1000.0).clamp(10.0, 100.0));
                    let line_space_reduction_pct = c
                        .attr("lnSpcReduction")
                        .and_then(|v| v.parse::<f32>().ok())
                        .map_or(0.0, |v| (v / 1000.0).clamp(0.0, 90.0));
                    tb.autofit = AutofitMode::Normal {
                        font_scale_pct,
                        line_space_reduction_pct,
                    };
                }
                _ => {}
            }
        }
    }

    if let Some(lst) = tb_node.child_by_local("lstStyle") {
        tb.list_style = parse_text_list_style(lst, theme, color_map);
    }

    for c in tb_node.children_elements() {
        if c.local_name() == "p" {
            tb.paragraphs
                .push(parse_text_paragraph(c, rels, theme, color_map));
        }
    }
    tb
}

fn parse_text_list_style(
    node: &XmlNode,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
) -> TextListStyle {
    let mut ls = TextListStyle::default();
    for c in node.children_elements() {
        let name = c.local_name();
        let lvl = match name {
            "defPPr" | "lvl1pPr" => Some(0),
            "lvl2pPr" => Some(1),
            "lvl3pPr" => Some(2),
            "lvl4pPr" => Some(3),
            "lvl5pPr" => Some(4),
            "lvl6pPr" => Some(5),
            "lvl7pPr" => Some(6),
            "lvl8pPr" => Some(7),
            "lvl9pPr" => Some(8),
            _ => None,
        };
        if let Some(level) = lvl {
            let mut style = parse_level_style(c, theme, color_map);
            style.level = level;
            ls.levels.insert(level, style);
        }
    }
    ls
}

fn parse_level_style(
    ppr: &XmlNode,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
) -> TextLevelStyle {
    let mut style = TextLevelStyle::default();
    if let Some(lvl) = ppr.attr("lvl").and_then(|v| v.parse::<u8>().ok()) {
        style.level = lvl.min(8);
    }
    if let Some(algn) = ppr.attr("algn") {
        style.align = Some(match algn {
            "ctr" => PptTextAlign::Center,
            "r" => PptTextAlign::Right,
            "just" | "dist" => PptTextAlign::Justify,
            _ => PptTextAlign::Left,
        });
    }
    if let Some(mar_l) = ppr.attr("marL").and_then(|v| v.parse::<f32>().ok()) {
        style.margin_left_pt = Some(mar_l / EMU_PER_POINT);
    }
    if let Some(indent) = ppr.attr("indent").and_then(|v| v.parse::<f32>().ok()) {
        style.indent_pt = Some(indent / EMU_PER_POINT);
    }

    for c in ppr.children_elements() {
        match c.local_name() {
            "lnSpc" => {
                if let Some(pct) = c
                    .child_by_local("spcPct")
                    .and_then(|s| s.attr("val"))
                    .and_then(|v| v.parse::<f32>().ok())
                {
                    style.line_spacing_mult = Some((pct / 100_000.0).clamp(0.5, 4.0));
                } else if let Some(pts) = c
                    .child_by_local("spcPts")
                    .and_then(|s| s.attr("val"))
                    .and_then(|v| v.parse::<f32>().ok())
                {
                    style.line_spacing_mult = Some(-(pts / 100.0).max(4.0));
                }
            }
            "spcBef" => {
                style.space_before_pt = parse_spacing_pts(c);
            }
            "spcAft" => {
                style.space_after_pt = parse_spacing_pts(c);
            }
            "buNone" => style.bullet = BulletKind::None,
            "buChar" => {
                if let Some(ch) = c.attr("char") {
                    style.bullet = BulletKind::Char(ch.to_string());
                }
            }
            "buAutoNum" => {
                let scheme = c.attr("type").unwrap_or("arabicPeriod").to_string();
                let start_at = c
                    .attr("startAt")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(1)
                    .max(1);
                style.bullet = BulletKind::AutoNum { scheme, start_at };
            }
            "defRPr" => {
                style.default_run = parse_run_style(c, &BTreeMap::new(), theme, color_map);
            }
            _ => {}
        }
    }
    style
}

fn parse_spacing_pts(spc_node: &XmlNode) -> Option<f32> {
    if let Some(pts) = spc_node
        .child_by_local("spcPts")
        .and_then(|s| s.attr("val"))
        .and_then(|v| v.parse::<f32>().ok())
    {
        return Some((pts / 100.0).max(0.0));
    }
    if let Some(pct) = spc_node
        .child_by_local("spcPct")
        .and_then(|s| s.attr("val"))
        .and_then(|v| v.parse::<f32>().ok())
    {
        return Some((pct / 100_000.0 * 18.0).max(0.0));
    }
    None
}

fn parse_text_paragraph(
    p_node: &XmlNode,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
) -> TextParagraph {
    let mut para = TextParagraph::default();
    if let Some(ppr) = p_node.child_by_local("pPr") {
        para.props = parse_level_style(ppr, theme, color_map);
    }
    for c in p_node.children_elements() {
        match c.local_name() {
            "r" => {
                let style = c
                    .child_by_local("rPr")
                    .map_or_else(TextRunStyle::default, |rpr| {
                        parse_run_style(rpr, rels, theme, color_map)
                    });
                let text = c
                    .child_by_local("t")
                    .map(XmlNode::text_content)
                    .unwrap_or_default();
                if !text.is_empty() {
                    para.runs.push(PptTextRun {
                        text,
                        style,
                        field_type: None,
                    });
                }
            }
            "br" => {
                let style = c
                    .child_by_local("rPr")
                    .map_or_else(TextRunStyle::default, |rpr| {
                        parse_run_style(rpr, rels, theme, color_map)
                    });
                para.runs.push(PptTextRun {
                    text: "\n".to_string(),
                    style,
                    field_type: None,
                });
            }
            "fld" => {
                let style = c
                    .child_by_local("rPr")
                    .map_or_else(TextRunStyle::default, |rpr| {
                        parse_run_style(rpr, rels, theme, color_map)
                    });
                let text = c
                    .child_by_local("t")
                    .map(XmlNode::text_content)
                    .unwrap_or_default();
                let field_type = c.attr("type").map(str::to_string);
                para.runs.push(PptTextRun {
                    text,
                    style,
                    field_type,
                });
            }
            _ => {}
        }
    }
    para
}

fn parse_run_style(
    rpr: &XmlNode,
    rels: &BTreeMap<String, RelEntry>,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
) -> TextRunStyle {
    let mut style = TextRunStyle::default();
    if let Some(sz) = rpr.attr("sz").and_then(|v| v.parse::<f32>().ok()) {
        style.font_size_pt = Some((sz / 100.0).clamp(2.0, 400.0));
    }
    if let Some(b) = rpr.attr("b") {
        style.bold = Some(matches!(b, "1" | "true"));
    }
    if let Some(i) = rpr.attr("i") {
        style.italic = Some(matches!(i, "1" | "true"));
    }
    if let Some(u) = rpr.attr("u") {
        style.underline = Some(u != "none");
    }
    if let Some(s) = rpr.attr("strike") {
        style.strikethrough = Some(s != "noStrike");
    }
    if let Some(bl) = rpr.attr("baseline").and_then(|v| v.parse::<f32>().ok()) {
        style.baseline_pct = Some(bl / 1000.0);
    }
    if let Some(latin) = rpr.child_by_local("latin")
        && let Some(tf) = latin.attr("typeface")
    {
        let resolved = match tf {
            "+mj-lt" | "+mj-ea" | "+mj-cs" => theme.major_latin_font.as_str(),
            "+mn-lt" | "+mn-ea" | "+mn-cs" => theme.minor_latin_font.as_str(),
            other => other,
        };
        style.font_family = Some(resolved.to_string());
    }
    if let Some(sf) = rpr.child_by_local("solidFill") {
        style.color = resolve_color_from_node(sf, theme, color_map);
    }
    if let Some(hl) = rpr.child_by_local("hlinkClick")
        && let Some(rid) = hl.attr("r:id").or_else(|| hl.attr("id"))
        && let Some(rel) = rels.get(rid)
    {
        style.hyperlink = Some(rel.target.clone());
    }
    style
}

fn resolve_color_from_node(
    container: &XmlNode,
    theme: &DrawingTheme,
    color_map: &BTreeMap<String, String>,
) -> Option<Rgba> {
    for c in container.children_elements() {
        let transforms = parse_color_transforms(c);
        match c.local_name() {
            "srgbClr" => {
                if let Some(val) = c.attr("val")
                    && let Some(base) = parse_hex_rgba(val)
                {
                    return Some(apply_color_transforms(base, &transforms));
                }
            }
            "schemeClr" => {
                if let Some(val) = c.attr("val") {
                    let mapped = color_map.get(val).map_or(val, String::as_str);
                    return theme.resolve_transformed(mapped, &transforms);
                }
            }
            "sysClr" => {
                if let Some(last) = c.attr("lastClr")
                    && let Some(base) = parse_hex_rgba(last)
                {
                    return Some(apply_color_transforms(base, &transforms));
                }
                if let Some(val) = c.attr("val") {
                    let fallback = match val {
                        "window" | "windowFrame" => Rgba::WHITE,
                        _ => Rgba::BLACK,
                    };
                    return Some(apply_color_transforms(fallback, &transforms));
                }
            }
            "prstClr" => {
                if let Some(val) = c.attr("val")
                    && let Some(base) = Rgba::from_css_str(val)
                {
                    return Some(apply_color_transforms(base, &transforms));
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_color_transforms(color_node: &XmlNode) -> Vec<ColorTransform> {
    let mut out = Vec::new();
    for t in color_node.children_elements() {
        let val = t
            .attr("val")
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0);
        match t.local_name() {
            "lumMod" => out.push(ColorTransform::LumMod(val / 100_000.0)),
            "lumOff" => out.push(ColorTransform::LumOff(val / 100_000.0)),
            "tint" => out.push(ColorTransform::Tint(val / 100_000.0)),
            "shade" => out.push(ColorTransform::Shade(val / 100_000.0)),
            "satMod" => out.push(ColorTransform::SatMod(val / 100_000.0)),
            "satOff" => out.push(ColorTransform::SatOff(val / 100_000.0)),
            "hueMod" => out.push(ColorTransform::HueMod(val / 100_000.0)),
            "hueOff" => out.push(ColorTransform::HueOff(val / 60_000.0)),
            "alpha" => out.push(ColorTransform::Alpha(val / 100_000.0)),
            "comp" => out.push(ColorTransform::Comp),
            "inv" => out.push(ColorTransform::Inv),
            _ => {}
        }
    }
    out
}

#[allow(clippy::unnecessary_wraps)]
fn load_pptx_image(
    rid: &str,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    pres: &mut Presentation,
) -> Result<Option<String>> {
    let Some(rel) = rels.get(rid) else {
        return Ok(None);
    };
    if rel.external {
        return Ok(None);
    }
    let zip_path = resolve_zip_path(base_part, &rel.target);
    if pres.images.contains_key(&zip_path) {
        return Ok(Some(zip_path));
    }
    let Ok(data) = read_zip_bytes(archive, &zip_path, max_bytes) else {
        return Ok(None);
    };
    let (width, height) = image::ImageReader::new(Cursor::new(&data))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((100, 100));
    let mime_type = guess_image_mime(&zip_path, &data);
    pres.images.insert(
        zip_path.clone(),
        ImageResource {
            id: zip_path.clone(),
            mime_type,
            data: Some(data),
            url: None,
            width,
            height,
        },
    );
    Ok(Some(zip_path))
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn guess_image_mime(path: &str, data: &[u8]) -> String {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png".to_string();
    }
    if data.starts_with(b"\xFF\xD8\xFF") {
        return "image/jpeg".to_string();
    }
    if data.starts_with(b"GIF8") {
        return "image/gif".to_string();
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".emf") {
        "image/x-emf".to_string()
    } else if lower.ends_with(".wmf") {
        "image/x-wmf".to_string()
    } else if lower.ends_with(".svg") {
        "image/svg+xml".to_string()
    } else {
        "image/png".to_string()
    }
}

fn check_shape_forensics(shape: &SlideShape, pres: &mut Presentation) {
    if shape.hidden {
        pres.forensic.hidden_shape_count += 1;
    }
    if shape.has_explicit_xfrm
        && (shape.x_pt + shape.width_pt < 0.0
            || shape.y_pt + shape.height_pt < 0.0
            || shape.x_pt > pres.slide_width_pt
            || shape.y_pt > pres.slide_height_pt)
    {
        pres.forensic.off_slide_shape_count += 1;
    }
}

// =============================================================================
// Lightweight DOM-like XML helper for OOXML parts
// =============================================================================

/// Maximum element nesting kept in the [`XmlNode`] tree.
const MAX_XML_DEPTH: usize = 128;

#[derive(Debug, Clone, Default)]
struct XmlNode {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<XmlChild>,
}

#[derive(Debug, Clone)]
enum XmlChild {
    Element(XmlNode),
    Text(String),
}

impl XmlNode {
    fn parse(xml: &str) -> Self {
        let mut reader = Reader::from_str(xml);
        let mut stack: Vec<XmlNode> = vec![XmlNode {
            name: "root".to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
        }];
        // Elements nested deeper than `MAX_XML_DEPTH` are not kept as nodes; their content
        // is attached to the deepest kept ancestor, and their end tags must not pop it.
        let mut skipped_depth = 0usize;

        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    if stack.len() < MAX_XML_DEPTH {
                        stack.push(Self::from_start(&e));
                    } else {
                        skipped_depth += 1;
                    }
                }
                Ok(Event::Empty(e)) => {
                    let node = Self::from_start(&e);
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(XmlChild::Element(node));
                    }
                }
                Ok(Event::End(_)) => {
                    if skipped_depth > 0 {
                        skipped_depth -= 1;
                    } else if stack.len() > 1
                        && let Some(done) = stack.pop()
                        && let Some(parent) = stack.last_mut()
                    {
                        parent.children.push(XmlChild::Element(done));
                    }
                }
                Ok(Event::Text(t)) => {
                    let text = quick_xml::escape::unescape(t.as_ref())
                        .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                    if !text.is_empty()
                        && let Some(parent) = stack.last_mut()
                    {
                        parent.children.push(XmlChild::Text(text));
                    }
                }
                Ok(Event::CData(c)) => {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(XmlChild::Text(c.as_ref().to_string()));
                    }
                }
                Ok(Event::GeneralRef(e)) => {
                    let resolved = if let Ok(Some(ch)) = e.resolve_char_ref() {
                        Some(ch.to_string())
                    } else {
                        quick_xml::escape::resolve_predefined_entity(e.as_ref()).map(str::to_string)
                    };
                    if let Some(text) = resolved
                        && let Some(parent) = stack.last_mut()
                    {
                        parent.children.push(XmlChild::Text(text));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }

        while stack.len() > 1 {
            if let Some(done) = stack.pop()
                && let Some(parent) = stack.last_mut()
            {
                parent.children.push(XmlChild::Element(done));
            }
        }

        let root = stack.pop().unwrap_or_default();
        for child in root.children {
            if let XmlChild::Element(first) = child {
                return first;
            }
        }
        XmlNode::default()
    }

    fn from_start(e: &BytesStart<'_>) -> Self {
        let name = e.name().as_ref().to_string();
        let mut attrs = Vec::new();
        for attr in e.attributes().flatten() {
            let key = attr.key.as_ref().to_string();
            let raw = attr.value.as_ref();
            let val = quick_xml::escape::unescape(raw)
                .map_or_else(|_| raw.to_owned(), std::borrow::Cow::into_owned);
            attrs.push((key, val));
        }
        Self {
            name,
            attrs,
            children: Vec::new(),
        }
    }

    fn local_name(&self) -> &str {
        local_name(&self.name)
    }

    fn attr(&self, key: &str) -> Option<&str> {
        let target_local = local_name(key);
        for (k, v) in &self.attrs {
            if k == key || local_name(k) == target_local {
                return Some(v.as_str());
            }
        }
        None
    }

    fn children_elements(&self) -> impl Iterator<Item = &XmlNode> {
        self.children.iter().filter_map(|c| match c {
            XmlChild::Element(e) => Some(e),
            XmlChild::Text(_) => None,
        })
    }

    fn child_by_local(&self, local: &str) -> Option<&XmlNode> {
        self.children_elements().find(|c| c.local_name() == local)
    }

    fn text_content(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out);
        out
    }

    fn collect_text(&self, out: &mut String) {
        for c in &self.children {
            match c {
                XmlChild::Text(t) => out.push_str(t),
                XmlChild::Element(e) => e.collect_text(out),
            }
        }
    }
}

fn find_node_by_local<'a>(node: &'a XmlNode, local: &str) -> Option<&'a XmlNode> {
    if node.local_name() == local {
        return Some(node);
    }
    for c in node.children_elements() {
        if let Some(found) = find_node_by_local(c, local) {
            return Some(found);
        }
    }
    None
}

fn local_name(qname: &str) -> &str {
    qname.rsplit_once(':').map_or(qname, |(_, l)| l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_past_the_depth_limit_do_not_close_their_ancestors() {
        let deep = "<g>".repeat(MAX_XML_DEPTH + 20) + &"</g>".repeat(MAX_XML_DEPTH + 20);
        let xml = format!("<root><a>{deep}</a><b>after</b></root>");
        let root = XmlNode::parse(&xml);
        // `b` must stay a child of `root`, not be pushed out by unmatched end tags.
        let names: Vec<&str> = root.children_elements().map(XmlNode::local_name).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(root.child_by_local("b").unwrap().text_content(), "after");
    }
}
