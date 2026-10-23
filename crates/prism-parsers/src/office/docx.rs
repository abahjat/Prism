// SPDX-License-Identifier: AGPL-3.0-only
//! DOCX (Microsoft Word) parser
//!
//! Parses DOCX files into the Unified Document Model with high fidelity.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, FontResource, Page, PageMargins, PageMetadata, Rect,
        TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::io::Cursor;
use tracing::{debug, warn};
use zip::ZipArchive;

use crate::office::relationships::Relationships;
use crate::office::styles::Styles;
use crate::office::tables;
use crate::office::theme::{Theme, parse_theme};
use crate::office::utils;

/// English Metric Units per point
const EMU_PER_POINT: f64 = 12_700.0;

/// Color used to show tracked changes (as Word's "All Markup" view does)
const REVISION_COLOR: &str = "#C00000";

/// Run property elements (children of `w:rPr`) handled by [`apply_run_property`]
macro_rules! run_property {
    () => {
        b"w:b" | b"w:i" | b"w:u" | b"w:strike" | b"w:dstrike" | b"w:color" | b"w:sz" | b"w:rFonts"
    };
}

/// Explicit on/off run properties from direct formatting.
///
/// Kept apart from `TextStyle` so that an explicit "off" (`<w:b w:val="0"/>`)
/// overrides a paragraph style that turns the property on.
#[derive(Debug, Default, Clone, Copy)]
struct RunToggles {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
}

impl RunToggles {
    fn apply(self, style: &mut TextStyle) {
        if let Some(on) = self.bold {
            style.bold = on;
        }
        if let Some(on) = self.italic {
            style.italic = on;
        }
        if let Some(on) = self.underline {
            style.underline = on;
        }
        if let Some(on) = self.strike {
            style.strikethrough = on;
        }
    }
}

/// Tracked change (`w:ins`/`w:del`/`w:moveTo`/`w:moveFrom`) a run belongs to
#[derive(Debug, Clone, Copy)]
enum Revision {
    Inserted,
    Deleted,
}

impl Revision {
    /// Mark the run so a reviewer can tell inserted and deleted text apart
    fn apply(self, style: &mut TextStyle) {
        style.color = Some(REVISION_COLOR.to_string());
        match self {
            Self::Inserted => style.underline = true,
            Self::Deleted => style.strikethrough = true,
        }
    }
}

/// Apply one run property element to the current run's direct formatting
fn apply_run_property(
    e: &BytesStart<'_>,
    theme: Option<&Theme>,
    style: &mut TextStyle,
    toggles: &mut RunToggles,
) {
    match e.name().as_ref().as_bytes() {
        b"w:b" => toggles.bold = Some(utils::on_off_value(e)),
        b"w:i" => toggles.italic = Some(utils::on_off_value(e)),
        b"w:u" => toggles.underline = Some(utils::on_off_value(e)),
        b"w:strike" | b"w:dstrike" => toggles.strike = Some(utils::on_off_value(e)),
        b"w:color" => {
            let val = utils::attr_value_opt(e, b"w:val");
            let theme_color = utils::attr_value_opt(e, b"w:themeColor");
            // themeTint/themeShade are hex bytes (ST_UcharHexNumber)
            let hex_byte = |key: &[u8]| {
                utils::attr_value_opt(e, key)
                    .and_then(|v| u8::from_str_radix(&v, 16).ok())
                    .map(f64::from)
            };
            let resolved = theme.and_then(|t| {
                utils::resolve_word_color(
                    val.as_deref(),
                    theme_color.as_deref(),
                    hex_byte(b"w:themeTint"),
                    hex_byte(b"w:themeShade"),
                    t,
                )
            });
            if let Some(color) =
                resolved.or_else(|| val.filter(|v| v != "auto").map(|v| format!("#{v}")))
            {
                style.color = Some(color);
            }
        }
        b"w:sz" => {
            if let Some(half_points) =
                utils::attr_value_opt(e, b"w:val").and_then(|v| v.parse::<f64>().ok())
            {
                style.font_size = Some(half_points / 2.0);
            }
        }
        b"w:rFonts" => {
            if let Some(font) = utils::attr_value_opt(e, b"w:ascii")
                .or_else(|| utils::attr_value_opt(e, b"w:hAnsi"))
            {
                style.font_family = Some(font);
            }
        }
        _ => {}
    }
}

/// Page size of the document's final section (`w:body/w:sectPr/w:pgSz`), in points
fn section_page_size(document_xml: &str) -> Option<Dimensions> {
    let mut reader = Reader::from_str(document_xml);
    let mut buf = Vec::new();
    let mut size = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) if e.name().as_ref().as_bytes() == b"w:pgSz" => {
                // Sizes are in twentieths of a point
                let twips =
                    |key: &[u8]| utils::attr_value_opt(&e, key).and_then(|v| v.parse::<f64>().ok());
                if let (Some(w), Some(h)) = (twips(b"w:w"), twips(b"w:h"))
                    && w > 0.0
                    && h > 0.0
                {
                    size = Some(Dimensions::new(w / 20.0, h / 20.0));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    size
}

/// Page margins of the document's final section (`w:body/w:sectPr/w:pgMar`), in points
fn section_page_margins(document_xml: &str) -> Option<PageMargins> {
    let mut reader = Reader::from_str(document_xml);
    let mut buf = Vec::new();
    let mut margins = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) if e.name().as_ref().as_bytes() == b"w:pgMar" => {
                let twips = |key: &[u8]| {
                    utils::attr_value_opt(&e, key)
                        .and_then(|v| v.parse::<f64>().ok())
                        .map(|t| (t / 20.0).max(0.0))
                };
                let top = twips(b"w:top").unwrap_or(72.0);
                let right = twips(b"w:right").unwrap_or(72.0);
                let bottom = twips(b"w:bottom").unwrap_or(72.0);
                let left = twips(b"w:left").unwrap_or(72.0);
                margins = Some(PageMargins::new(top, right, bottom, left));
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    margins
}

/// Deobfuscate an OOXML `.odttf` font stream using its 16-byte GUID key (ECMA-376 §17.8.1).
fn deobfuscate_odttf_bytes(data: &[u8], guid: &str) -> Option<Vec<u8>> {
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

/// Extract embedded fonts declared in `word/fontTable.xml` and `word/_rels/fontTable.xml.rels`.
fn extract_embedded_fonts(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    limit: usize,
) -> Vec<FontResource> {
    let mut font_rels = Relationships::new();
    if let Ok(mut file) = archive.by_name("word/_rels/fontTable.xml.rels") {
        let mut xml = String::new();
        if crate::limits::read_to_string_with_limit(&mut file, &mut xml, limit).is_ok()
            && let Ok(r) = Relationships::from_xml(&xml)
        {
            font_rels = r;
        }
    }
    if font_rels.map.is_empty() {
        return Vec::new();
    }

    let mut font_table_xml = String::new();
    let Ok(mut ft_file) = archive.by_name("word/fontTable.xml") else {
        return Vec::new();
    };
    if crate::limits::read_to_string_with_limit(&mut ft_file, &mut font_table_xml, limit).is_err() {
        return Vec::new();
    }
    drop(ft_file);

    // Collect (family_name, style_label, r_id, font_key)
    let mut specs: Vec<(String, &'static str, String, Option<String>)> = Vec::new();
    let mut reader = Reader::from_str(&font_table_xml);
    let mut buf = Vec::new();
    let mut current_family: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                let name = e.name();
                match name.as_ref().as_bytes() {
                    b"w:font" => {
                        current_family = utils::attr_value_opt(&e, b"w:name");
                    }
                    b"w:embedRegular" | b"w:embedBold" | b"w:embedItalic"
                    | b"w:embedBoldItalic" => {
                        if let Some(ref fam) = current_family
                            && let Some(r_id) = utils::attr_value_opt(&e, b"r:id")
                        {
                            let style = match name.as_ref().as_bytes() {
                                b"w:embedBold" => "Bold",
                                b"w:embedItalic" => "Italic",
                                b"w:embedBoldItalic" => "BoldItalic",
                                _ => "Regular",
                            };
                            let font_key = utils::attr_value_opt(&e, b"w:fontKey");
                            specs.push((fam.clone(), style, r_id, font_key));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) if e.name().as_ref().as_bytes() == b"w:font" => {
                current_family = None;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let mut resources = Vec::new();
    for (family, style, r_id, font_key) in specs {
        let Some(rel) = font_rels.get(&r_id) else {
            continue;
        };
        let target = rel.target.trim_start_matches('/');
        let zip_path = if target.starts_with("word/") {
            target.to_string()
        } else {
            format!("word/{target}")
        };
        let Ok(mut font_file) = archive.by_name(&zip_path) else {
            continue;
        };
        let mut raw = Vec::new();
        if crate::limits::read_to_end_with_limit(&mut font_file, &mut raw, limit).is_err() {
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

/// MIME type of an embedded image, detected from its content
fn image_mime_type(data: &[u8], path: &str) -> String {
    prism_core::format::detect_format(data, Some(path))
        .map(|detection| detection.format.mime_type)
        .filter(|mime| mime.starts_with("image/"))
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

/// Pixel dimensions of an embedded image, or (0, 0) if unknown
fn image_pixel_size(data: &[u8]) -> (u32, u32) {
    image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .unwrap_or((0, 0))
}

/// DOCX parser
#[derive(Debug, Clone)]
pub struct DocxParser;

impl DocxParser {
    /// Create a new DOCX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data is a valid DOCX file (ZIP with word/document.xml)
    #[must_use]
    fn is_docx_zip(data: &[u8]) -> bool {
        if data.len() < 4 || &data[0..2] != b"PK" {
            return false;
        }

        let cursor = std::io::Cursor::new(data);
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
}

impl Default for DocxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for DocxParser {
    fn format(&self) -> Format {
        Format::docx()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_docx_zip(data)
    }

    #[allow(clippy::too_many_lines)]
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        const PARAS_PER_PAGE: usize = 50;
        debug!("Parsing DOCX file: {:?}", context.filename);

        let limit = crate::limits::max_decompressed_bytes(&context);
        let cursor = Cursor::new(data.as_ref());
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open DOCX ZIP: {e}")))?;

        // 1. Parse Relationships
        let mut _rels = Relationships::new();
        if let Ok(mut file) = archive.by_name("word/_rels/document.xml.rels") {
            let mut xml = String::new();
            crate::limits::read_to_string_with_limit(&mut file, &mut xml, limit)?;
            if let Ok(r) = Relationships::from_xml(&xml) {
                _rels = r;
            }
        }

        // 2. Parse Theme (Start with Default)
        let mut theme: Option<Theme> = None;
        if let Ok(mut file) = archive.by_name("word/theme/theme1.xml") {
            let mut xml = Vec::new(); // Theme parser expects bytes, usually
            crate::limits::read_to_end_with_limit(&mut file, &mut xml, limit)?;
            if let Ok(t) = parse_theme(&xml) {
                theme = Some(t);
            }
        }

        // 3. Parse Styles
        let mut styles = Styles::new();
        if let Ok(mut file) = archive.by_name("word/styles.xml") {
            let mut xml = String::new();
            crate::limits::read_to_string_with_limit(&mut file, &mut xml, limit)?;
            if let Ok(s) = Styles::from_xml(&xml, theme.as_ref()) {
                styles = s;
            }
        }

        // 3. Parse Document Content
        let mut document_xml = String::new();
        match archive.by_name("word/document.xml") {
            Ok(mut file) => {
                crate::limits::read_to_string_with_limit(&mut file, &mut document_xml, limit)?;
            }
            Err(_) => return Err(Error::ParseError("word/document.xml not found".to_string())),
        }

        // Streaming Parse of Document XML
        let mut reader = Reader::from_str(&document_xml);
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();

        let mut pages = Vec::new();
        let mut current_page_content = Vec::new();
        let mut images_found: Vec<prism_core::document::ImageResource> = Vec::new();

        // State for paragraph parsing
        let mut in_paragraph = false;
        let mut current_paragraph_runs = Vec::new();
        let mut current_paragraph_style: Option<String> = None;

        // State for run parsing
        let mut in_run = false;
        let mut current_run_text = String::new();
        let mut current_run_style = TextStyle::default();
        let mut run_toggles = RunToggles::default();
        let mut in_run_props = false;
        // Only w:t / w:delText carry document text (w:instrText holds field codes)
        let mut in_text = false;
        let mut revision: Option<Revision> = None;

        // State for drawing parsing
        let mut in_drawing = false;
        let mut current_drawing_image_id: Option<String> = None;
        let mut current_drawing_extent: Option<Dimensions> = None;
        let mut current_drawing_alt: Option<String> = None;

        let page_size = section_page_size(&document_xml).unwrap_or(Dimensions::LETTER);
        let page_margins = section_page_margins(&document_xml);

        // Count paragraphs for approximate pagination
        let mut para_count = 0;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = e.name();
                    match name.as_ref().as_bytes() {
                        b"w:p" => {
                            in_paragraph = true;
                            current_paragraph_runs.clear();
                            current_paragraph_style = None;
                            para_count += 1;
                        }
                        b"w:pStyle" => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref().as_bytes() == b"w:val" {
                                    current_paragraph_style = Some(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        b"w:r" => {
                            if in_paragraph {
                                in_run = true;
                                current_run_text.clear();
                                current_run_style = TextStyle::default();
                                run_toggles = RunToggles::default();
                            }
                        }
                        b"w:rPr" => in_run_props = true,
                        // Run Properties
                        run_property!() if in_run_props => apply_run_property(
                            &e,
                            theme.as_ref(),
                            &mut current_run_style,
                            &mut run_toggles,
                        ),
                        b"w:t" | b"w:delText" => in_text = true,
                        b"w:ins" | b"w:moveTo" if !in_run_props => {
                            revision = Some(Revision::Inserted);
                        }
                        b"w:del" | b"w:moveFrom" if !in_run_props => {
                            revision = Some(Revision::Deleted);
                        }
                        b"w:drawing" => {
                            in_drawing = true;
                            current_drawing_image_id = None;
                            current_drawing_extent = None;
                            current_drawing_alt = None;
                        }
                        b"a:blip" if in_drawing => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref().as_bytes() == b"r:embed" {
                                    current_drawing_image_id = Some(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        b"w:tbl" => {
                            // Delegate to table parser
                            // Note: parse_table expects we just consumed <w:tbl>
                            match tables::parse_table(&mut reader) {
                                Ok(table_block) => {
                                    current_page_content.push(ContentBlock::Table(table_block));
                                }
                                Err(e) => warn!("Failed to parse table: {e}"),
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(e)) => {
                    // Handle empty tags like <w:b/>
                    let name = e.name();
                    match name.as_ref().as_bytes() {
                        run_property!() if in_run_props => apply_run_property(
                            &e,
                            theme.as_ref(),
                            &mut current_run_style,
                            &mut run_toggles,
                        ),
                        b"w:tab" if in_run && !in_run_props => current_run_text.push('\t'),
                        b"w:br" | b"w:cr" if in_run => current_run_text.push('\n'),
                        b"wp:extent" if in_drawing => {
                            let emu = |key: &[u8]| {
                                utils::attr_value_opt(&e, key).and_then(|v| v.parse::<f64>().ok())
                            };
                            if let (Some(cx), Some(cy)) = (emu(b"cx"), emu(b"cy")) {
                                current_drawing_extent =
                                    Some(Dimensions::new(cx / EMU_PER_POINT, cy / EMU_PER_POINT));
                            }
                        }
                        b"wp:docPr" if in_drawing => {
                            current_drawing_alt = utils::attr_value_opt(&e, b"descr")
                                .filter(|d| !d.is_empty())
                                .or_else(|| utils::attr_value_opt(&e, b"title"));
                        }
                        b"w:pStyle" => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref().as_bytes() == b"w:val" {
                                    current_paragraph_style = Some(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        b"a:blip" if in_drawing => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref().as_bytes() == b"r:embed" {
                                    current_drawing_image_id = Some(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(e)) => {
                    match e.name().as_ref().as_bytes() {
                        b"w:p" => {
                            if !current_paragraph_runs.is_empty() {
                                let block = TextBlock {
                                    runs: current_paragraph_runs.clone(),
                                    paragraph_style: current_paragraph_style.clone(),
                                    bounds: Rect::default(),
                                    vertical_alignment: None,
                                    style: prism_core::document::ShapeStyle::default(),
                                    rotation: 0.0,
                                };
                                current_page_content.push(ContentBlock::Text(block));

                                #[allow(clippy::cast_possible_truncation)]
                                if para_count >= PARAS_PER_PAGE {
                                    pages.push(Page {
                                        number: (pages.len() + 1) as u32,
                                        dimensions: page_size,
                                        content: current_page_content.clone(),
                                        annotations: Vec::new(),
                                        metadata: PageMetadata {
                                            margins: page_margins,
                                            ..PageMetadata::default()
                                        },
                                    });
                                    current_page_content.clear();
                                    para_count = 0;
                                }
                            }
                            in_paragraph = false;
                        }
                        b"w:r" => {
                            if !current_run_text.is_empty() {
                                // Resolve style against global styles if needed
                                let mut effective_style = styles.resolve_text_style(
                                    current_paragraph_style.as_deref(),
                                    &current_run_style,
                                );
                                run_toggles.apply(&mut effective_style);
                                if let Some(revision) = revision {
                                    revision.apply(&mut effective_style);
                                }

                                current_paragraph_runs.push(TextRun {
                                    text: current_run_text.clone(),
                                    style: effective_style,
                                    bounds: None,
                                    char_positions: None,
                                });
                            }
                            in_run = false;
                        }
                        b"w:rPr" => in_run_props = false,
                        b"w:t" | b"w:delText" => in_text = false,
                        b"w:ins" | b"w:del" | b"w:moveTo" | b"w:moveFrom" if !in_run_props => {
                            revision = None;
                        }
                        b"w:drawing" => {
                            in_drawing = false;
                            if let Some(r_id) = current_drawing_image_id.take()
                                && let Some(rel) = _rels.get(&r_id)
                            {
                                // Construct path in zip
                                let mut path = rel.target.clone();
                                if path.starts_with('/') {
                                    path = path.trim_start_matches('/').to_string();
                                } else {
                                    path = format!("word/{path}");
                                }

                                if let Ok(mut image_file) = archive.by_name(&path) {
                                    let mut image_data = Vec::new();
                                    crate::limits::read_to_end_with_limit(
                                        &mut image_file,
                                        &mut image_data,
                                        limit,
                                    )?;
                                    let (width, height) = image_pixel_size(&image_data);
                                    let image_resource = prism_core::document::ImageResource {
                                        id: r_id.clone(),
                                        mime_type: image_mime_type(&image_data, &path),
                                        data: Some(image_data),
                                        url: None,
                                        width,
                                        height,
                                    };

                                    images_found.push(image_resource);

                                    let image_block = prism_core::document::ImageBlock {
                                        bounds: Rect::default(),
                                        resource_id: r_id.clone(),
                                        alt_text: current_drawing_alt.take(),
                                        format: None,
                                        original_size: current_drawing_extent.take(),
                                        style: prism_core::document::ShapeStyle::default(),
                                        rotation: 0.0,
                                    };
                                    current_page_content.push(ContentBlock::Image(image_block));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Text(e)) => {
                    if in_run && in_text {
                        current_run_text.push_str(e.as_ref());
                    }
                }
                Ok(Event::GeneralRef(e)) => {
                    if in_run
                        && in_text
                        && let Some(resolved) = utils::resolve_general_ref(&e)
                    {
                        current_run_text.push_str(&resolved);
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    warn!("XML error: {e}");
                    break;
                }
                _ => {}
            }
            buf.clear();
        }

        // Add final page
        if !current_page_content.is_empty() {
            #[allow(clippy::cast_possible_truncation)]
            pages.push(Page {
                number: (pages.len() + 1) as u32,
                dimensions: page_size,
                content: current_page_content,
                annotations: Vec::new(),
                metadata: PageMetadata {
                    margins: page_margins,
                    ..PageMetadata::default()
                },
            });
        }

        // Ensure at least one page
        if pages.is_empty() {
            let mut p = Page::new(1, page_size);
            p.metadata.margins = page_margins;
            pages.push(p);
        }

        let embedded_fonts = extract_embedded_fonts(&mut archive, limit);

        let mut metadata = Metadata::new();
        if let Some(filename) = context.filename {
            metadata.title = Some(filename);
        }
        metadata.add_custom("format", "DOCX");

        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;
        document.structure.headings = Vec::new(); // TODO: Extract headings from structure

        document.resources.images = images_found;
        document.resources.fonts = embedded_fonts;

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "DOCX Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::document::ImageBlock;
    use std::io::Write;

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    fn build_docx(body: &str, styles: Option<&str>, extra: &[(&str, &[u8])]) -> Bytes {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::FileOptions::default();
        let document = format!(
            r#"<?xml version="1.0"?><w:document {NS}><w:body>{body}</w:body></w:document>"#
        );
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document.as_bytes()).unwrap();
        if let Some(styles) = styles {
            zip.start_file("word/styles.xml", options).unwrap();
            zip.write_all(
                format!(r#"<?xml version="1.0"?><w:styles {NS}>{styles}</w:styles>"#).as_bytes(),
            )
            .unwrap();
        }
        for (name, data) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        Bytes::from(zip.finish().unwrap().into_inner())
    }

    async fn parse(data: Bytes) -> Document {
        let context = ParseContext {
            format: Format::docx(),
            filename: Some("test.docx".to_string()),
            size: data.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        DocxParser::new().parse(data, context).await.unwrap()
    }

    fn runs(document: &Document) -> Vec<&TextRun> {
        document.pages[0]
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(&text.runs),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[tokio::test]
    async fn explicit_off_overrides_style_and_empty_properties_apply() {
        let styles =
            r#"<w:style w:type="paragraph" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>"#;
        let body = r#"<w:p><w:pPr><w:pStyle w:val="Strong"/></w:pPr>
            <w:r><w:t>bold </w:t></w:r>
            <w:r><w:rPr><w:b w:val="0"/><w:sz w:val="28"/><w:rFonts w:ascii="Georgia"/><w:color w:val="FF0000"/></w:rPr><w:t>plain</w:t></w:r>
            <w:r><w:rPr><w:u w:val="none"/><w:i w:val="false"/></w:rPr><w:t>x</w:t></w:r>
        </w:p>"#;
        let document = parse(build_docx(body, Some(styles), &[])).await;
        let runs = runs(&document);
        assert!(runs[0].style.bold);
        assert!(!runs[1].style.bold, "w:val=0 must turn bold off");
        assert_eq!(runs[1].style.font_size, Some(14.0));
        assert_eq!(runs[1].style.font_family.as_deref(), Some("Georgia"));
        assert_eq!(runs[1].style.color.as_deref(), Some("#FF0000"));
        assert!(!runs[2].style.underline && !runs[2].style.italic);
    }

    #[tokio::test]
    async fn field_codes_hidden_and_revisions_marked() {
        let body = r#"<w:p>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r>
            <w:r><w:instrText> HYPERLINK "https://example.com" </w:instrText></w:r>
            <w:r><w:fldChar w:fldCharType="separate"/></w:r>
            <w:r><w:t>link</w:t></w:r>
            <w:r><w:fldChar w:fldCharType="end"/></w:r>
            <w:ins w:id="1" w:author="A"><w:r><w:t>added</w:t></w:r></w:ins>
            <w:del w:id="2" w:author="A"><w:r><w:delText>removed</w:delText></w:r></w:del>
            <w:r><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r>
        </w:p>"#;
        let document = parse(build_docx(body, None, &[])).await;
        assert_eq!(document.extract_text(), "linkaddedremoveda\tb");
        let runs = runs(&document);
        let added = runs.iter().find(|r| r.text == "added").unwrap();
        let removed = runs.iter().find(|r| r.text == "removed").unwrap();
        let normal = runs.iter().find(|r| r.text == "link").unwrap();
        assert!(added.style.underline && !added.style.strikethrough);
        assert!(removed.style.strikethrough);
        assert!(!normal.style.underline && !normal.style.strikethrough);
    }

    #[tokio::test]
    async fn page_size_from_final_section() {
        let body = r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>
            <w:sectPr><w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/><w:pgMar w:top="1440" w:right="1080" w:bottom="1440" w:left="1080"/></w:sectPr>"#;
        let document = parse(build_docx(body, None, &[])).await;
        let dims = document.pages[0].dimensions;
        assert!((dims.width - 841.9).abs() < 0.1 && (dims.height - 595.3).abs() < 0.1);
        let margins = document.pages[0].metadata.margins.expect("w:pgMar margins");
        assert!((margins.top - 72.0).abs() < 0.1);
        assert!((margins.bottom - 72.0).abs() < 0.1);
        assert!((margins.left - 54.0).abs() < 0.1);
        assert!((margins.right - 54.0).abs() < 0.1);
    }

    #[tokio::test]
    async fn image_type_and_extent() {
        let mut png = Vec::new();
        image::RgbImage::new(4, 3)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
        let body = r#"<w:p><w:r><w:drawing><wp:inline>
            <wp:extent cx="914400" cy="457200"/><wp:docPr id="1" name="Picture 1" descr="Company logo"/>
            <a:graphic><a:graphicData><a:blip r:embed="rId5"/></a:graphicData></a:graphic>
        </wp:inline></w:drawing></w:r></w:p>"#;
        let document = parse(build_docx(
            body,
            None,
            &[
                ("word/_rels/document.xml.rels", rels),
                ("word/media/image1.png", &png),
            ],
        ))
        .await;
        let resource = &document.resources.images[0];
        assert_eq!(resource.mime_type, "image/png");
        assert_eq!((resource.width, resource.height), (4, 3));
        let image: &ImageBlock = document.pages[0]
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::Image(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let size = image.original_size.unwrap();
        assert!((size.width - 72.0).abs() < 0.01 && (size.height - 36.0).abs() < 0.01);
        assert_eq!(image.alt_text.as_deref(), Some("Company logo"));
    }

    #[tokio::test]
    async fn zip_entry_decompression_bomb_rejected() {
        let large_body = format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", "x".repeat(64 * 1024));
        let data = build_docx(&large_body, None, &[]);
        let context = ParseContext {
            format: Format::docx(),
            filename: Some("bomb.docx".to_string()),
            size: data.len(),
            options: prism_core::parser::ParseOptions {
                max_memory: Some(4096),
                ..prism_core::parser::ParseOptions::default()
            },
        };
        let err = DocxParser::new().parse(data, context).await.unwrap_err();
        assert!(
            matches!(err, Error::MemoryLimitExceeded { limit: 4096, .. }),
            "expected MemoryLimitExceeded, got {err:?}"
        );
    }
}
