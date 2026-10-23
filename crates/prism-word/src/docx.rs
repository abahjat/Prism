// SPDX-License-Identifier: AGPL-3.0-only
//! `WordprocessingML` (`.docx`) package reader producing a [`WordDocument`].

use crate::inheritance::resolve_word_color;
use crate::model::{
    AbstractNum, AnchorPlacement, CellMargins, ColumnDef, DrawingPayload, ExternalReference,
    FieldKind, FontSlots, ForensicFacts, HeaderFooterKind, LevelDef, LevelOverride, LevelSuffix,
    NumInstance, NumberFormat, NumberingRef, PageVerticalAlign, ParagraphContent,
    ParagraphProperties, RevisionKind, RunItem, RunProperties, SectionBreakKind, SectionProperties,
    StyleType, TabLeader, TabStop, TabStopKind, TableLayoutMode, TableProperties,
    TableStyleConditional, TableStyleRegion, TableWidth, ToggleProp, UnderlineKind, VerticalMerge,
    VerticalRunAlign, WordAlignment, WordBlock, WordBorder, WordBorderStyle, WordBorders,
    WordComment, WordDocument, WordDrawing, WordField, WordHyperlink, WordLineSpacing,
    WordNumbering, WordParagraph, WordRevisionMeta, WordRun, WordSection, WordSettings, WordStory,
    WordStyle, WordStyleSheet, WordTable, WordTableCell, WordTableRow, WordTheme, WrapMode,
};
use prism_core::document::{FontResource, ImageResource};
use prism_core::error::{Error, Result};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use zip::ZipArchive;

/// Maximum XML element nesting depth to guard against deeply nested XML `DoS`.
const MAX_XML_DEPTH: usize = 128;

/// Maximum nested table depth (`w:tbl` inside `w:tc`).
const MAX_TABLE_DEPTH: usize = 16;

/// English Metric Units per typographical point (`914,400 / 72 = 12,700`).
const EMU_PER_POINT: f32 = 12_700.0;

/// Twips (twentieths of a point) per typographical point.
const TWIPS_PER_POINT: f32 = 20.0;

/// Default maximum decompressed bytes per ZIP entry (256 MiB).
pub const DEFAULT_MAX_ENTRY_BYTES: usize = 256 * 1024 * 1024;

/// Crop rectangle `(left, top, right, bottom)` in normalized fractions `[0.0, 1.0]`.
type CropRect = (f32, f32, f32, f32);

/// Read a stream up to `limit` bytes, returning [`Error::MemoryLimitExceeded`] if exceeded.
///
/// # Errors
/// Returns an error on I/O failure or if the stream exceeds `limit` bytes.
pub fn read_to_end_with_limit<R: Read>(
    reader: R,
    buf: &mut Vec<u8>,
    limit: usize,
) -> Result<usize> {
    let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader
        .take(limit_u64)
        .read_to_end(buf)
        .map_err(|e| Error::ParseError(format!("Failed to read DOCX stream: {e}")))?;
    if buf.len() > limit {
        return Err(Error::MemoryLimitExceeded {
            limit,
            used: buf.len(),
        });
    }
    Ok(n)
}

/// Read a UTF-8 XML stream up to `limit` bytes, returning [`Error::MemoryLimitExceeded`] if exceeded.
///
/// # Errors
/// Returns an error on I/O failure or if the stream exceeds `limit` bytes.
pub fn read_to_string_with_limit<R: Read>(
    reader: R,
    buf: &mut String,
    limit: usize,
) -> Result<usize> {
    let mut raw = Vec::new();
    let n = read_to_end_with_limit(reader, &mut raw, limit)?;
    let s = String::from_utf8_lossy(&raw);
    buf.push_str(&s);
    Ok(n)
}

/// Check if a byte slice starts with a ZIP archive containing `word/document.xml`.
#[must_use]
pub fn is_docx_zip(data: &[u8]) -> bool {
    if data.len() < 4 || &data[0..2] != b"PK" {
        return false;
    }
    let cursor = Cursor::new(data);
    if let Ok(mut archive) = ZipArchive::new(cursor) {
        for i in 0..archive.len() {
            if let Ok(file) = archive.by_index(i)
                && file.name() == "word/document.xml"
            {
                return true;
            }
        }
    }
    false
}

/// Lightweight bounded XML DOM node used internally to parse `WordprocessingML` parts.
#[derive(Debug, Clone)]
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
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();

        let mut stack: Vec<Self> = vec![Self {
            name: "__root__".to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
        }];

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    if stack.len() >= MAX_XML_DEPTH {
                        // Skip deeply nested subtree safely
                        let end_name = e.name().as_ref().to_owned();
                        let _ = reader
                            .read_to_end_into(quick_xml::name::QName(&end_name), &mut Vec::new());
                    } else {
                        stack.push(Self::from_start(&e));
                    }
                }
                Ok(Event::Empty(e)) => {
                    if stack.len() < MAX_XML_DEPTH {
                        let node = Self::from_start(&e);
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(XmlChild::Element(node));
                        }
                    }
                }
                Ok(Event::End(_)) => {
                    if stack.len() > 1
                        && let Some(node) = stack.pop()
                        && let Some(parent) = stack.last_mut()
                    {
                        parent.children.push(XmlChild::Element(node));
                    }
                }
                Ok(Event::Text(e)) => {
                    let text = quick_xml::escape::unescape(e.as_ref())
                        .map_or_else(|_| e.as_ref().to_owned(), std::borrow::Cow::into_owned);
                    if !text.is_empty()
                        && let Some(parent) = stack.last_mut()
                    {
                        parent.children.push(XmlChild::Text(text));
                    }
                }
                Ok(Event::CData(e)) => {
                    let text = e.as_ref().to_owned();
                    if !text.is_empty()
                        && let Some(parent) = stack.last_mut()
                    {
                        parent.children.push(XmlChild::Text(text));
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
            buf.clear();
        }

        while stack.len() > 1 {
            if let Some(node) = stack.pop()
                && let Some(parent) = stack.last_mut()
            {
                parent.children.push(XmlChild::Element(node));
            }
        }

        stack.pop().unwrap_or(Self {
            name: "__root__".to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
        })
    }

    fn from_start(e: &BytesStart<'_>) -> Self {
        let name = e.name().as_ref().to_string();
        let mut attrs = Vec::new();
        for attr in e.attributes().flatten() {
            let k = attr.key.as_ref().to_string();
            let raw = attr.value.as_ref();
            let v = quick_xml::escape::unescape(raw)
                .map_or_else(|_| raw.to_owned(), std::borrow::Cow::into_owned);
            attrs.push((k, v));
        }
        Self {
            name,
            attrs,
            children: Vec::new(),
        }
    }

    fn local_name(&self) -> &str {
        self.name
            .rsplit_once(':')
            .map_or(self.name.as_str(), |(_, local)| local)
    }

    fn attr(&self, key: &str) -> Option<&str> {
        for (k, v) in &self.attrs {
            if k == key {
                return Some(v.as_str());
            }
        }
        let target_local = key.rsplit_once(':').map_or(key, |(_, l)| l);
        for (k, v) in &self.attrs {
            let k_local = k.rsplit_once(':').map_or(k.as_str(), |(_, l)| l);
            if k_local == target_local {
                return Some(v.as_str());
            }
        }
        None
    }

    fn children_elements(&self) -> impl Iterator<Item = &Self> {
        self.children.iter().filter_map(|c| match c {
            XmlChild::Element(el) => Some(el),
            XmlChild::Text(_) => None,
        })
    }

    fn child_by_local(&self, local: &str) -> Option<&Self> {
        self.children_elements()
            .find(|c| c.local_name() == local || c.name == local)
    }

    fn text_content(&self) -> String {
        let mut out = String::new();
        for c in &self.children {
            match c {
                XmlChild::Text(t) => out.push_str(t),
                XmlChild::Element(el) => out.push_str(&el.text_content()),
            }
        }
        out
    }
}

/// Relationship entry in a `.rels` file.
#[derive(Debug, Clone)]
struct RelEntry {
    id: String,
    rel_type: String,
    target: String,
    external: bool,
}

fn parse_relationships(xml: &str) -> BTreeMap<String, RelEntry> {
    let mut map = BTreeMap::new();
    let root = XmlNode::parse(xml);
    for node in root.children_elements() {
        for rel in node.children_elements() {
            if rel.local_name() == "Relationship"
                && let (Some(id), Some(target)) = (rel.attr("Id"), rel.attr("Target"))
            {
                let rel_type = rel.attr("Type").unwrap_or("").to_string();
                let external = rel
                    .attr("TargetMode")
                    .is_some_and(|m| m.eq_ignore_ascii_case("External"));
                map.insert(
                    id.to_string(),
                    RelEntry {
                        id: id.to_string(),
                        rel_type,
                        target: target.to_string(),
                        external,
                    },
                );
            }
        }
    }
    map
}

/// Parse a `.docx` byte buffer into a [`WordDocument`].
///
/// # Errors
/// Returns an error if the ZIP archive is invalid, `word/document.xml` is missing,
/// or any decompressed ZIP entry exceeds `max_entry_bytes`.
#[allow(clippy::too_many_lines)]
pub fn parse_docx(
    data: &[u8],
    filename: Option<&str>,
    max_entry_bytes: usize,
) -> Result<WordDocument> {
    let cursor = Cursor::new(data);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|e| Error::ParseError(format!("Failed to open DOCX ZIP: {e}")))?;

    let mut doc = WordDocument::default();
    if let Some(name) = filename {
        doc.title = Some(name.to_string());
    }

    // Check for VBA project
    doc.forensic.vba_project_present = archive.by_name("word/vbaProject.bin").is_ok();

    // 1. Parse core and custom properties
    if let Some(xml) = read_zip_string(&mut archive, "docProps/core.xml", max_entry_bytes)? {
        parse_core_props(&xml, &mut doc);
    }
    if let Some(xml) = read_zip_string(&mut archive, "docProps/custom.xml", max_entry_bytes)? {
        parse_custom_props(&xml, &mut doc.forensic);
    }

    // 2. Parse document relationships
    let doc_rels = if let Some(xml) = read_zip_string(
        &mut archive,
        "word/_rels/document.xml.rels",
        max_entry_bytes,
    )? {
        parse_relationships(&xml)
    } else {
        BTreeMap::new()
    };
    for rel in doc_rels.values() {
        if rel.external {
            doc.forensic.external_relationships.push(ExternalReference {
                id: rel.id.clone(),
                ref_type: short_rel_type(&rel.rel_type),
                target: rel.target.clone(),
            });
        }
    }

    // 3. Parse theme (`word/theme/theme1.xml`)
    if let Some(xml) = read_zip_string(&mut archive, "word/theme/theme1.xml", max_entry_bytes)? {
        doc.theme = parse_theme_xml(&xml);
    }

    // 4. Parse settings (`word/settings.xml` + `word/_rels/settings.xml.rels`)
    let settings_rels = if let Some(xml) = read_zip_string(
        &mut archive,
        "word/_rels/settings.xml.rels",
        max_entry_bytes,
    )? {
        parse_relationships(&xml)
    } else {
        BTreeMap::new()
    };
    if let Some(xml) = read_zip_string(&mut archive, "word/settings.xml", max_entry_bytes)? {
        parse_settings_xml(&xml, &settings_rels, &mut doc.settings, &mut doc.forensic);
    }

    // 5. Parse styles (`word/styles.xml`)
    if let Some(xml) = read_zip_string(&mut archive, "word/styles.xml", max_entry_bytes)? {
        doc.styles = parse_styles_xml(&xml, &doc.theme);
    }

    // 6. Parse numbering (`word/numbering.xml`)
    if let Some(xml) = read_zip_string(&mut archive, "word/numbering.xml", max_entry_bytes)? {
        doc.numbering = parse_numbering_xml(&xml, &doc.theme);
    }

    // 7. Extract embedded fonts (`word/fontTable.xml` + `word/_rels/fontTable.xml.rels`)
    doc.fonts = extract_embedded_fonts(&mut archive, max_entry_bytes);

    // 8. Parse main document (`word/document.xml`)
    let document_xml = read_zip_string(&mut archive, "word/document.xml", max_entry_bytes)?
        .ok_or_else(|| Error::ParseError("word/document.xml not found".to_string()))?;

    let mut image_map: BTreeMap<String, ImageResource> = BTreeMap::new();
    let mut rev_authors = BTreeSet::new();

    parse_document_body(
        &document_xml,
        &doc_rels,
        &mut archive,
        max_entry_bytes,
        &mut doc,
        &mut image_map,
        &mut rev_authors,
    )?;

    // 9. Parse headers & footers referenced in sections
    let mut header_rids = BTreeSet::new();
    let mut footer_rids = BTreeSet::new();
    for sec in &doc.sections {
        for rid in sec.props.headers.values() {
            header_rids.insert(rid.clone());
        }
        for rid in sec.props.footers.values() {
            footer_rids.insert(rid.clone());
        }
    }

    for rid in header_rids {
        if let Some(rel) = doc_rels.get(&rid)
            && !rel.external
        {
            let zip_path = resolve_word_zip_path(&rel.target);
            let part_rels = read_part_rels(&mut archive, &zip_path, max_entry_bytes)?;
            let active_rels = if part_rels.is_empty() {
                &doc_rels
            } else {
                &part_rels
            };
            if let Some(xml) = read_zip_string(&mut archive, &zip_path, max_entry_bytes)? {
                let blocks = parse_story_blocks(
                    &xml,
                    active_rels,
                    &mut archive,
                    max_entry_bytes,
                    &doc.theme,
                    &mut doc.forensic,
                    &mut image_map,
                    &mut rev_authors,
                )?;
                doc.headers
                    .insert(rid.clone(), WordStory { id: rid, blocks });
            }
        }
    }

    for rid in footer_rids {
        if let Some(rel) = doc_rels.get(&rid)
            && !rel.external
        {
            let zip_path = resolve_word_zip_path(&rel.target);
            let part_rels = read_part_rels(&mut archive, &zip_path, max_entry_bytes)?;
            let active_rels = if part_rels.is_empty() {
                &doc_rels
            } else {
                &part_rels
            };
            if let Some(xml) = read_zip_string(&mut archive, &zip_path, max_entry_bytes)? {
                let blocks = parse_story_blocks(
                    &xml,
                    active_rels,
                    &mut archive,
                    max_entry_bytes,
                    &doc.theme,
                    &mut doc.forensic,
                    &mut image_map,
                    &mut rev_authors,
                )?;
                doc.footers
                    .insert(rid.clone(), WordStory { id: rid, blocks });
            }
        }
    }

    // 10. Parse footnotes (`word/footnotes.xml`) and endnotes (`word/endnotes.xml`)
    if let Some(xml) = read_zip_string(&mut archive, "word/footnotes.xml", max_entry_bytes)? {
        doc.footnotes = parse_notes_xml(
            &xml,
            "footnote",
            &doc_rels,
            &mut archive,
            max_entry_bytes,
            &doc.theme,
            &mut doc.forensic,
            &mut image_map,
            &mut rev_authors,
        )?;
    }
    if let Some(xml) = read_zip_string(&mut archive, "word/endnotes.xml", max_entry_bytes)? {
        doc.endnotes = parse_notes_xml(
            &xml,
            "endnote",
            &doc_rels,
            &mut archive,
            max_entry_bytes,
            &doc.theme,
            &mut doc.forensic,
            &mut image_map,
            &mut rev_authors,
        )?;
    }

    // 11. Parse comments (`word/comments.xml`)
    if let Some(xml) = read_zip_string(&mut archive, "word/comments.xml", max_entry_bytes)? {
        doc.comments = parse_comments_xml(
            &xml,
            &doc_rels,
            &mut archive,
            max_entry_bytes,
            &doc.theme,
            &mut doc.forensic,
            &mut image_map,
            &mut rev_authors,
        )?;
    }

    doc.forensic.revision_authors = rev_authors.into_iter().collect();
    doc.images = image_map.into_values().collect();

    Ok(doc)
}

fn read_zip_string(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
    limit: usize,
) -> Result<Option<String>> {
    let Ok(mut file) = archive.by_name(path) else {
        return Ok(None);
    };
    let mut xml = String::new();
    read_to_string_with_limit(&mut file, &mut xml, limit)?;
    Ok(Some(xml))
}

fn read_part_rels(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    part_zip_path: &str,
    limit: usize,
) -> Result<BTreeMap<String, RelEntry>> {
    let (dir, base) = part_zip_path
        .rsplit_once('/')
        .unwrap_or(("", part_zip_path));
    let rels_path = if dir.is_empty() {
        format!("_rels/{base}.rels")
    } else {
        format!("{dir}/_rels/{base}.rels")
    };
    if let Some(xml) = read_zip_string(archive, &rels_path, limit)? {
        Ok(parse_relationships(&xml))
    } else {
        Ok(BTreeMap::new())
    }
}

fn resolve_word_zip_path(target: &str) -> String {
    let trimmed = target.trim_start_matches('/');
    if trimmed.starts_with("word/") {
        trimmed.to_string()
    } else if let Some(stripped) = trimmed.strip_prefix("../") {
        stripped.to_string()
    } else {
        format!("word/{trimmed}")
    }
}

fn short_rel_type(uri: &str) -> String {
    uri.rsplit_once('/')
        .map_or(uri, |(_, tail)| tail)
        .to_string()
}

fn parse_core_props(xml: &str, doc: &mut WordDocument) {
    let root = XmlNode::parse(xml);
    for cp in root.children_elements() {
        for child in cp.children_elements() {
            let text = child.text_content().trim().to_string();
            if text.is_empty() {
                continue;
            }
            match child.local_name() {
                "title" => {
                    doc.title = Some(text.clone());
                    doc.properties.insert("title".to_string(), text);
                }
                "creator" => {
                    doc.author = Some(text.clone());
                    doc.properties.insert("author".to_string(), text);
                }
                other => {
                    doc.properties.insert(other.to_string(), text);
                }
            }
        }
    }
}

fn parse_custom_props(xml: &str, forensic: &mut ForensicFacts) {
    let root = XmlNode::parse(xml);
    for props in root.children_elements() {
        for prop in props.children_elements() {
            if prop.local_name() == "property"
                && let Some(name) = prop.attr("name")
            {
                let val = prop.text_content().trim().to_string();
                forensic.custom_properties.insert(name.to_string(), val);
            }
        }
    }
}

fn parse_theme_xml(xml: &str) -> WordTheme {
    let mut theme = WordTheme::default();
    let root = XmlNode::parse(xml);
    let Some(theme_el) = root.child_by_local("theme") else {
        return theme;
    };
    if let Some(name) = theme_el.attr("name") {
        theme.name = name.to_string();
    }
    let Some(elements) = theme_el.child_by_local("themeElements") else {
        return theme;
    };
    if let Some(clr_scheme) = elements.child_by_local("clrScheme") {
        for slot_el in clr_scheme.children_elements() {
            let slot_name = slot_el.local_name().to_string();
            for clr_child in slot_el.children_elements() {
                let hex = match clr_child.local_name() {
                    "srgbClr" => clr_child.attr("val").map(str::to_string),
                    "sysClr" => clr_child
                        .attr("lastClr")
                        .or_else(|| clr_child.attr("val"))
                        .map(str::to_string),
                    _ => None,
                };
                if let Some(h) = hex {
                    theme.colors.insert(slot_name.clone(), h);
                }
            }
        }
    }
    if let Some(font_scheme) = elements.child_by_local("fontScheme") {
        if let Some(major) = font_scheme.child_by_local("majorFont") {
            theme.major_latin = major
                .child_by_local("latin")
                .and_then(|n| n.attr("typeface"))
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            theme.major_east_asia = major
                .child_by_local("ea")
                .and_then(|n| n.attr("typeface"))
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            theme.major_cs = major
                .child_by_local("cs")
                .and_then(|n| n.attr("typeface"))
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if let Some(minor) = font_scheme.child_by_local("minorFont") {
            theme.minor_latin = minor
                .child_by_local("latin")
                .and_then(|n| n.attr("typeface"))
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            theme.minor_east_asia = minor
                .child_by_local("ea")
                .and_then(|n| n.attr("typeface"))
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            theme.minor_cs = minor
                .child_by_local("cs")
                .and_then(|n| n.attr("typeface"))
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
    }
    theme
}

fn parse_settings_xml(
    xml: &str,
    settings_rels: &BTreeMap<String, RelEntry>,
    settings: &mut WordSettings,
    forensic: &mut ForensicFacts,
) {
    let root = XmlNode::parse(xml);
    let Some(settings_el) = root.child_by_local("settings") else {
        return;
    };
    for child in settings_el.children_elements() {
        match child.local_name() {
            "defaultTabStop" => {
                if let Some(twips) = child.attr("w:val").and_then(|v| v.parse::<f32>().ok()) {
                    settings.default_tab_stop_pt = (twips / TWIPS_PER_POINT).max(0.0);
                }
            }
            "evenAndOddHeaders" => {
                settings.even_and_odd_headers = parse_bool_attr(child);
            }
            "mirrorMargins" => {
                settings.mirror_margins = parse_bool_attr(child);
            }
            "autoHyphenation" => {
                settings.auto_hyphenation = parse_bool_attr(child);
            }
            "trackRevisions" => {
                settings.track_revisions = parse_bool_attr(child);
            }
            "attachedTemplate" => {
                if let Some(rid) = child.attr("r:id")
                    && let Some(rel) = settings_rels.get(rid)
                {
                    forensic.attached_template = Some(rel.target.clone());
                    forensic.external_relationships.push(ExternalReference {
                        id: rid.to_string(),
                        ref_type: "attachedTemplate".to_string(),
                        target: rel.target.clone(),
                    });
                }
            }
            "docVars" => {
                for var in child.children_elements() {
                    if var.local_name() == "docVar"
                        && let (Some(k), Some(v)) = (var.attr("w:name"), var.attr("w:val"))
                    {
                        forensic
                            .document_variables
                            .insert(k.to_string(), v.to_string());
                    }
                }
            }
            "compat" => {
                for cs in child.children_elements() {
                    let local = cs.local_name();
                    if local == "compatSetting" {
                        if let Some(name) = cs.attr("w:name") {
                            let val = cs.attr("w:val").unwrap_or("1");
                            if name == "compatibilityMode" {
                                if let Ok(mode) = val.parse::<u16>() {
                                    settings.compatibility_mode = mode;
                                }
                            } else {
                                settings
                                    .compat_flags
                                    .insert(name.to_string(), !matches!(val, "0" | "false"));
                            }
                        }
                    } else {
                        settings
                            .compat_flags
                            .insert(local.to_string(), parse_bool_attr(cs));
                    }
                }
            }
            _ => {}
        }
    }
}

fn parse_styles_xml(xml: &str, theme: &WordTheme) -> WordStyleSheet {
    let mut sheet = WordStyleSheet::default();
    let root = XmlNode::parse(xml);
    let Some(styles_el) = root.child_by_local("styles") else {
        return sheet;
    };

    for child in styles_el.children_elements() {
        match child.local_name() {
            "docDefaults" => {
                if let Some(rpr_def) = child.child_by_local("rPrDefault")
                    && let Some(rpr) = rpr_def.child_by_local("rPr")
                {
                    sheet.default_run_props = parse_run_properties(rpr, theme);
                }
                if let Some(ppr_def) = child.child_by_local("pPrDefault")
                    && let Some(ppr) = ppr_def.child_by_local("pPr")
                {
                    sheet.default_para_props = parse_paragraph_properties(ppr, theme);
                }
            }
            "style" => {
                let Some(id) = child.attr("w:styleId") else {
                    continue;
                };
                let style_type = match child.attr("w:type").unwrap_or("paragraph") {
                    "character" => StyleType::Character,
                    "table" => StyleType::Table,
                    "numbering" => StyleType::Numbering,
                    _ => StyleType::Paragraph,
                };
                let is_default = child
                    .attr("w:default")
                    .is_some_and(|v| matches!(v, "1" | "true" | "on"));

                let mut style = WordStyle {
                    id: id.to_string(),
                    name: None,
                    style_type,
                    is_default,
                    based_on: None,
                    next: None,
                    link: None,
                    para_props: ParagraphProperties::default(),
                    run_props: RunProperties::default(),
                    table_borders: WordBorders::default(),
                    table_conditionals: BTreeMap::new(),
                };

                for sc in child.children_elements() {
                    match sc.local_name() {
                        "name" => style.name = sc.attr("w:val").map(str::to_string),
                        "basedOn" => style.based_on = sc.attr("w:val").map(str::to_string),
                        "next" => style.next = sc.attr("w:val").map(str::to_string),
                        "link" => style.link = sc.attr("w:val").map(str::to_string),
                        "pPr" => style.para_props = parse_paragraph_properties(sc, theme),
                        "rPr" => style.run_props = parse_run_properties(sc, theme),
                        "tblPr" => {
                            if let Some(bdr) = sc.child_by_local("tblBorders") {
                                style.table_borders = parse_borders(bdr, theme);
                            }
                        }
                        "tblStylePr" => {
                            if let Some(region) = sc.attr("w:type").and_then(parse_table_region) {
                                let mut cond = TableStyleConditional::default();
                                if let Some(ppr) = sc.child_by_local("pPr") {
                                    cond.para_props = parse_paragraph_properties(ppr, theme);
                                }
                                if let Some(rpr) = sc.child_by_local("rPr") {
                                    cond.run_props = parse_run_properties(rpr, theme);
                                }
                                if let Some(tc_pr) = sc.child_by_local("tcPr") {
                                    if let Some(bdr) = tc_pr.child_by_local("tcBorders") {
                                        cond.borders = parse_borders(bdr, theme);
                                    }
                                    if let Some(shd) = tc_pr.child_by_local("shd") {
                                        cond.shading_fill = parse_shading_fill(shd, theme);
                                    }
                                }
                                style.table_conditionals.insert(region, cond);
                            }
                        }
                        _ => {}
                    }
                }
                sheet.styles.insert(style.id.clone(), style);
            }
            _ => {}
        }
    }
    sheet
}

fn parse_table_region(s: &str) -> Option<TableStyleRegion> {
    match s {
        "wholeTable" => Some(TableStyleRegion::WholeTable),
        "band1Vert" => Some(TableStyleRegion::Band1Vert),
        "band2Vert" => Some(TableStyleRegion::Band2Vert),
        "band1Horz" => Some(TableStyleRegion::Band1Horz),
        "band2Horz" => Some(TableStyleRegion::Band2Horz),
        "firstRow" => Some(TableStyleRegion::FirstRow),
        "lastRow" => Some(TableStyleRegion::LastRow),
        "firstCol" => Some(TableStyleRegion::FirstCol),
        "lastCol" => Some(TableStyleRegion::LastCol),
        "nwCell" => Some(TableStyleRegion::NwCell),
        "neCell" => Some(TableStyleRegion::NeCell),
        "swCell" => Some(TableStyleRegion::SwCell),
        "seCell" => Some(TableStyleRegion::SeCell),
        _ => None,
    }
}

fn parse_numbering_xml(xml: &str, theme: &WordTheme) -> WordNumbering {
    let mut numbering = WordNumbering::default();
    let root = XmlNode::parse(xml);
    let Some(num_el) = root.child_by_local("numbering") else {
        return numbering;
    };

    for child in num_el.children_elements() {
        match child.local_name() {
            "abstractNum" => {
                let Some(id) = child
                    .attr("w:abstractNumId")
                    .and_then(|v| v.parse::<u32>().ok())
                else {
                    continue;
                };
                let mut abs = AbstractNum {
                    id,
                    num_style_link: None,
                    levels: BTreeMap::new(),
                };
                for sub in child.children_elements() {
                    match sub.local_name() {
                        "numStyleLink" => {
                            abs.num_style_link = sub.attr("w:val").map(str::to_string);
                        }
                        "lvl" => {
                            if let Some(level) = parse_level_def(sub, theme) {
                                abs.levels.insert(level.ilvl, level);
                            }
                        }
                        _ => {}
                    }
                }
                numbering.abstract_nums.insert(id, abs);
            }
            "num" => {
                let Some(num_id) = child.attr("w:numId").and_then(|v| v.parse::<u32>().ok()) else {
                    continue;
                };
                let Some(abstract_num_id) = child
                    .child_by_local("abstractNumId")
                    .and_then(|n| n.attr("w:val"))
                    .and_then(|v| v.parse::<u32>().ok())
                else {
                    continue;
                };
                let mut overrides = BTreeMap::new();
                for sub in child.children_elements() {
                    if sub.local_name() == "lvlOverride"
                        && let Some(ilvl) = sub.attr("w:ilvl").and_then(|v| v.parse::<u8>().ok())
                    {
                        let start_override = sub
                            .child_by_local("startOverride")
                            .and_then(|n| n.attr("w:val"))
                            .and_then(|v| v.parse::<i32>().ok());
                        let level_def = sub
                            .child_by_local("lvl")
                            .and_then(|l| parse_level_def(l, theme));
                        overrides.insert(
                            ilvl,
                            LevelOverride {
                                ilvl,
                                start_override,
                                level_def,
                            },
                        );
                    }
                }
                numbering.nums.insert(
                    num_id,
                    NumInstance {
                        num_id,
                        abstract_num_id,
                        overrides,
                    },
                );
            }
            _ => {}
        }
    }
    numbering
}

fn parse_level_def(node: &XmlNode, theme: &WordTheme) -> Option<LevelDef> {
    let ilvl = node.attr("w:ilvl")?.parse::<u8>().ok()?;
    let mut level = LevelDef {
        ilvl,
        start: 1,
        num_fmt: NumberFormat::Decimal,
        lvl_text: String::new(),
        alignment: WordAlignment::Left,
        suffix: LevelSuffix::Tab,
        is_legal: false,
        restart_after_level: None,
        para_style_id: None,
        para_props: ParagraphProperties::default(),
        run_props: RunProperties::default(),
    };
    for c in node.children_elements() {
        match c.local_name() {
            "start" => {
                if let Some(v) = c.attr("w:val").and_then(|v| v.parse::<i32>().ok()) {
                    level.start = v;
                }
            }
            "numFmt" => {
                if let Some(v) = c.attr("w:val") {
                    level.num_fmt = parse_num_format(v);
                }
            }
            "lvlText" => {
                level.lvl_text = c.attr("w:val").unwrap_or("").to_string();
            }
            "lvlJc" => {
                if let Some(v) = c.attr("w:val") {
                    level.alignment = parse_alignment(v);
                }
            }
            "suff" => {
                level.suffix = match c.attr("w:val").unwrap_or("tab") {
                    "space" => LevelSuffix::Space,
                    "nothing" => LevelSuffix::Nothing,
                    _ => LevelSuffix::Tab,
                };
            }
            "isLgl" => level.is_legal = parse_bool_attr(c),
            "lvlRestart" => {
                level.restart_after_level = c
                    .attr("w:val")
                    .and_then(|v| v.parse::<u8>().ok())
                    .map(|v| v.saturating_sub(1));
            }
            "pStyle" => level.para_style_id = c.attr("w:val").map(str::to_string),
            "pPr" => level.para_props = parse_paragraph_properties(c, theme),
            "rPr" => level.run_props = parse_run_properties(c, theme),
            _ => {}
        }
    }
    Some(level)
}

fn parse_num_format(s: &str) -> NumberFormat {
    match s {
        "decimal" => NumberFormat::Decimal,
        "decimalZero" => NumberFormat::DecimalZero,
        "upperRoman" => NumberFormat::UpperRoman,
        "lowerRoman" => NumberFormat::LowerRoman,
        "upperLetter" => NumberFormat::UpperLetter,
        "lowerLetter" => NumberFormat::LowerLetter,
        "ordinal" => NumberFormat::Ordinal,
        "bullet" => NumberFormat::Bullet,
        "none" => NumberFormat::None,
        other => NumberFormat::Custom(other.to_string()),
    }
}

fn parse_run_properties(rpr: &XmlNode, theme: &WordTheme) -> RunProperties {
    let mut props = RunProperties::default();
    for c in rpr.children_elements() {
        match c.local_name() {
            "rStyle" => props.style_id = c.attr("w:val").map(str::to_string),
            "rFonts" => {
                props.fonts = FontSlots {
                    ascii: c.attr("w:ascii").map(str::to_string),
                    h_ansi: c.attr("w:hAnsi").map(str::to_string),
                    east_asia: c.attr("w:eastAsia").map(str::to_string),
                    cs: c.attr("w:cs").map(str::to_string),
                    ascii_theme: c.attr("w:asciiTheme").map(str::to_string),
                    h_ansi_theme: c.attr("w:hAnsiTheme").map(str::to_string),
                    east_asia_theme: c.attr("w:eastAsiaTheme").map(str::to_string),
                    cs_theme: c.attr("w:cstheme").map(str::to_string),
                    hint: c.attr("w:hint").map(str::to_string),
                };
            }
            "b" => props.bold = Some(parse_toggle_prop(c)),
            "bCs" => props.bold_cs = Some(parse_toggle_prop(c)),
            "i" => props.italic = Some(parse_toggle_prop(c)),
            "iCs" => props.italic_cs = Some(parse_toggle_prop(c)),
            "caps" => props.caps = Some(parse_toggle_prop(c)),
            "smallCaps" => props.small_caps = Some(parse_toggle_prop(c)),
            "strike" => props.strike = Some(parse_toggle_prop(c)),
            "dstrike" => props.double_strike = Some(parse_toggle_prop(c)),
            "vanish" | "specVanish" => props.vanish = Some(parse_toggle_prop(c)),
            "u" => {
                let val = c.attr("w:val").unwrap_or("single");
                props.underline = Some(match val {
                    "none" | "0" | "false" | "off" => UnderlineKind::None,
                    "double" => UnderlineKind::Double,
                    "dotted" | "dottedHeavy" => UnderlineKind::Dotted,
                    "dash" | "dashedHeavy" | "dashLong" => UnderlineKind::Dashed,
                    "wave" | "wavyHeavy" | "wavyDouble" => UnderlineKind::Wave,
                    "words" => UnderlineKind::Words,
                    _ => UnderlineKind::Single,
                });
                props.underline_color = parse_color_attrs(c, theme);
            }
            "sz" => {
                if let Some(hp) = c.attr("w:val").and_then(|v| v.parse::<f32>().ok()) {
                    props.font_size_pt = Some((hp / 2.0).max(0.5));
                }
            }
            "szCs" => {
                if let Some(hp) = c.attr("w:val").and_then(|v| v.parse::<f32>().ok()) {
                    props.font_size_cs_pt = Some((hp / 2.0).max(0.5));
                }
            }
            "color" => {
                props.color = parse_color_attrs(c, theme);
            }
            "highlight" => {
                if let Some(hl) = c.attr("w:val").and_then(named_highlight_to_hex) {
                    props.highlight_color = Some(hl.to_string());
                }
            }
            "shd" => {
                if props.highlight_color.is_none() {
                    props.highlight_color = parse_shading_fill(c, theme);
                }
            }
            "spacing" => {
                if let Some(twips) = c.attr("w:val").and_then(|v| v.parse::<f32>().ok()) {
                    props.char_spacing_pt = Some(twips / TWIPS_PER_POINT);
                }
            }
            "position" => {
                if let Some(hp) = c.attr("w:val").and_then(|v| v.parse::<f32>().ok()) {
                    props.position_pt = Some(hp / 2.0);
                }
            }
            "vertAlign" => {
                props.vert_align = Some(match c.attr("w:val").unwrap_or("baseline") {
                    "superscript" => VerticalRunAlign::Superscript,
                    "subscript" => VerticalRunAlign::Subscript,
                    _ => VerticalRunAlign::Baseline,
                });
            }
            _ => {}
        }
    }
    props
}

#[allow(clippy::too_many_lines)]
fn parse_paragraph_properties(ppr: &XmlNode, theme: &WordTheme) -> ParagraphProperties {
    let mut props = ParagraphProperties::default();
    for c in ppr.children_elements() {
        match c.local_name() {
            "pStyle" => props.style_id = c.attr("w:val").map(str::to_string),
            "jc" => {
                if let Some(v) = c.attr("w:val") {
                    props.alignment = Some(parse_alignment(v));
                }
            }
            "spacing" => {
                if let Some(before) = c.attr("w:before").and_then(|v| v.parse::<f32>().ok()) {
                    props.space_before_pt = Some((before / TWIPS_PER_POINT).max(0.0));
                }
                if let Some(after) = c.attr("w:after").and_then(|v| v.parse::<f32>().ok()) {
                    props.space_after_pt = Some((after / TWIPS_PER_POINT).max(0.0));
                }
                if let Some(ba) = c.attr("w:beforeAutospacing") {
                    let auto = !matches!(ba, "0" | "false" | "off");
                    props.space_before_auto = Some(auto);
                    if auto && props.space_before_pt.is_none() {
                        props.space_before_pt = Some(14.0);
                    }
                }
                if let Some(aa) = c.attr("w:afterAutospacing") {
                    let auto = !matches!(aa, "0" | "false" | "off");
                    props.space_after_auto = Some(auto);
                    if auto && props.space_after_pt.is_none() {
                        props.space_after_pt = Some(14.0);
                    }
                }
                if let Some(line) = c.attr("w:line").and_then(|v| v.parse::<f32>().ok())
                    && line > 0.0
                {
                    let rule = c.attr("w:lineRule").unwrap_or("auto");
                    props.line_spacing = Some(match rule {
                        "exact" => WordLineSpacing::ExactPt(line / TWIPS_PER_POINT),
                        "atLeast" => WordLineSpacing::AtLeastPt(line / TWIPS_PER_POINT),
                        _ => WordLineSpacing::Auto(line / 240.0),
                    });
                }
            }
            "contextualSpacing" => props.contextual_spacing = Some(parse_bool_attr(c)),
            "ind" => {
                if let Some(left) = c
                    .attr("w:left")
                    .or_else(|| c.attr("w:start"))
                    .and_then(|v| v.parse::<f32>().ok())
                {
                    props.indent_left_pt = Some(left / TWIPS_PER_POINT);
                }
                if let Some(right) = c
                    .attr("w:right")
                    .or_else(|| c.attr("w:end"))
                    .and_then(|v| v.parse::<f32>().ok())
                {
                    props.indent_right_pt = Some(right / TWIPS_PER_POINT);
                }
                if let Some(hanging) = c.attr("w:hanging").and_then(|v| v.parse::<f32>().ok()) {
                    props.indent_first_line_pt = Some(-(hanging / TWIPS_PER_POINT));
                } else if let Some(first) =
                    c.attr("w:firstLine").and_then(|v| v.parse::<f32>().ok())
                {
                    props.indent_first_line_pt = Some(first / TWIPS_PER_POINT);
                }
            }
            "keepNext" => props.keep_next = Some(parse_bool_attr(c)),
            "keepLines" => props.keep_lines = Some(parse_bool_attr(c)),
            "pageBreakBefore" => props.page_break_before = Some(parse_bool_attr(c)),
            "widowControl" => props.widow_control = Some(parse_bool_attr(c)),
            "suppressAutoHyphens" => props.suppress_auto_hyphens = Some(parse_bool_attr(c)),
            "outlineLvl" => {
                if let Some(lvl) = c.attr("w:val").and_then(|v| v.parse::<u8>().ok()) {
                    props.outline_level = Some(lvl);
                }
            }
            "numPr" => {
                let num_id = c
                    .child_by_local("numId")
                    .and_then(|n| n.attr("w:val"))
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(0);
                let ilvl = c
                    .child_by_local("ilvl")
                    .and_then(|n| n.attr("w:val"))
                    .and_then(|v| v.parse::<u8>().ok())
                    .unwrap_or(0);
                if num_id > 0 {
                    props.numbering = Some(NumberingRef { num_id, ilvl });
                }
            }
            "tabs" => {
                for tab in c.children_elements() {
                    if tab.local_name() == "tab"
                        && let Some(pos) = tab.attr("w:pos").and_then(|v| v.parse::<f32>().ok())
                    {
                        let kind = match tab.attr("w:val").unwrap_or("left") {
                            "center" => TabStopKind::Center,
                            "right" | "end" => TabStopKind::Right,
                            "decimal" => TabStopKind::Decimal,
                            "bar" => TabStopKind::Bar,
                            "clear" => TabStopKind::Clear,
                            _ => TabStopKind::Left,
                        };
                        let leader = match tab.attr("w:leader").unwrap_or("none") {
                            "dot" | "middleDot" => TabLeader::Dot,
                            "hyphen" => TabLeader::Hyphen,
                            "underscore" | "heavy" => TabLeader::Underscore,
                            _ => TabLeader::None,
                        };
                        props.tab_stops.push(TabStop {
                            position_pt: pos / TWIPS_PER_POINT,
                            kind,
                            leader,
                        });
                    }
                }
            }
            "pBdr" => props.borders = parse_borders(c, theme),
            "shd" => props.shading_fill = parse_shading_fill(c, theme),
            "rPr" => props.mark_run_props = Some(parse_run_properties(c, theme)),
            "sectPr" => props.section_break = Some(Box::new(parse_section_properties(c))),
            _ => {}
        }
    }
    props
}

#[allow(clippy::too_many_lines)]
fn parse_section_properties(sect: &XmlNode) -> SectionProperties {
    let mut props = SectionProperties::default();
    for c in sect.children_elements() {
        match c.local_name() {
            "type" => {
                props.break_kind = match c.attr("w:val").unwrap_or("nextPage") {
                    "continuous" => SectionBreakKind::Continuous,
                    "evenPage" => SectionBreakKind::EvenPage,
                    "oddPage" => SectionBreakKind::OddPage,
                    _ => SectionBreakKind::NextPage,
                };
            }
            "pgSz" => {
                if let Some(w) = c.attr("w:w").and_then(|v| v.parse::<f32>().ok())
                    && w > 0.0
                {
                    props.page_width_pt = w / TWIPS_PER_POINT;
                }
                if let Some(h) = c.attr("w:h").and_then(|v| v.parse::<f32>().ok())
                    && h > 0.0
                {
                    props.page_height_pt = h / TWIPS_PER_POINT;
                }
                props.landscape = c
                    .attr("w:orient")
                    .is_some_and(|v| v.eq_ignore_ascii_case("landscape"));
            }
            "pgMar" => {
                if let Some(t) = c.attr("w:top").and_then(|v| v.parse::<f32>().ok()) {
                    props.margin_top_pt = t.abs() / TWIPS_PER_POINT;
                }
                if let Some(r) = c.attr("w:right").and_then(|v| v.parse::<f32>().ok()) {
                    props.margin_right_pt = (r / TWIPS_PER_POINT).max(0.0);
                }
                if let Some(b) = c.attr("w:bottom").and_then(|v| v.parse::<f32>().ok()) {
                    props.margin_bottom_pt = b.abs() / TWIPS_PER_POINT;
                }
                if let Some(l) = c.attr("w:left").and_then(|v| v.parse::<f32>().ok()) {
                    props.margin_left_pt = (l / TWIPS_PER_POINT).max(0.0);
                }
                if let Some(hdr) = c.attr("w:header").and_then(|v| v.parse::<f32>().ok()) {
                    props.header_margin_pt = (hdr / TWIPS_PER_POINT).max(0.0);
                }
                if let Some(ftr) = c.attr("w:footer").and_then(|v| v.parse::<f32>().ok()) {
                    props.footer_margin_pt = (ftr / TWIPS_PER_POINT).max(0.0);
                }
                if let Some(g) = c.attr("w:gutter").and_then(|v| v.parse::<f32>().ok()) {
                    props.gutter_pt = (g / TWIPS_PER_POINT).max(0.0);
                }
            }
            "cols" => {
                if let Some(num) = c.attr("w:num").and_then(|v| v.parse::<u16>().ok()) {
                    props.column_count = num.max(1);
                }
                if let Some(sp) = c.attr("w:space").and_then(|v| v.parse::<f32>().ok()) {
                    props.column_space_pt = (sp / TWIPS_PER_POINT).max(0.0);
                }
                props.column_separator = c
                    .attr("w:sep")
                    .is_some_and(|v| matches!(v, "1" | "true" | "on"));
                for col in c.children_elements() {
                    if col.local_name() == "col"
                        && let Some(w) = col.attr("w:w").and_then(|v| v.parse::<f32>().ok())
                    {
                        let sp = col
                            .attr("w:space")
                            .and_then(|v| v.parse::<f32>().ok())
                            .unwrap_or(0.0);
                        props.custom_columns.push(ColumnDef {
                            width_pt: (w / TWIPS_PER_POINT).max(1.0),
                            space_pt: (sp / TWIPS_PER_POINT).max(0.0),
                        });
                    }
                }
            }
            "titlePg" => props.title_pg = parse_bool_attr(c),
            "vAlign" => {
                props.vertical_align = match c.attr("w:val").unwrap_or("top") {
                    "center" => PageVerticalAlign::Center,
                    "both" => PageVerticalAlign::Both,
                    "bottom" => PageVerticalAlign::Bottom,
                    _ => PageVerticalAlign::Top,
                };
            }
            "pgNumType" => {
                if let Some(fmt) = c.attr("w:fmt") {
                    props.page_num_format = parse_num_format(fmt);
                }
                props.page_num_start = c.attr("w:start").and_then(|v| v.parse::<u32>().ok());
            }
            "headerReference" => {
                if let Some(rid) = c.attr("r:id") {
                    let kind = parse_hf_kind(c.attr("w:type"));
                    props.headers.insert(kind, rid.to_string());
                }
            }
            "footerReference" => {
                if let Some(rid) = c.attr("r:id") {
                    let kind = parse_hf_kind(c.attr("w:type"));
                    props.footers.insert(kind, rid.to_string());
                }
            }
            _ => {}
        }
    }
    props
}

fn parse_hf_kind(s: Option<&str>) -> HeaderFooterKind {
    match s.unwrap_or("default") {
        "first" => HeaderFooterKind::First,
        "even" => HeaderFooterKind::Even,
        _ => HeaderFooterKind::Default,
    }
}

fn parse_borders(node: &XmlNode, theme: &WordTheme) -> WordBorders {
    let mut borders = WordBorders::default();
    for c in node.children_elements() {
        let b = parse_single_border(c, theme);
        match c.local_name() {
            "top" => borders.top = Some(b),
            "right" | "end" => borders.right = Some(b),
            "bottom" => borders.bottom = Some(b),
            "left" | "start" => borders.left = Some(b),
            "between" => borders.between = Some(b),
            "insideH" => borders.inside_h = Some(b),
            "insideV" => borders.inside_v = Some(b),
            _ => {}
        }
    }
    borders
}

fn parse_single_border(node: &XmlNode, theme: &WordTheme) -> WordBorder {
    let style = match node.attr("w:val").unwrap_or("none") {
        "none" | "nil" => WordBorderStyle::None,
        "dotted" => WordBorderStyle::Dotted,
        "dashed" | "dashSmallGap" | "dotDash" | "dotDotDash" => WordBorderStyle::Dashed,
        "double" | "triple" => WordBorderStyle::Double,
        "thick" => WordBorderStyle::Thick,
        _ => WordBorderStyle::Single,
    };
    let width_pt = node
        .attr("w:sz")
        .and_then(|v| v.parse::<f32>().ok())
        .map_or(0.5, |eighths| (eighths / 8.0).clamp(0.25, 12.0));
    let color = parse_color_attrs(node, theme).unwrap_or_else(|| "#000000".to_string());
    let space_pt = node
        .attr("w:space")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0);
    WordBorder {
        style,
        width_pt,
        color,
        space_pt,
    }
}

fn parse_shading_fill(node: &XmlNode, theme: &WordTheme) -> Option<String> {
    let fill = node.attr("w:fill");
    let tc = node.attr("w:themeFill");
    let tint = node
        .attr("w:themeFillTint")
        .and_then(|v| u8::from_str_radix(v, 16).ok());
    let shade = node
        .attr("w:themeFillShade")
        .and_then(|v| u8::from_str_radix(v, 16).ok());
    resolve_word_color(fill, tc, tint, shade, theme)
}

fn parse_color_attrs(node: &XmlNode, theme: &WordTheme) -> Option<String> {
    let val = node.attr("w:val").or_else(|| node.attr("w:color"));
    let tc = node.attr("w:themeColor");
    let tint = node
        .attr("w:themeTint")
        .and_then(|v| u8::from_str_radix(v, 16).ok());
    let shade = node
        .attr("w:themeShade")
        .and_then(|v| u8::from_str_radix(v, 16).ok());
    resolve_word_color(val, tc, tint, shade, theme)
}

fn parse_toggle_prop(node: &XmlNode) -> ToggleProp {
    match node.attr("w:val") {
        Some("0" | "false" | "off" | "none") => ToggleProp::Off,
        _ => ToggleProp::Toggle,
    }
}

fn parse_bool_attr(node: &XmlNode) -> bool {
    !matches!(node.attr("w:val"), Some("0" | "false" | "off" | "none"))
}

fn parse_alignment(val: &str) -> WordAlignment {
    match val {
        "center" => WordAlignment::Center,
        "right" | "end" => WordAlignment::Right,
        "both" | "justify" => WordAlignment::Justify,
        "distribute" => WordAlignment::Distribute,
        _ => WordAlignment::Left,
    }
}

fn named_highlight_to_hex(name: &str) -> Option<&'static str> {
    match name {
        "yellow" => Some("#FFFF00"),
        "green" => Some("#00FF00"),
        "cyan" => Some("#00FFFF"),
        "magenta" => Some("#FF00FF"),
        "blue" => Some("#0000FF"),
        "red" => Some("#FF0000"),
        "darkBlue" => Some("#000080"),
        "darkCyan" => Some("#008080"),
        "darkGreen" => Some("#008000"),
        "darkMagenta" => Some("#800080"),
        "darkRed" => Some("#800000"),
        "darkYellow" => Some("#808000"),
        "darkGray" => Some("#808080"),
        "lightGray" => Some("#C0C0C0"),
        "black" => Some("#000000"),
        "white" => Some("#FFFFFF"),
        _ => None,
    }
}

struct StoryParseCtx<'a, 'b> {
    rels: &'a BTreeMap<String, RelEntry>,
    archive: &'a mut ZipArchive<Cursor<&'b [u8]>>,
    max_entry_bytes: usize,
    theme: &'a WordTheme,
    forensic: &'a mut ForensicFacts,
    image_map: &'a mut BTreeMap<String, ImageResource>,
    rev_authors: &'a mut BTreeSet<String>,
    table_depth: usize,
}

fn parse_document_body(
    document_xml: &str,
    rels: &BTreeMap<String, RelEntry>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_entry_bytes: usize,
    doc: &mut WordDocument,
    image_map: &mut BTreeMap<String, ImageResource>,
    rev_authors: &mut BTreeSet<String>,
) -> Result<()> {
    let root = XmlNode::parse(document_xml);
    let Some(doc_el) = root.child_by_local("document") else {
        return Ok(());
    };
    let Some(body_el) = doc_el.child_by_local("body") else {
        return Ok(());
    };

    let mut ctx = StoryParseCtx {
        rels,
        archive,
        max_entry_bytes,
        theme: &doc.theme,
        forensic: &mut doc.forensic,
        image_map,
        rev_authors,
        table_depth: 0,
    };

    let mut current_blocks: Vec<WordBlock> = Vec::new();
    let mut final_sect_props: Option<SectionProperties> = None;

    for child in body_el.children_elements() {
        match child.local_name() {
            "p" => {
                let mut para = parse_paragraph(child, &mut ctx, None)?;
                let sect_break = para.props.section_break.take();
                current_blocks.push(WordBlock::Paragraph(para));
                if let Some(sect_props) = sect_break {
                    doc.sections.push(WordSection {
                        props: *sect_props,
                        blocks: std::mem::take(&mut current_blocks),
                    });
                }
            }
            "tbl" => {
                if let Some(tbl) = parse_table(child, &mut ctx)? {
                    current_blocks.push(WordBlock::Table(tbl));
                }
            }
            "sdt" => {
                if let Some(content) = child.child_by_local("sdtContent") {
                    for sc in content.children_elements() {
                        match sc.local_name() {
                            "p" => {
                                let mut para = parse_paragraph(sc, &mut ctx, None)?;
                                let sect_break = para.props.section_break.take();
                                current_blocks.push(WordBlock::Paragraph(para));
                                if let Some(sect_props) = sect_break {
                                    doc.sections.push(WordSection {
                                        props: *sect_props,
                                        blocks: std::mem::take(&mut current_blocks),
                                    });
                                }
                            }
                            "tbl" => {
                                if let Some(tbl) = parse_table(sc, &mut ctx)? {
                                    current_blocks.push(WordBlock::Table(tbl));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "sectPr" => {
                final_sect_props = Some(parse_section_properties(child));
            }
            _ => {}
        }
    }

    if !current_blocks.is_empty() || doc.sections.is_empty() {
        doc.sections.push(WordSection {
            props: final_sect_props.unwrap_or_default(),
            blocks: current_blocks,
        });
    } else if let Some(final_props) = final_sect_props
        && let Some(last) = doc.sections.last_mut()
    {
        last.props = final_props;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn parse_story_blocks(
    xml: &str,
    rels: &BTreeMap<String, RelEntry>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_entry_bytes: usize,
    theme: &WordTheme,
    forensic: &mut ForensicFacts,
    image_map: &mut BTreeMap<String, ImageResource>,
    rev_authors: &mut BTreeSet<String>,
) -> Result<Vec<WordBlock>> {
    let root = XmlNode::parse(xml);
    let mut ctx = StoryParseCtx {
        rels,
        archive,
        max_entry_bytes,
        theme,
        forensic,
        image_map,
        rev_authors,
        table_depth: 0,
    };
    let mut blocks = Vec::new();
    for top in root.children_elements() {
        parse_container_blocks(top, &mut ctx, &mut blocks)?;
    }
    Ok(blocks)
}

fn parse_container_blocks(
    container: &XmlNode,
    ctx: &mut StoryParseCtx<'_, '_>,
    out: &mut Vec<WordBlock>,
) -> Result<()> {
    for child in container.children_elements() {
        match child.local_name() {
            "p" => out.push(WordBlock::Paragraph(parse_paragraph(child, ctx, None)?)),
            "tbl" => {
                if let Some(tbl) = parse_table(child, ctx)? {
                    out.push(WordBlock::Table(tbl));
                }
            }
            "sdt" => {
                if let Some(content) = child.child_by_local("sdtContent") {
                    parse_container_blocks(content, ctx, out)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn parse_notes_xml(
    xml: &str,
    note_tag: &str,
    rels: &BTreeMap<String, RelEntry>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_entry_bytes: usize,
    theme: &WordTheme,
    forensic: &mut ForensicFacts,
    image_map: &mut BTreeMap<String, ImageResource>,
    rev_authors: &mut BTreeSet<String>,
) -> Result<BTreeMap<i32, WordStory>> {
    let mut notes = BTreeMap::new();
    let root = XmlNode::parse(xml);
    let mut ctx = StoryParseCtx {
        rels,
        archive,
        max_entry_bytes,
        theme,
        forensic,
        image_map,
        rev_authors,
        table_depth: 0,
    };
    for top in root.children_elements() {
        for note in top.children_elements() {
            if note.local_name() == note_tag
                && let Some(id) = note.attr("w:id").and_then(|v| v.parse::<i32>().ok())
            {
                let note_type = note.attr("w:type").unwrap_or("normal");
                if id <= 0 || note_type != "normal" {
                    continue;
                }
                let mut blocks = Vec::new();
                parse_container_blocks(note, &mut ctx, &mut blocks)?;
                notes.insert(
                    id,
                    WordStory {
                        id: id.to_string(),
                        blocks,
                    },
                );
            }
        }
    }
    Ok(notes)
}

#[allow(clippy::too_many_arguments)]
fn parse_comments_xml(
    xml: &str,
    rels: &BTreeMap<String, RelEntry>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_entry_bytes: usize,
    theme: &WordTheme,
    forensic: &mut ForensicFacts,
    image_map: &mut BTreeMap<String, ImageResource>,
    rev_authors: &mut BTreeSet<String>,
) -> Result<BTreeMap<String, WordComment>> {
    let mut comments = BTreeMap::new();
    let root = XmlNode::parse(xml);
    let mut comment_authors = BTreeSet::new();
    let mut ctx = StoryParseCtx {
        rels,
        archive,
        max_entry_bytes,
        theme,
        forensic,
        image_map,
        rev_authors,
        table_depth: 0,
    };
    for top in root.children_elements() {
        for c_el in top.children_elements() {
            if c_el.local_name() == "comment"
                && let Some(id) = c_el.attr("w:id")
            {
                let author = c_el.attr("w:author").map(str::to_string);
                if let Some(ref a) = author
                    && !a.is_empty()
                {
                    comment_authors.insert(a.clone());
                }
                let initials = c_el.attr("w:initials").map(str::to_string);
                let date = c_el.attr("w:date").map(str::to_string);
                let mut blocks = Vec::new();
                parse_container_blocks(c_el, &mut ctx, &mut blocks)?;
                comments.insert(
                    id.to_string(),
                    WordComment {
                        id: id.to_string(),
                        author,
                        initials,
                        date,
                        parent_id: None,
                        done: false,
                        blocks,
                    },
                );
            }
        }
    }
    ctx.forensic.comment_authors = comment_authors.into_iter().collect();
    Ok(comments)
}

/// Active complex field builder (`w:fldChar` `begin`..`separate`..`end`).
struct ActiveComplexField {
    instruction: String,
    in_result: bool,
    cached_result: Vec<ParagraphContent>,
    locked: bool,
    dirty: bool,
}

fn parse_paragraph(
    p_node: &XmlNode,
    ctx: &mut StoryParseCtx<'_, '_>,
    active_rev: Option<&WordRevisionMeta>,
) -> Result<WordParagraph> {
    let mut para = WordParagraph::default();
    if let Some(ppr) = p_node.child_by_local("pPr") {
        para.props = parse_paragraph_properties(ppr, ctx.theme);
    }

    let mut field_stack: Vec<ActiveComplexField> = Vec::new();
    parse_paragraph_children(p_node, ctx, active_rev, &mut field_stack, &mut para.content)?;

    while let Some(af) = field_stack.pop() {
        let field = finalize_complex_field(af, ctx.forensic);
        push_inline_content(
            &mut field_stack,
            &mut para.content,
            ParagraphContent::Field(field),
        );
    }

    Ok(para)
}

fn push_inline_content(
    field_stack: &mut [ActiveComplexField],
    top_out: &mut Vec<ParagraphContent>,
    item: ParagraphContent,
) {
    if let Some(af) = field_stack.last_mut() {
        if af.in_result {
            af.cached_result.push(item);
        }
    } else {
        top_out.push(item);
    }
}

fn finalize_complex_field(af: ActiveComplexField, forensic: &mut ForensicFacts) -> WordField {
    let kind = classify_field_instruction(&af.instruction, forensic);
    WordField {
        instruction: af.instruction.trim().to_string(),
        kind,
        cached_result: af.cached_result,
        locked: af.locked,
        dirty: af.dirty,
    }
}

#[allow(clippy::too_many_lines)]
fn parse_paragraph_children(
    container: &XmlNode,
    ctx: &mut StoryParseCtx<'_, '_>,
    active_rev: Option<&WordRevisionMeta>,
    field_stack: &mut Vec<ActiveComplexField>,
    out: &mut Vec<ParagraphContent>,
) -> Result<()> {
    for child in container.children_elements() {
        match child.local_name() {
            "r" => {
                parse_run_element(child, ctx, active_rev, field_stack, out)?;
            }
            "fldSimple" => {
                let instr = child.attr("w:instr").unwrap_or("").to_string();
                let locked = child
                    .attr("w:fldLock")
                    .is_some_and(|v| matches!(v, "1" | "true" | "on"));
                let dirty = child
                    .attr("w:dirty")
                    .is_some_and(|v| matches!(v, "1" | "true" | "on"));
                let kind = classify_field_instruction(&instr, ctx.forensic);
                let mut cached_result = Vec::new();
                let mut inner_stack = Vec::new();
                parse_paragraph_children(
                    child,
                    ctx,
                    active_rev,
                    &mut inner_stack,
                    &mut cached_result,
                )?;
                let field = WordField {
                    instruction: instr.trim().to_string(),
                    kind,
                    cached_result,
                    locked,
                    dirty,
                };
                push_inline_content(field_stack, out, ParagraphContent::Field(field));
            }
            "hyperlink" => {
                let target = if let Some(rid) = child.attr("r:id") {
                    ctx.rels
                        .get(rid)
                        .map(|r| r.target.clone())
                        .unwrap_or_default()
                } else if let Some(anchor) = child.attr("w:anchor") {
                    format!("#{anchor}")
                } else {
                    String::new()
                };
                let tooltip = child.attr("w:tooltip").map(str::to_string);
                let mut content = Vec::new();
                parse_paragraph_children(child, ctx, active_rev, field_stack, &mut content)?;
                push_inline_content(
                    field_stack,
                    out,
                    ParagraphContent::Hyperlink(WordHyperlink {
                        target,
                        tooltip,
                        content,
                    }),
                );
            }
            "ins" | "del" | "moveFrom" | "moveTo" => {
                let kind = match child.local_name() {
                    "del" => RevisionKind::Deleted,
                    "moveFrom" => RevisionKind::MoveFrom,
                    "moveTo" => RevisionKind::MoveTo,
                    _ => RevisionKind::Inserted,
                };
                let author = child.attr("w:author").map(str::to_string);
                if let Some(ref a) = author
                    && !a.is_empty()
                {
                    ctx.rev_authors.insert(a.clone());
                }
                ctx.forensic.revision_count += 1;
                let rev = WordRevisionMeta {
                    id: child.attr("w:id").unwrap_or("0").to_string(),
                    kind,
                    author,
                    date: child.attr("w:date").map(str::to_string),
                };
                parse_paragraph_children(child, ctx, Some(&rev), field_stack, out)?;
            }
            "bookmarkStart" => {
                if let (Some(id), Some(name)) = (child.attr("w:id"), child.attr("w:name")) {
                    push_inline_content(
                        field_stack,
                        out,
                        ParagraphContent::BookmarkStart {
                            id: id.to_string(),
                            name: name.to_string(),
                        },
                    );
                }
            }
            "bookmarkEnd" => {
                if let Some(id) = child.attr("w:id") {
                    push_inline_content(
                        field_stack,
                        out,
                        ParagraphContent::BookmarkEnd { id: id.to_string() },
                    );
                }
            }
            "commentRangeStart" => {
                if let Some(id) = child.attr("w:id") {
                    push_inline_content(
                        field_stack,
                        out,
                        ParagraphContent::CommentRangeStart(id.to_string()),
                    );
                }
            }
            "commentRangeEnd" => {
                if let Some(id) = child.attr("w:id") {
                    push_inline_content(
                        field_stack,
                        out,
                        ParagraphContent::CommentRangeEnd(id.to_string()),
                    );
                }
            }
            "sdt" => {
                if let Some(sc) = child.child_by_local("sdtContent") {
                    parse_paragraph_children(sc, ctx, active_rev, field_stack, out)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn parse_run_element(
    r_node: &XmlNode,
    ctx: &mut StoryParseCtx<'_, '_>,
    active_rev: Option<&WordRevisionMeta>,
    field_stack: &mut Vec<ActiveComplexField>,
    out: &mut Vec<ParagraphContent>,
) -> Result<()> {
    let props = r_node
        .child_by_local("rPr")
        .map_or_else(RunProperties::default, |rpr| {
            parse_run_properties(rpr, ctx.theme)
        });

    if props.vanish == Some(ToggleProp::Toggle) {
        ctx.forensic.hidden_run_count += 1;
    }
    if props.font_size_pt.is_some_and(|sz| sz < 2.0)
        || props
            .color
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case("#FFFFFF"))
    {
        ctx.forensic.concealed_run_count += 1;
    }

    let mut items: Vec<RunItem> = Vec::new();

    for c in r_node.children_elements() {
        match c.local_name() {
            "fldChar" => {
                if !items.is_empty() {
                    let run = WordRun {
                        props: props.clone(),
                        items: std::mem::take(&mut items),
                        revision: active_rev.cloned(),
                    };
                    push_inline_content(field_stack, out, ParagraphContent::Run(run));
                }
                match c.attr("w:fldCharType").unwrap_or("") {
                    "begin" => {
                        if field_stack.len() < 16 {
                            let locked = c
                                .attr("w:fldLock")
                                .is_some_and(|v| matches!(v, "1" | "true" | "on"));
                            let dirty = c
                                .attr("w:dirty")
                                .is_some_and(|v| matches!(v, "1" | "true" | "on"));
                            field_stack.push(ActiveComplexField {
                                instruction: String::new(),
                                in_result: false,
                                cached_result: Vec::new(),
                                locked,
                                dirty,
                            });
                        }
                    }
                    "separate" => {
                        if let Some(af) = field_stack.last_mut() {
                            af.in_result = true;
                        }
                    }
                    "end" => {
                        if let Some(af) = field_stack.pop() {
                            let field = finalize_complex_field(af, ctx.forensic);
                            push_inline_content(field_stack, out, ParagraphContent::Field(field));
                        }
                    }
                    _ => {}
                }
            }
            "instrText" | "delInstrText" => {
                if let Some(af) = field_stack.last_mut() {
                    af.instruction.push_str(&c.text_content());
                }
            }
            "t" | "delText" => {
                let text = c.text_content();
                if !text.is_empty() {
                    items.push(RunItem::Text(text));
                }
            }
            "tab" => items.push(RunItem::Tab),
            "cr" => items.push(RunItem::LineBreak),
            "br" => match c.attr("w:type").unwrap_or("textWrapping") {
                "page" => items.push(RunItem::PageBreak),
                "column" => items.push(RunItem::ColumnBreak),
                _ => items.push(RunItem::LineBreak),
            },
            "softHyphen" => items.push(RunItem::SoftHyphen),
            "noBreakHyphen" => items.push(RunItem::NoBreakHyphen),
            "sym" => {
                let font = c.attr("w:font").unwrap_or("Symbol").to_string();
                let char_code = c
                    .attr("w:char")
                    .and_then(|v| u16::from_str_radix(v, 16).ok())
                    .unwrap_or(0x2022);
                items.push(RunItem::Symbol { font, char_code });
            }
            "footnoteReference" => {
                if let Some(id) = c.attr("w:id").and_then(|v| v.parse::<i32>().ok()) {
                    items.push(RunItem::FootnoteRef(id));
                }
            }
            "endnoteReference" => {
                if let Some(id) = c.attr("w:id").and_then(|v| v.parse::<i32>().ok()) {
                    items.push(RunItem::EndnoteRef(id));
                }
            }
            "drawing" => {
                if let Some(drawing) = parse_drawing_element(c, ctx)? {
                    items.push(RunItem::Drawing(drawing));
                }
            }
            "pict" | "object" => {
                if let Some(drawing) = parse_vml_pict(c, ctx)? {
                    items.push(RunItem::Drawing(drawing));
                }
            }
            _ => {}
        }
    }

    if !items.is_empty() {
        let run = WordRun {
            props,
            items,
            revision: active_rev.cloned(),
        };
        push_inline_content(field_stack, out, ParagraphContent::Run(run));
    }

    Ok(())
}

pub(crate) fn classify_field_instruction(instr: &str, forensic: &mut ForensicFacts) -> FieldKind {
    let trimmed = instr.trim();
    let mut parts = trimmed.split_whitespace();
    let Some(first) = parts.next() else {
        return FieldKind::Other(String::new());
    };
    let cmd = first.to_ascii_uppercase();
    match cmd.as_str() {
        "PAGE" => FieldKind::Page,
        "NUMPAGES" => FieldKind::NumPages,
        "SECTIONPAGES" => FieldKind::SectionPages,
        "TOC" => FieldKind::Toc,
        "HYPERLINK" => {
            let arg = extract_field_argument(&trimmed[first.len()..]);
            FieldKind::Hyperlink(arg)
        }
        "INCLUDEPICTURE" => {
            let arg = extract_field_argument(&trimmed[first.len()..]);
            forensic.external_relationships.push(ExternalReference {
                id: "field".to_string(),
                ref_type: "INCLUDEPICTURE".to_string(),
                target: arg.clone(),
            });
            FieldKind::IncludePicture(arg)
        }
        "INCLUDETEXT" => {
            let arg = extract_field_argument(&trimmed[first.len()..]);
            forensic.external_relationships.push(ExternalReference {
                id: "field".to_string(),
                ref_type: "INCLUDETEXT".to_string(),
                target: arg.clone(),
            });
            FieldKind::IncludeText(arg)
        }
        "LINK" | "DDE" | "DDEAUTO" => {
            let arg = extract_field_argument(&trimmed[first.len()..]);
            forensic.external_relationships.push(ExternalReference {
                id: "field".to_string(),
                ref_type: cmd.clone(),
                target: arg,
            });
            FieldKind::Other(cmd)
        }
        _ => FieldKind::Other(cmd),
    }
}

fn extract_field_argument(rest: &str) -> String {
    let s = rest.trim();
    if let Some(after_quote) = s.strip_prefix('"')
        && let Some((quoted, _)) = after_quote.split_once('"')
    {
        return quoted.to_string();
    }
    s.split_whitespace()
        .find(|tok| !tok.starts_with('\\'))
        .unwrap_or("")
        .trim_matches('"')
        .to_string()
}

#[allow(clippy::too_many_lines)]
fn parse_drawing_element(
    drawing_node: &XmlNode,
    ctx: &mut StoryParseCtx<'_, '_>,
) -> Result<Option<WordDrawing>> {
    let container = drawing_node
        .child_by_local("inline")
        .or_else(|| drawing_node.child_by_local("anchor"));
    let Some(wp) = container else {
        return Ok(None);
    };

    let is_anchor = wp.local_name() == "anchor";
    let mut width_pt = 72.0;
    let mut height_pt = 72.0;
    if let Some(ext) = wp.child_by_local("extent") {
        if let Some(cx) = ext.attr("cx").and_then(|v| v.parse::<f32>().ok()) {
            width_pt = (cx / EMU_PER_POINT).max(1.0);
        }
        if let Some(cy) = ext.attr("cy").and_then(|v| v.parse::<f32>().ok()) {
            height_pt = (cy / EMU_PER_POINT).max(1.0);
        }
    }

    let alt_text = wp.child_by_local("docPr").and_then(|dp| {
        dp.attr("descr")
            .filter(|d| !d.is_empty())
            .or_else(|| dp.attr("title").filter(|t| !t.is_empty()))
            .map(str::to_string)
    });

    let mut wrap = WrapMode::Inline;
    let mut anchor = None;

    if is_anchor {
        wrap = WrapMode::None;
        let behind_doc = wp
            .attr("behindDoc")
            .is_some_and(|v| matches!(v, "1" | "true"));
        let allow_overlap = wp
            .attr("allowOverlap")
            .is_none_or(|v| !matches!(v, "0" | "false"));
        let layout_in_cell = wp
            .attr("layoutInCell")
            .is_none_or(|v| !matches!(v, "0" | "false"));
        let z_order = wp
            .attr("relativeHeight")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);

        let mut horizontal_offset = 0.0;
        let mut h_relative_from = "column".to_string();
        if let Some(ph) = wp.child_by_local("positionH") {
            if let Some(rf) = ph.attr("relativeFrom") {
                h_relative_from = rf.to_string();
            }
            if let Some(po) = ph.child_by_local("posOffset")
                && let Ok(emu) = po.text_content().trim().parse::<f32>()
            {
                horizontal_offset = emu / EMU_PER_POINT;
            }
        }

        let mut vertical_offset = 0.0;
        let mut v_relative_from = "paragraph".to_string();
        if let Some(pv) = wp.child_by_local("positionV") {
            if let Some(rf) = pv.attr("relativeFrom") {
                v_relative_from = rf.to_string();
            }
            if let Some(po) = pv.child_by_local("posOffset")
                && let Ok(emu) = po.text_content().trim().parse::<f32>()
            {
                vertical_offset = emu / EMU_PER_POINT;
            }
        }

        for c in wp.children_elements() {
            match c.local_name() {
                "wrapSquare" => wrap = WrapMode::Square,
                "wrapTopAndBottom" => wrap = WrapMode::TopAndBottom,
                "wrapNone" => wrap = WrapMode::None,
                "wrapTight" => {
                    wrap = WrapMode::Tight {
                        polygon: parse_wrap_polygon(c),
                    };
                }
                "wrapThrough" => {
                    wrap = WrapMode::Through {
                        polygon: parse_wrap_polygon(c),
                    };
                }
                _ => {}
            }
        }

        anchor = Some(AnchorPlacement {
            pos_x_pt: horizontal_offset,
            h_relative_from,
            pos_y_pt: vertical_offset,
            v_relative_from,
            behind_doc,
            allow_overlap,
            layout_in_cell,
            z_order,
        });
    }

    if let Some((rid, crop)) = find_blip_in_subtree(wp) {
        load_image_resource(&rid, ctx)?;
        return Ok(Some(WordDrawing {
            width_pt,
            height_pt,
            alt_text,
            rotation_deg: 0.0,
            wrap,
            anchor,
            payload: DrawingPayload::Image {
                resource_id: rid,
                crop,
            },
        }));
    }

    if let Some(txbx_content) = find_node_by_local(wp, "txbxContent") {
        let mut blocks = Vec::new();
        parse_container_blocks(txbx_content, ctx, &mut blocks)?;
        return Ok(Some(WordDrawing {
            width_pt,
            height_pt,
            alt_text,
            rotation_deg: 0.0,
            wrap,
            anchor,
            payload: DrawingPayload::TextBox {
                blocks,
                fill_color: None,
                stroke_color: None,
                stroke_width_pt: 0.75,
            },
        }));
    }

    if let Some(chart_node) = find_node_by_local(wp, "chart")
        && let Some(rid) = chart_node.attr("r:id").or_else(|| chart_node.attr("id"))
        && let Some(rel) = ctx.rels.get(rid)
        && !rel.external
    {
        let zip_path = resolve_word_zip_path(&rel.target);
        if let Ok(mut chart_file) = ctx.archive.by_name(&zip_path) {
            let mut chart_bytes = Vec::new();
            read_to_end_with_limit(&mut chart_file, &mut chart_bytes, ctx.max_entry_bytes)?;
            let chart_xml = String::from_utf8_lossy(&chart_bytes);
            if let Ok(chart_space) = prism_drawingml::parse_chart_xml(&chart_xml) {
                return Ok(Some(WordDrawing {
                    width_pt,
                    height_pt,
                    alt_text,
                    rotation_deg: 0.0,
                    wrap,
                    anchor,
                    payload: DrawingPayload::Chart(Box::new(chart_space)),
                }));
            }
        }
    }

    Ok(None)
}

fn parse_vml_pict(
    pict_node: &XmlNode,
    ctx: &mut StoryParseCtx<'_, '_>,
) -> Result<Option<WordDrawing>> {
    if let Some(img_data) = find_node_by_local(pict_node, "imagedata")
        && let Some(rid) = img_data.attr("r:id").or_else(|| img_data.attr("o:relid"))
    {
        load_image_resource(rid, ctx)?;
        return Ok(Some(WordDrawing {
            width_pt: 72.0,
            height_pt: 72.0,
            alt_text: img_data.attr("o:title").map(str::to_string),
            rotation_deg: 0.0,
            wrap: WrapMode::Inline,
            anchor: None,
            payload: DrawingPayload::Image {
                resource_id: rid.to_string(),
                crop: None,
            },
        }));
    }
    if let Some(txbx_content) = find_node_by_local(pict_node, "txbxContent") {
        let mut blocks = Vec::new();
        parse_container_blocks(txbx_content, ctx, &mut blocks)?;
        return Ok(Some(WordDrawing {
            width_pt: 144.0,
            height_pt: 72.0,
            alt_text: None,
            rotation_deg: 0.0,
            wrap: WrapMode::Inline,
            anchor: None,
            payload: DrawingPayload::TextBox {
                blocks,
                fill_color: None,
                stroke_color: None,
                stroke_width_pt: 0.75,
            },
        }));
    }
    Ok(None)
}

fn parse_wrap_polygon(wrap_node: &XmlNode) -> Vec<(f32, f32)> {
    let mut pts = Vec::new();
    let Some(poly) = wrap_node.child_by_local("wrapPolygon") else {
        return pts;
    };
    for c in poly.children_elements() {
        if matches!(c.local_name(), "start" | "lineTo")
            && let (Some(x), Some(y)) = (
                c.attr("x").and_then(|v| v.parse::<f32>().ok()),
                c.attr("y").and_then(|v| v.parse::<f32>().ok()),
            )
        {
            pts.push((x, y));
        }
    }
    pts
}

fn find_blip_in_subtree(node: &XmlNode) -> Option<(String, Option<CropRect>)> {
    if node.local_name() == "blipFill" {
        let rid = node
            .child_by_local("blip")
            .and_then(|b| b.attr("r:embed"))
            .map(str::to_string)?;
        let crop = node.child_by_local("srcRect").map(|sr| {
            let pct = |k: &str| {
                sr.attr(k)
                    .and_then(|v| v.parse::<f32>().ok())
                    .map_or(0.0, |v| (v / 100_000.0).clamp(0.0, 1.0))
            };
            (pct("l"), pct("t"), pct("r"), pct("b"))
        });
        return Some((rid, crop));
    }
    if node.local_name() == "blip"
        && let Some(rid) = node.attr("r:embed")
    {
        return Some((rid.to_string(), None));
    }
    for c in node.children_elements() {
        if let Some(res) = find_blip_in_subtree(c) {
            return Some(res);
        }
    }
    None
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

fn load_image_resource(rid: &str, ctx: &mut StoryParseCtx<'_, '_>) -> Result<()> {
    if ctx.image_map.contains_key(rid) {
        return Ok(());
    }
    let Some(rel) = ctx.rels.get(rid) else {
        return Ok(());
    };
    if rel.external {
        return Ok(());
    }
    let zip_path = resolve_word_zip_path(&rel.target);
    let Ok(mut image_file) = ctx.archive.by_name(&zip_path) else {
        return Ok(());
    };
    let mut image_data = Vec::new();
    read_to_end_with_limit(&mut image_file, &mut image_data, ctx.max_entry_bytes)?;
    let (width, height) = image_pixel_size(&image_data);
    let mime_type = image_mime_type(&image_data, &zip_path);
    ctx.image_map.insert(
        rid.to_string(),
        ImageResource {
            id: rid.to_string(),
            mime_type,
            data: Some(image_data),
            url: None,
            width,
            height,
        },
    );
    Ok(())
}

fn image_mime_type(data: &[u8], path: &str) -> String {
    prism_core::format::detect_format(data, Some(path))
        .map(|detection| detection.format.mime_type)
        .filter(|mime| mime.starts_with("image/"))
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn image_pixel_size(data: &[u8]) -> (u32, u32) {
    image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .unwrap_or((0, 0))
}

fn parse_table(tbl_node: &XmlNode, ctx: &mut StoryParseCtx<'_, '_>) -> Result<Option<WordTable>> {
    if ctx.table_depth >= MAX_TABLE_DEPTH {
        return Ok(None);
    }
    ctx.table_depth += 1;

    let mut table = WordTable::default();
    if let Some(tbl_pr) = tbl_node.child_by_local("tblPr") {
        table.props = parse_table_properties(tbl_pr, ctx.theme);
    }
    if let Some(grid) = tbl_node.child_by_local("tblGrid") {
        for col in grid.children_elements() {
            if col.local_name() == "gridCol"
                && let Some(w) = col.attr("w:w").and_then(|v| v.parse::<f32>().ok())
            {
                table
                    .grid_col_widths_pt
                    .push((w / TWIPS_PER_POINT).max(1.0));
            }
        }
    }

    for child in tbl_node.children_elements() {
        if child.local_name() == "tr" {
            let row = parse_table_row(child, ctx)?;
            table.rows.push(row);
        }
    }

    ctx.table_depth -= 1;
    Ok(Some(table))
}

fn parse_table_properties(tbl_pr: &XmlNode, theme: &WordTheme) -> TableProperties {
    let mut props = TableProperties::default();
    for c in tbl_pr.children_elements() {
        match c.local_name() {
            "tblStyle" => props.style_id = c.attr("w:val").map(str::to_string),
            "tblW" => props.preferred_width = parse_table_width(c),
            "jc" => {
                if let Some(v) = c.attr("w:val") {
                    props.alignment = parse_alignment(v);
                }
            }
            "tblInd" => {
                if let TableWidth::Pt(pt) = parse_table_width(c) {
                    props.indent_left_pt = pt;
                }
            }
            "tblLayout" => {
                if c.attr("w:type").is_some_and(|t| t == "fixed") {
                    props.layout_mode = TableLayoutMode::Fixed;
                }
            }
            "tblCellMar" => props.cell_margins = parse_cell_margins(c),
            "tblBorders" => props.borders = parse_borders(c, theme),
            "shd" => props.shading_fill = parse_shading_fill(c, theme),
            "tblLook" => {
                if let Some(fr) = c.attr("w:firstRow") {
                    props.look_first_row = !matches!(fr, "0" | "false");
                }
                if let Some(lr) = c.attr("w:lastRow") {
                    props.look_last_row = matches!(lr, "1" | "true");
                }
                if let Some(fc) = c.attr("w:firstColumn") {
                    props.look_first_col = !matches!(fc, "0" | "false");
                }
                if let Some(lc) = c.attr("w:lastColumn") {
                    props.look_last_col = matches!(lc, "1" | "true");
                }
                if let Some(nh) = c.attr("w:noHBand") {
                    props.look_h_band = matches!(nh, "0" | "false");
                }
                if let Some(nv) = c.attr("w:noVBand") {
                    props.look_v_band = matches!(nv, "0" | "false");
                }
            }
            _ => {}
        }
    }
    props
}

fn parse_table_row(tr_node: &XmlNode, ctx: &mut StoryParseCtx<'_, '_>) -> Result<WordTableRow> {
    let mut row = WordTableRow::default();
    if let Some(tr_pr) = tr_node.child_by_local("trPr") {
        for c in tr_pr.children_elements() {
            match c.local_name() {
                "trHeight" => {
                    if let Some(twips) = c.attr("w:val").and_then(|v| v.parse::<f32>().ok()) {
                        row.height_pt = Some((twips / TWIPS_PER_POINT).max(0.0));
                    }
                    row.height_exact = c.attr("w:hRule").is_some_and(|r| r == "exact");
                }
                "tblHeader" => row.is_header = parse_bool_attr(c),
                "cantSplit" => row.cant_split = parse_bool_attr(c),
                _ => {}
            }
        }
    }

    for child in tr_node.children_elements() {
        if child.local_name() == "tc" {
            row.cells.push(parse_table_cell(child, ctx)?);
        }
    }
    Ok(row)
}

fn parse_table_cell(tc_node: &XmlNode, ctx: &mut StoryParseCtx<'_, '_>) -> Result<WordTableCell> {
    let mut cell = WordTableCell::default();
    if let Some(tc_pr) = tc_node.child_by_local("tcPr") {
        for c in tc_pr.children_elements() {
            match c.local_name() {
                "tcW" => cell.preferred_width = Some(parse_table_width(c)),
                "gridSpan" => {
                    if let Some(gs) = c.attr("w:val").and_then(|v| v.parse::<u16>().ok()) {
                        cell.grid_span = gs.max(1);
                    }
                }
                "vMerge" => {
                    cell.v_merge = match c.attr("w:val") {
                        Some("restart") => VerticalMerge::Restart,
                        _ => VerticalMerge::Continue,
                    };
                }
                "tcMar" => cell.margins = Some(parse_cell_margins(c)),
                "tcBorders" => cell.borders = parse_borders(c, ctx.theme),
                "shd" => cell.shading_fill = parse_shading_fill(c, ctx.theme),
                "vAlign" => {
                    cell.vertical_align = match c.attr("w:val").unwrap_or("top") {
                        "center" => PageVerticalAlign::Center,
                        "both" => PageVerticalAlign::Both,
                        "bottom" => PageVerticalAlign::Bottom,
                        _ => PageVerticalAlign::Top,
                    };
                }
                _ => {}
            }
        }
    }

    parse_container_blocks(tc_node, ctx, &mut cell.blocks)?;
    Ok(cell)
}

fn parse_table_width(node: &XmlNode) -> TableWidth {
    let w_type = node.attr("w:type").unwrap_or("dxa");
    let val = node
        .attr("w:w")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0);
    match w_type {
        "auto" | "nil" => TableWidth::Auto,
        "pct" => {
            let pct = if val > 100.0 { val / 50.0 } else { val };
            TableWidth::Percent(pct.clamp(0.0, 100.0))
        }
        _ => TableWidth::Pt((val / TWIPS_PER_POINT).max(0.0)),
    }
}

fn parse_cell_margins(node: &XmlNode) -> CellMargins {
    let mut mar = CellMargins::default();
    for c in node.children_elements() {
        if let TableWidth::Pt(pt) = parse_table_width(c) {
            match c.local_name() {
                "top" => mar.top_pt = pt,
                "right" | "end" => mar.right_pt = pt,
                "bottom" => mar.bottom_pt = pt,
                "left" | "start" => mar.left_pt = pt,
                _ => {}
            }
        }
    }
    mar
}

/// Deobfuscate an OOXML `.odttf` font stream using its 16-byte GUID key (ECMA-376 §17.8.1).
#[must_use]
pub fn deobfuscate_odttf_bytes(data: &[u8], guid: &str) -> Option<Vec<u8>> {
    if data.len() < 32 {
        return None;
    }
    let hex_digits: String = guid.chars().filter(char::is_ascii_hexdigit).collect();
    if hex_digits.len() != 32 {
        return None;
    }
    let mut guid_bytes = [0u8; 16];
    for i in 0..16 {
        guid_bytes[i] = u8::from_str_radix(&hex_digits[i * 2..i * 2 + 2], 16).ok()?;
    }
    let mut out = data.to_vec();
    for i in 0..32 {
        out[i] ^= guid_bytes[15 - (i % 16)];
    }
    Some(out)
}

fn extract_embedded_fonts(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    limit: usize,
) -> Vec<FontResource> {
    let Ok(Some(rels_xml)) = read_zip_string(archive, "word/_rels/fontTable.xml.rels", limit)
    else {
        return Vec::new();
    };
    let font_rels = parse_relationships(&rels_xml);
    if font_rels.is_empty() {
        return Vec::new();
    }
    let Ok(Some(ft_xml)) = read_zip_string(archive, "word/fontTable.xml", limit) else {
        return Vec::new();
    };
    let root = XmlNode::parse(&ft_xml);
    let Some(fonts_el) = root.child_by_local("fonts") else {
        return Vec::new();
    };

    let mut specs: Vec<(String, &'static str, String, Option<String>)> = Vec::new();
    for font_el in fonts_el.children_elements() {
        if font_el.local_name() == "font"
            && let Some(family) = font_el.attr("w:name")
        {
            for sub in font_el.children_elements() {
                let style = match sub.local_name() {
                    "embedRegular" => Some("Regular"),
                    "embedBold" => Some("Bold"),
                    "embedItalic" => Some("Italic"),
                    "embedBoldItalic" => Some("BoldItalic"),
                    _ => None,
                };
                if let (Some(st), Some(rid)) = (style, sub.attr("r:id")) {
                    let font_key = sub.attr("w:fontKey").map(str::to_string);
                    specs.push((family.to_string(), st, rid.to_string(), font_key));
                }
            }
        }
    }

    let mut resources = Vec::new();
    for (family, style, r_id, font_key) in specs {
        let Some(rel) = font_rels.get(&r_id) else {
            continue;
        };
        if rel.external {
            continue;
        }
        let zip_path = resolve_word_zip_path(&rel.target);
        let Ok(mut font_file) = archive.by_name(&zip_path) else {
            continue;
        };
        let mut raw = Vec::new();
        if read_to_end_with_limit(&mut font_file, &mut raw, limit).is_err() {
            continue;
        }
        let is_odttf = std::path::Path::new(&zip_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("odttf"));
        let decoded = if is_odttf && let Some(ref key) = font_key {
            deobfuscate_odttf_bytes(&raw, key).unwrap_or(raw)
        } else {
            raw
        };
        resources.push(FontResource {
            family,
            style: style.to_string(),
            embedded: true,
            data: Some(decoded),
        });
    }
    resources
}
