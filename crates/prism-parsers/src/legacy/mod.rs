// SPDX-License-Identifier: AGPL-3.0-only
//! Legacy and regional document & database parsers (`M8.3`):
//! - **Corel `WordPerfect` (`.wpd`)**: WP6+ prefix, index/packet area, and variable-length function groups.
//! - **Korean Hangul Word Processor (`.hwp`)**: HWP v5.0 OLE2 (`FileHeader`, `DocInfo`, `BodyText/Section*`, `BinData/*`) and v3.0.
//! - **Korean Hangul OWPML (`.hwpx`)**: ZIP/XML (`Contents/section*.xml`, `<hp:p>`, `<hp:tbl>`).

#![allow(
    clippy::case_sensitive_file_extension_comparisons,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unused_self
)]

use crate::limits::read_bounded;
use async_trait::async_trait;
use bytes::Bytes;
use cfb::CompoundFile;
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use prism_core::document::{
    ContentBlock, Dimensions, Document, Page, Rect, TableBlock, TableCell, TableRow, TextBlock,
    TextRun,
};
use prism_core::error::{Error, Result};
use prism_core::format::Format;
use prism_core::metadata::{Metadata, MetadataValue};
use prism_core::parser::{ParseContext, Parser};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::io::{Cursor, Write};
use zip::ZipArchive;
use zip::write::FileOptions;

fn make_page(number: usize) -> Page {
    Page::new((number.max(1)) as u32, Dimensions::LETTER)
}

fn make_text_block(text: impl Into<String>) -> TextBlock {
    let mut tb = TextBlock::new(Rect::default());
    tb.add_run(TextRun::new(text));
    tb
}

fn make_table_cell(text: impl Into<String>) -> TableCell {
    TableCell {
        content: vec![ContentBlock::Text(make_text_block(text))],
        col_span: 1,
        row_span: 1,
        background_color: None,
        borders: None,
    }
}

// =========================================================================
// 1. Corel WordPerfect (.wpd)
// =========================================================================

/// Corel `WordPerfect` (`.wpd`) magic prefix (`\xFFWPC`).
pub const WPD_MAGIC: [u8; 4] = [0xFF, b'W', b'P', b'C'];

/// Native parser for Corel `WordPerfect` (`.wpd`) documents.
#[derive(Debug, Clone, Default)]
pub struct WpdParser;

impl WpdParser {
    /// Create a new [`WpdParser`].
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Parser for WpdParser {
    fn format(&self) -> Format {
        Format::wpd()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        data.starts_with(&WPD_MAGIC)
    }

    async fn parse(&self, data: Bytes, _context: ParseContext) -> Result<Document> {
        parse_wpd_bytes(&data)
    }
}

/// Parse a Corel `WordPerfect` (`.wpd`) byte slice into a UDM [`Document`].
pub fn parse_wpd_bytes(data: &[u8]) -> Result<Document> {
    if data.len() < 16 || !data.starts_with(&WPD_MAGIC) {
        return Err(Error::ParseError(
            "Invalid WordPerfect (.wpd) header: expected \\xFFWPC magic".to_string(),
        ));
    }

    let doc_area_ptr = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let encryption = u16::from_le_bytes([data[12], data[13]]);
    if encryption != 0 {
        return Err(Error::Encrypted(
            "WordPerfect (.wpd) document is encrypted".to_string(),
        ));
    }

    let mut metadata = Metadata {
        custom: std::collections::HashMap::new(),
        ..Metadata::default()
    };
    metadata.custom.insert(
        "format".to_string(),
        MetadataValue::String("WPD".to_string()),
    );

    // Inspect packet area (between offset 16 and doc_area_ptr) for metadata strings (`TITLE=`, `AUTHOR=`, `COMMENT=`)
    let packet_end = doc_area_ptr.clamp(16, data.len());
    let packet_slice = &data[16..packet_end];
    if let Ok(pkt_str) = std::str::from_utf8(packet_slice) {
        for line in pkt_str.lines() {
            if let Some(t) = line.strip_prefix("TITLE=") {
                metadata.title = Some(t.trim().to_string());
            } else if let Some(a) = line.strip_prefix("AUTHOR=") {
                metadata.author = Some(a.trim().to_string());
            } else if let Some(c) = line.strip_prefix("COMMENT=") {
                metadata.custom.insert(
                    "wpd_comment".to_string(),
                    MetadataValue::String(c.trim().to_string()),
                );
            }
        }
    }

    let doc_slice = if doc_area_ptr < data.len() {
        &data[doc_area_ptr..]
    } else {
        &data[16..]
    };

    let mut pages = Vec::new();
    let mut current_blocks = Vec::new();
    let mut current_runs: Vec<TextRun> = Vec::new();
    let mut cur_text = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut hidden_texts: Vec<String> = Vec::new();

    let flush_run = |cur_text: &mut String,
                     runs: &mut Vec<TextRun>,
                     bold: bool,
                     italic: bool,
                     underline: bool| {
        if !cur_text.is_empty() {
            let mut r = TextRun::new(std::mem::take(cur_text));
            r.style.bold = bold;
            r.style.italic = italic;
            r.style.underline = underline;
            runs.push(r);
        }
    };

    let flush_para = |cur_text: &mut String,
                      runs: &mut Vec<TextRun>,
                      blocks: &mut Vec<ContentBlock>,
                      bold: bool,
                      italic: bool,
                      underline: bool| {
        flush_run(cur_text, runs, bold, italic, underline);
        if !runs.is_empty() {
            let mut tb = TextBlock::new(Rect::default());
            tb.runs = std::mem::take(runs);
            blocks.push(ContentBlock::Text(tb));
        }
    };

    let mut pos = 0usize;
    while pos < doc_slice.len() {
        let b = doc_slice[pos];
        match b {
            0x0A | 0x0D => {
                if b == 0x0D && doc_slice.get(pos + 1) == Some(&0x0A) {
                    pos += 1;
                }
                flush_para(
                    &mut cur_text,
                    &mut current_runs,
                    &mut current_blocks,
                    bold,
                    italic,
                    underline,
                );
                pos += 1;
            }
            0x0C => {
                flush_para(
                    &mut cur_text,
                    &mut current_runs,
                    &mut current_blocks,
                    bold,
                    italic,
                    underline,
                );
                if !current_blocks.is_empty() {
                    let mut p = make_page(pages.len() + 1);
                    p.content = std::mem::take(&mut current_blocks);
                    pages.push(p);
                }
                pos += 1;
            }
            0x80 => {
                cur_text.push(' ');
                pos += 1;
            }
            0x81 => {
                cur_text.push('\u{00A0}');
                pos += 1;
            }
            0xA9 | 0xAA => {
                cur_text.push('-');
                pos += 1;
            }
            0xC0 if pos + 3 < doc_slice.len() && doc_slice[pos + 3] == 0xC0 => {
                // WP Extended Character: [0xC0, char_code, charset, 0xC0]
                let ch_code = doc_slice[pos + 1];
                let charset = doc_slice[pos + 2];
                let mapped = match (charset, ch_code) {
                    (4, 28) => '“',
                    (4, 29) => '”',
                    (4, 30) => '–',
                    (4, 31) => '—',
                    (4, 0) => '•',
                    _ => char::from(ch_code.max(b' ')),
                };
                cur_text.push(mapped);
                pos += 4;
            }
            0xF2 if pos + 2 < doc_slice.len() && doc_slice[pos + 2] == 0xF2 => {
                // Attribute On: [0xF2, attr, 0xF2]
                flush_run(&mut cur_text, &mut current_runs, bold, italic, underline);
                match doc_slice[pos + 1] {
                    8 => italic = true,
                    12 => bold = true,
                    14 => underline = true,
                    _ => {}
                }
                pos += 3;
            }
            0xF3 if pos + 2 < doc_slice.len() && doc_slice[pos + 2] == 0xF3 => {
                // Attribute Off: [0xF3, attr, 0xF3]
                flush_run(&mut cur_text, &mut current_runs, bold, italic, underline);
                match doc_slice[pos + 1] {
                    8 => italic = false,
                    12 => bold = false,
                    14 => underline = false,
                    _ => {}
                }
                pos += 3;
            }
            0xD0..=0xFE => {
                // Variable-length WP6 function group: [group, subgroup, size_lo, size_hi, ...]
                if pos + 4 <= doc_slice.len() {
                    let subgroup = doc_slice[pos + 1];
                    let size =
                        usize::from(u16::from_le_bytes([doc_slice[pos + 2], doc_slice[pos + 3]]));
                    if size >= 4 && pos + size <= doc_slice.len() {
                        let inner =
                            &doc_slice[pos + 4..(pos + size).saturating_sub(4).max(pos + 4)];
                        if b == 0xD0 {
                            match subgroup {
                                0x04 => cur_text.push(' '),
                                0x0A => flush_para(
                                    &mut cur_text,
                                    &mut current_runs,
                                    &mut current_blocks,
                                    bold,
                                    italic,
                                    underline,
                                ),
                                0x0C => {
                                    flush_para(
                                        &mut cur_text,
                                        &mut current_runs,
                                        &mut current_blocks,
                                        bold,
                                        italic,
                                        underline,
                                    );
                                    if !current_blocks.is_empty() {
                                        let mut p = make_page(pages.len() + 1);
                                        p.content = std::mem::take(&mut current_blocks);
                                        pages.push(p);
                                    }
                                }
                                _ => {}
                            }
                        } else if b == 0xD8 {
                            // Header/Footer/Footnote/Hidden Comment group
                            let extracted: String = inner
                                .iter()
                                .copied()
                                .filter(|&c| (0x20..=0x7E).contains(&c))
                                .map(char::from)
                                .collect();
                            if !extracted.trim().is_empty() {
                                hidden_texts.push(extracted.trim().to_string());
                            }
                        }
                        pos += size;
                        continue;
                    }
                }
                pos += 1;
            }
            0x20..=0x7E | 0x09 => {
                cur_text.push(char::from(b));
                pos += 1;
            }
            _ => {
                pos += 1;
            }
        }
    }

    flush_para(
        &mut cur_text,
        &mut current_runs,
        &mut current_blocks,
        bold,
        italic,
        underline,
    );
    if !hidden_texts.is_empty() {
        metadata.custom.insert(
            "hidden_text".to_string(),
            MetadataValue::String(hidden_texts.join(" | ")),
        );
    }

    if !current_blocks.is_empty() || pages.is_empty() {
        let mut p = make_page(pages.len() + 1);
        p.content = current_blocks;
        pages.push(p);
    }

    Ok(Document {
        metadata,
        pages,
        ..Document::default()
    })
}

/// Build a deterministic synthetic Corel `WordPerfect` (`.wpd`) file for testing.
pub fn build_wpd_bytes(
    title: &str,
    author: &str,
    paragraphs: &[&str],
    hidden_note: Option<&str>,
) -> Vec<u8> {
    let mut pkt = String::new();
    let _ = std::fmt::Write::write_fmt(&mut pkt, format_args!("TITLE={title}\nAUTHOR={author}\n"));
    let doc_area_ptr = (16 + pkt.len()) as u32;

    let mut out = Vec::new();
    out.extend_from_slice(&WPD_MAGIC);
    out.extend_from_slice(&doc_area_ptr.to_le_bytes());
    out.push(1); // product_type = WordPerfect
    out.push(0x0A); // file_type = WP document
    out.push(2); // major_ver (WP 6+)
    out.push(1); // minor_ver
    out.extend_from_slice(&0u16.to_le_bytes()); // encryption = 0 (unencrypted)
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(pkt.as_bytes());

    if let Some(note) = hidden_note {
        let note_bytes = note.as_bytes();
        let total_size = (8 + note_bytes.len()) as u16;
        out.push(0xD8);
        out.push(0x01);
        out.extend_from_slice(&total_size.to_le_bytes());
        out.extend_from_slice(note_bytes);
        out.extend_from_slice(&total_size.to_le_bytes());
        out.push(0x01);
        out.push(0xD8);
    }

    for (i, para) in paragraphs.iter().enumerate() {
        if i == 0 {
            // Wrap first paragraph in bold attribute on/off
            out.extend_from_slice(&[0xF2, 12, 0xF2]);
        }
        out.extend_from_slice(para.as_bytes());
        if i == 0 {
            out.extend_from_slice(&[0xF3, 12, 0xF3]);
        }
        // WP6 EOL hard return function group: [0xD0, 0x0A, 8, 0, 8, 0, 0x0A, 0xD0]
        out.extend_from_slice(&[0xD0, 0x0A, 0x08, 0x00, 0x08, 0x00, 0x0A, 0xD0]);
    }

    out
}

// =========================================================================
// 2. Korean Hangul Word Processor (.hwp)
// =========================================================================

/// Native parser for Korean Hangul Word Processor (`.hwp` v5.0 OLE2 and v3.0) documents.
#[derive(Debug, Clone, Default)]
pub struct HwpParser;

impl HwpParser {
    /// Create a new [`HwpParser`].
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Parser for HwpParser {
    fn format(&self) -> Format {
        Format::hwp()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        if data.starts_with(b"HWP Document File") {
            return true;
        }
        if data.len() >= 8
            && &data[0..8] == b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1"
            && let Ok(comp) = CompoundFile::open(Cursor::new(data))
        {
            return comp.exists("/FileHeader") || comp.exists("FileHeader");
        }
        false
    }

    async fn parse(&self, data: Bytes, _context: ParseContext) -> Result<Document> {
        parse_hwp_bytes(&data)
    }
}

/// Parse a Korean Hangul (`.hwp`) byte slice into a UDM [`Document`].
pub fn parse_hwp_bytes(data: &[u8]) -> Result<Document> {
    let mut metadata = Metadata {
        custom: std::collections::HashMap::new(),
        ..Metadata::default()
    };
    metadata.custom.insert(
        "format".to_string(),
        MetadataValue::String("HWP".to_string()),
    );

    if data.starts_with(b"HWP Document File") {
        // HWP 3.x is a different binary layout with Johab-encoded text; reading it as UTF-8
        // would show garbage as the document's content.
        return Err(Error::UnsupportedFormat(
            "Hangul HWP 3.x documents are not supported (only HWP 5.x)".to_string(),
        ));
    }

    let mut comp = CompoundFile::open(Cursor::new(data))
        .map_err(|e| Error::ParseError(format!("Failed to open OLE2 HWP container: {e}")))?;

    let header_path = if comp.exists("/FileHeader") {
        "/FileHeader"
    } else if comp.exists("FileHeader") {
        "FileHeader"
    } else {
        return Err(Error::ParseError(
            "Missing FileHeader stream in HWP container".to_string(),
        ));
    };

    let fh = read_bounded(
        comp.open_stream(header_path)
            .map_err(|e| Error::ParseError(format!("HWP FileHeader: {e}")))?,
        4096,
    )?;
    if fh.len() < 40 || !fh.starts_with(b"HWP Document File") {
        return Err(Error::ParseError(
            "Invalid HWP FileHeader signature".to_string(),
        ));
    }
    let flags = u32::from_le_bytes([fh[36], fh[37], fh[38], fh[39]]);
    let is_compressed = (flags & 0x01) != 0;
    let is_encrypted = (flags & 0x02) != 0;
    if is_encrypted {
        return Err(Error::Encrypted(
            "Hangul (.hwp) document is password-encrypted".to_string(),
        ));
    }

    let section_paths: Vec<String> = comp
        .walk()
        .filter(cfb::Entry::is_stream)
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.contains("BodyText/Section") || p.contains("BodyText\\Section"))
        .collect();

    let mut pages = Vec::new();
    for (sec_idx, path) in section_paths.iter().enumerate() {
        let raw_stream = read_bounded(
            comp.open_stream(path)
                .map_err(|e| Error::ParseError(format!("HWP section {path}: {e}")))?,
            32 * 1024 * 1024,
        )?;
        let sec_bytes = if is_compressed {
            let decoder = DeflateDecoder::new(Cursor::new(&raw_stream));
            read_bounded(decoder, 32 * 1024 * 1024).unwrap_or(raw_stream)
        } else {
            raw_stream
        };

        let mut blocks = Vec::new();
        let mut pos = 0usize;
        while pos + 4 <= sec_bytes.len() {
            let hdr = u32::from_le_bytes([
                sec_bytes[pos],
                sec_bytes[pos + 1],
                sec_bytes[pos + 2],
                sec_bytes[pos + 3],
            ]);
            pos += 4;
            let tag_id = hdr & 0x03FF;
            let mut rec_len = ((hdr >> 20) & 0x0FFF) as usize;
            if rec_len == 0x0FFF {
                if pos + 4 > sec_bytes.len() {
                    break;
                }
                rec_len = u32::from_le_bytes([
                    sec_bytes[pos],
                    sec_bytes[pos + 1],
                    sec_bytes[pos + 2],
                    sec_bytes[pos + 3],
                ]) as usize;
                pos += 4;
            }
            if pos + rec_len > sec_bytes.len() {
                break;
            }
            let payload = &sec_bytes[pos..pos + rec_len];
            pos += rec_len;

            // HWPTAG_PARA_TEXT = 67 (0x43)
            if tag_id == 67 {
                let text = decode_hwp_para_text(payload);
                if !text.trim().is_empty() {
                    blocks.push(ContentBlock::Text(make_text_block(text.trim())));
                }
            }
        }

        let mut p = make_page(sec_idx + 1);
        p.content = blocks;
        pages.push(p);
    }

    if pages.is_empty() {
        pages.push(make_page(1));
    }

    Ok(Document {
        metadata,
        pages,
        ..Document::default()
    })
}

fn decode_hwp_para_text(payload: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while i + 2 <= payload.len() {
        let ch = u16::from_le_bytes([payload[i], payload[i + 1]]);
        i += 2;
        match ch {
            // Tab is an inline control: eight code units like the others below.
            9 => {
                out.push('\t');
                i = i.saturating_add(14);
            }
            10 => out.push('\n'),
            // Inline (4-9, 19, 20) and extended (1-3, 11, 12, 14-18, 21-23) controls in
            // HWP v5.0 take eight code units, so seven more (14 bytes) follow the code.
            1..=8 | 11 | 12 | 14..=23 => {
                i = i.saturating_add(14);
            }
            0..=31 => {}
            _ => {
                if let Some(c) = char::from_u32(u32::from(ch)) {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// Build a deterministic synthetic Korean Hangul v5.0 (`.hwp`) OLE2 file for testing.
pub fn build_hwp_bytes(paragraphs: &[&str], compressed: bool, encrypted: bool) -> Vec<u8> {
    let mut comp = CompoundFile::create(Cursor::new(Vec::new())).unwrap();

    let mut fh = vec![0u8; 256];
    fh[0..17].copy_from_slice(b"HWP Document File");
    fh[32..36].copy_from_slice(&0x0500_0300u32.to_le_bytes()); // v5.0.3.0
    let mut flags = 0u32;
    if compressed {
        flags |= 0x01;
    }
    if encrypted {
        flags |= 0x02;
    }
    fh[36..40].copy_from_slice(&flags.to_le_bytes());
    comp.create_stream("/FileHeader")
        .unwrap()
        .write_all(&fh)
        .unwrap();

    comp.create_storage("/BodyText").unwrap();
    let mut sec0 = Vec::new();
    for para in paragraphs {
        let mut utf16_bytes = Vec::new();
        for u in para.encode_utf16() {
            utf16_bytes.extend_from_slice(&u.to_le_bytes());
        }
        utf16_bytes.extend_from_slice(&13u16.to_le_bytes()); // para end
        let tag_id = 67u32; // HWPTAG_PARA_TEXT
        let hdr = tag_id | ((utf16_bytes.len() as u32) << 20);
        sec0.extend_from_slice(&hdr.to_le_bytes());
        sec0.extend_from_slice(&utf16_bytes);
    }

    let sec0_final = if compressed {
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&sec0).unwrap();
        enc.finish().unwrap()
    } else {
        sec0
    };

    comp.create_stream("/BodyText/Section0")
        .unwrap()
        .write_all(&sec0_final)
        .unwrap();

    comp.into_inner().into_inner()
}

// =========================================================================
// 3. Korean Hangul OWPML (.hwpx)
// =========================================================================

/// Native parser for Korean Hangul OWPML (`.hwpx`) ZIP/XML documents.
#[derive(Debug, Clone, Default)]
pub struct HwpxParser;

impl HwpxParser {
    /// Create a new [`HwpxParser`].
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Parser for HwpxParser {
    fn format(&self) -> Format {
        Format::hwpx()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        data.starts_with(b"PK\x03\x04")
            && (data.windows(21).any(|w| w == b"Contents/section0.xml")
                || data.windows(20).any(|w| w == b"Contents/content.hpf"))
    }

    async fn parse(&self, data: Bytes, _context: ParseContext) -> Result<Document> {
        parse_hwpx_bytes(&data)
    }
}

/// Parse a Korean Hangul OWPML (`.hwpx`) ZIP archive into a UDM [`Document`].
pub fn parse_hwpx_bytes(data: &[u8]) -> Result<Document> {
    let mut zip = ZipArchive::new(Cursor::new(data))
        .map_err(|e| Error::ParseError(format!("Invalid HWPX ZIP container: {e}")))?;

    let mut metadata = Metadata {
        custom: std::collections::HashMap::new(),
        ..Metadata::default()
    };
    metadata.custom.insert(
        "format".to_string(),
        MetadataValue::String("HWPX".to_string()),
    );

    let mut section_names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.starts_with("Contents/section") && n.ends_with(".xml"))
        .collect();
    section_names.sort();

    let mut pages = Vec::new();
    for (idx, sec_name) in section_names.iter().enumerate() {
        let xml_bytes = read_bounded(
            zip.by_name(sec_name)
                .map_err(|e| Error::ParseError(format!("Missing {sec_name}: {e}")))?,
            16 * 1024 * 1024,
        )?;
        let blocks = parse_owpml_section_xml(&xml_bytes);
        let mut p = make_page(idx + 1);
        p.content = blocks;
        pages.push(p);
    }

    if pages.is_empty() {
        pages.push(make_page(1));
    }

    Ok(Document {
        metadata,
        pages,
        ..Document::default()
    })
}

fn parse_owpml_section_xml(xml: &[u8]) -> Vec<ContentBlock> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    let mut blocks = Vec::new();
    let mut in_text = false;
    let mut in_table = false;
    let mut cur_para = String::new();
    let mut cur_cell = String::new();
    let mut cur_row: Vec<TableCell> = Vec::new();
    let mut table_rows: Vec<TableRow> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    "t" => in_text = true,
                    "tbl" => {
                        in_table = true;
                        table_rows.clear();
                    }
                    "tr" if in_table => {
                        cur_row.clear();
                    }
                    "tc" if in_table => {
                        cur_cell.clear();
                    }
                    "p" if !in_table => {
                        cur_para.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_text => {
                let s: &str = t.as_ref();
                if in_table {
                    cur_cell.push_str(s);
                } else {
                    cur_para.push_str(s);
                }
            }
            Ok(Event::End(e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    "t" => in_text = false,
                    "tc" if in_table => {
                        cur_row.push(make_table_cell(cur_cell.trim()));
                        cur_cell.clear();
                    }
                    "tr" if in_table => {
                        if !cur_row.is_empty() {
                            table_rows.push(TableRow {
                                cells: std::mem::take(&mut cur_row),
                                height: None,
                            });
                        }
                    }
                    "tbl" if in_table => {
                        in_table = false;
                        if !table_rows.is_empty() {
                            let col_cnt = table_rows.first().map_or(1, |r| r.cells.len().max(1));
                            let mut tbl = TableBlock::new(Rect::default(), col_cnt);
                            tbl.rows = std::mem::take(&mut table_rows);
                            blocks.push(ContentBlock::Table(tbl));
                        }
                    }
                    "p" if !in_table => {
                        let trimmed = cur_para.trim();
                        if !trimmed.is_empty() {
                            blocks.push(ContentBlock::Text(make_text_block(trimmed)));
                        }
                        cur_para.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    blocks
}

/// Build a deterministic synthetic Korean Hangul OWPML (`.hwpx`) ZIP archive for testing.
pub fn build_hwpx_bytes(paragraphs: &[&str], table_rows: &[Vec<&str>]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let opts = FileOptions::default();
        zip.start_file("mimetype", opts).unwrap();
        zip.write_all(b"application/hwp+zip").unwrap();

        zip.start_file("Contents/content.hpf", opts).unwrap();
        zip.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><opf:package/>")
            .unwrap();

        let mut sec = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><hs:sec xmlns:hs=\"http://www.hancom.co.kr/hwpml/2011/section\" xmlns:hp=\"http://www.hancom.co.kr/hwpml/2011/paragraph\">",
        );
        for p in paragraphs {
            let _ = std::fmt::Write::write_fmt(
                &mut sec,
                format_args!("<hp:p><hp:run><hp:t>{p}</hp:t></hp:run></hp:p>"),
            );
        }
        if !table_rows.is_empty() {
            sec.push_str("<hp:tbl>");
            for row in table_rows {
                sec.push_str("<hp:tr>");
                for cell in row {
                    let _ = std::fmt::Write::write_fmt(
                        &mut sec,
                        format_args!(
                            "<hp:tc><hp:p><hp:run><hp:t>{cell}</hp:t></hp:run></hp:p></hp:tc>"
                        ),
                    );
                }
                sec.push_str("</hp:tr>");
            }
            sec.push_str("</hp:tbl>");
        }
        sec.push_str("</hs:sec>");

        zip.start_file("Contents/section0.xml", opts).unwrap();
        zip.write_all(sec.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    cursor.into_inner()
}
