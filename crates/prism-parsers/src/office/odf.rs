// SPDX-License-Identifier: AGPL-3.0-only
//! `OpenDocument` format parsers (ODT, ODS, ODP)
//!
//! Parses `OpenDocument` files (used by `LibreOffice`, `OpenOffice`) into the
//! Unified Document Model. These formats are ZIP archives containing XML.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, ImageBlock, ImageResource, Page, Rect, ShapeStyle,
        TableBlock, TableCell, TableRow, TextBlock, TextRun, TextStyle, VerticalAlignment,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use tracing::{debug, warn};
use zip::ZipArchive;

/// ODT (`OpenDocument` Text) parser
#[derive(Debug, Clone)]
pub struct OdtParser;

impl OdtParser {
    /// Create a new ODT parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for OdtParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for OdtParser {
    fn format(&self) -> Format {
        Format::odt()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        is_odf_zip(data, "application/vnd.oasis.opendocument.text")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing ODT file: {:?}", context.filename);
        parse_odf_document(data, "ODT", context)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "ODT Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// ODS (`OpenDocument` Spreadsheet) parser
#[derive(Debug, Clone)]
pub struct OdsParser;

impl OdsParser {
    /// Create a new ODS parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for OdsParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for OdsParser {
    fn format(&self) -> Format {
        Format::ods()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        is_odf_zip(data, "application/vnd.oasis.opendocument.spreadsheet")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing ODS file: {:?}", context.filename);
        parse_odf_spreadsheet(data, context)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "ODS Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// ODP (`OpenDocument` Presentation) parser
#[derive(Debug, Clone)]
pub struct OdpParser {
    format: Format,
}

impl OdpParser {
    /// Create a new ODP parser
    #[must_use]
    pub fn new() -> Self {
        Self {
            format: Format::odp(),
        }
    }

    /// Create a new ODP parser with a specific format
    #[must_use]
    pub fn new_with_format(format: Format) -> Self {
        Self { format }
    }
}

impl Default for OdpParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for OdpParser {
    fn format(&self) -> Format {
        self.format.clone()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        is_odf_zip(data, "application/vnd.oasis.opendocument.presentation")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing ODP file: {:?}", context.filename);
        parse_odf_document(data, "ODP", context)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "ODP Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// ODG (`OpenDocument` Graphics) parser
#[derive(Debug, Clone)]
pub struct OdgParser;

impl OdgParser {
    /// Create a new ODG parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for OdgParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for OdgParser {
    fn format(&self) -> Format {
        Format::odg()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        is_odf_zip(data, "application/vnd.oasis.opendocument.graphics")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing ODG file: {:?}", context.filename);
        parse_odf_document(data, "ODG", context)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "ODG Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// Check if data is a ZIP file with ODF mimetype
fn is_odf_zip(data: &[u8], expected_mimetype: &str) -> bool {
    // Check ZIP signature first
    if data.len() < 4 || &data[0..4] != b"PK\x03\x04" {
        return false;
    }

    // Try to read the mimetype file from the ZIP
    let cursor = Cursor::new(data);
    if let Ok(mut archive) = ZipArchive::new(cursor) {
        if let Ok(mut mimetype_file) = archive.by_name("mimetype") {
            let mut mimetype = String::new();
            if mimetype_file.read_to_string(&mut mimetype).is_ok() {
                return mimetype.trim() == expected_mimetype;
            }
        }
    }
    false
}

/// ODF Style properties
#[derive(Debug, Default, Clone)]
struct OdfStyle {
    // Shape properties
    fill_color: Option<String>,
    stroke_color: Option<String>,
    stroke_width: Option<f64>,

    // Text properties
    font_size: Option<f64>,
    font_color: Option<String>,
    bold: bool,
    italic: bool,
    underline: bool,

    // Paragraph properties
    vertical_alignment: Option<VerticalAlignment>,

    // Page properties
    page_background_color: Option<String>,
    page_background_image: Option<String>, // Name of the fill-image
}

type StyleMap = HashMap<String, OdfStyle>;

/// Parse an ODF text or presentation document
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn parse_odf_document(data: Bytes, format_name: &str, context: ParseContext) -> Result<Document> {
    let cursor = Cursor::new(&data);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| Error::ParseError(format!("Invalid ZIP: {e}")))?;

    // Read content.xml
    let content_xml = read_zip_file(&mut archive, "content.xml")?;

    // Phase 1: Parse Styles
    // In a real implementation, we would extract office:automatic-styles from content.xml
    // and potentially styles.xml. For now, we'll parse them from content.xml.
    let styles = parse_automatic_styles(&content_xml);

    // Phase 2: Parse Content
    // For ODP/ODG, we want to parse pages/slides. For ODT, likely just text.
    // However, to unify and support vertical alignment everywhere, we will iterate
    // pages if present (draw:page), or fallback to simple processing for ODT text body.

    let is_presentation = format_name == "ODP" || format_name == "ODG";
    let mut document = Document::new();

    if is_presentation {
        // Extract fill images from styles.xml if available
        let mut fill_images = HashMap::new();
        if let Ok(styles_xml) = read_zip_file(&mut archive, "styles.xml") {
            fill_images = parse_fill_images(&styles_xml);
        }

        let pages = parse_presentation_content(&content_xml, &styles, &fill_images);

        // Collect required resources (images)
        let mut resource_ids = Vec::new();
        for p in &pages {
            for block in &p.content {
                if let ContentBlock::Image(img) = block {
                    resource_ids.push(img.resource_id.clone());
                }
            }
        }

        // Extract resources from ZIP
        for id in resource_ids {
            // Check if already loaded
            if document.resources.images.iter().any(|r| r.id == id) {
                continue;
            }

            if let Ok(data) = read_zip_file_bytes(&mut archive, &id) {
                let mime_type = match std::path::Path::new(&id)
                    .extension()
                    .and_then(|e| e.to_str())
                {
                    Some("png") => "image/png",
                    Some("jpg" | "jpeg") => "image/jpeg",
                    Some("svg") => "image/svg+xml",
                    _ => "application/octet-stream",
                }
                .to_string();

                document.resources.images.push(ImageResource {
                    id: id.clone(),
                    mime_type,
                    data: Some(data),
                    url: None,
                    width: 0,
                    height: 0,
                });
            }
        }

        for p in pages {
            document.pages.push(p);
        }
    } else {
        // Fallback for ODT: existing logic or similar
        // For ODT, it's typically info inside office:text
        // We'll keep the old logic for non-presentation formats for safety/regression avoidance for now,
        // unless requested to upgrade ODT too. The request focused on "PowerPoint types".
        let text_content = extract_text_from_odf_xml(&content_xml)?;
        let paragraphs: Vec<&str> = text_content.split('\n').collect();
        let mut current_page_text = String::new();
        let mut page_num: u32 = 1;

        for para in paragraphs {
            if current_page_text.len() + para.len() > 2000 && !current_page_text.is_empty() {
                document
                    .pages
                    .push(create_text_page(&current_page_text, page_num));
                current_page_text.clear();
                page_num += 1;
            }
            if !current_page_text.is_empty() {
                current_page_text.push('\n');
            }
            current_page_text.push_str(para);
        }
        if !current_page_text.is_empty() {
            document
                .pages
                .push(create_text_page(&current_page_text, page_num));
        }
    }

    // If no content, add empty page
    if document.pages.is_empty() {
        document.pages.push(Page::new(1, Dimensions::LETTER));
    }

    // Set metadata
    let mut metadata = Metadata::default();
    if let Some(ref filename) = context.filename {
        metadata.title = Some(filename.clone());
    }
    metadata.add_custom("format", format_name);
    document.metadata = metadata;

    Ok(document)
}

/// Parse `office:styles` (or global styles) to extract `draw:fill-image` definitions.
/// Returns a map of image name -> xlink:href
fn parse_fill_images(xml: &str) -> HashMap<String, String> {
    let mut images = HashMap::new();
    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) => {
                if e.name().as_ref() == b"draw:fill-image" {
                    let mut name = String::new();
                    let mut href = String::new();
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"draw:name" => {
                                if let Ok(v) = std::str::from_utf8(&attr.value) {
                                    name = v.to_string();
                                }
                            }
                            k if k.ends_with(b"href") => {
                                if let Ok(v) = std::str::from_utf8(&attr.value) {
                                    href = v.to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                    if !name.is_empty() && !href.is_empty() {
                        images.insert(name, href);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    images
}

/// Parse `office:automatic-styles` to extract style properties
#[allow(clippy::too_many_lines)]
#[allow(unreachable_patterns)]
fn parse_automatic_styles(xml: &str) -> StyleMap {
    let mut styles = HashMap::new();
    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);
    let mut buf = Vec::new();

    let mut current_style_name = String::new();
    let mut current_style = OdfStyle::default();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"style:style" => {
                    current_style_name.clear();
                    current_style = OdfStyle::default(); // Reset
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"style:name" {
                            if let Ok(val) = std::str::from_utf8(&attr.value) {
                                current_style_name = val.to_string();
                            }
                        }
                    }
                }
                b"style:graphic-properties" => {
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"draw:fill-color" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    current_style.fill_color = Some(val.to_string());
                                }
                            }
                            b"svg:stroke-color" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    current_style.stroke_color = Some(val.to_string());
                                }
                            }
                            b"svg:stroke-width" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    current_style.stroke_width = Some(parse_measure(val));
                                }
                            }
                            b"draw:textarea-vertical-align" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    current_style.vertical_alignment = Some(match val {
                                        "bottom" => VerticalAlignment::Bottom,
                                        "middle" | "center" => VerticalAlignment::Center,
                                        "justify" => VerticalAlignment::Justify,
                                        _ => VerticalAlignment::Top,
                                    });
                                }
                            }
                            b"draw:fill-image-name" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    current_style.page_background_image = Some(val.to_string());
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"style:text-properties" => {
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"fo:font-size" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    current_style.font_size = Some(parse_measure(val));
                                }
                            }
                            b"fo:color" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    current_style.font_color = Some(val.to_string());
                                }
                            }
                            b"fo:font-weight" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    if val == "bold" {
                                        current_style.bold = true;
                                    }
                                }
                            }
                            b"fo:font-style" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    if val == "italic" {
                                        current_style.italic = true;
                                    }
                                }
                            }
                            b"style:text-underline-style" => {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    if val == "solid" {
                                        current_style.underline = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"style:paragraph-properties" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"draw:textarea-vertical-align" {
                            if let Ok(val) = std::str::from_utf8(&attr.value) {
                                current_style.vertical_alignment = Some(match val {
                                    "bottom" => VerticalAlignment::Bottom,
                                    "middle" | "center" => VerticalAlignment::Center,
                                    "justify" => VerticalAlignment::Justify,
                                    _ => VerticalAlignment::Top,
                                });
                            }
                        }
                    }
                }
                b"style:drawing-page-properties" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"draw:fill-color" {
                            if let Ok(val) = std::str::from_utf8(&attr.value) {
                                current_style.page_background_color = Some(val.to_string());
                            }
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(ref e)) => {
                match e.name().as_ref() {
                    b"style:graphic-properties" => {
                        if !current_style_name.is_empty() {
                            // We might have an existing style from Start tag of style:style?
                            // Yes, `current_style` is active. But wait, `style:style` is Start.
                            // `style:graphic-properties` is Empty inside that.
                            // The logic in Event::Start created a NEW style only if it wasn't checking current?
                            // In Event::Start: current_style is reset at style:style start.
                            // In Event::Start -> style:graphic-properties: it updates current_style.
                            // Here in Event::Empty -> style:graphic-properties: it should update current_style!
                            // The previous code for Empty was CREATING a new style and inserting it separately, which logic was flawed for keeping all props together.
                            // It should update `current_style`.

                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    b"draw:fill-color" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.fill_color = Some(val.to_string());
                                        }
                                    }
                                    b"svg:stroke-color" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.stroke_color = Some(val.to_string());
                                        }
                                    }
                                    b"svg:stroke-width" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.stroke_width = Some(parse_measure(val));
                                        }
                                    }
                                    b"draw:textarea-vertical-align" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.vertical_alignment = Some(match val {
                                                "bottom" => VerticalAlignment::Bottom,
                                                "middle" | "center" => VerticalAlignment::Center,
                                                "justify" => VerticalAlignment::Justify,
                                                _ => VerticalAlignment::Top,
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    b"style:text-properties" => {
                        if !current_style_name.is_empty() {
                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    b"fo:font-size" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.font_size = Some(parse_measure(val));
                                        }
                                    }
                                    b"fo:color" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.font_color = Some(val.to_string());
                                        }
                                    }
                                    b"fo:font-weight" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            if val == "bold" {
                                                current_style.bold = true;
                                            }
                                        }
                                    }
                                    b"fo:font-style" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            if val == "italic" {
                                                current_style.italic = true;
                                            }
                                        }
                                    }
                                    b"style:text-underline-style" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            if val == "solid" {
                                                current_style.underline = true;
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    b"style:paragraph-properties" => {
                        if !current_style_name.is_empty() {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"draw:textarea-vertical-align" {
                                    if let Ok(val) = std::str::from_utf8(&attr.value) {
                                        current_style.vertical_alignment = Some(match val {
                                            "bottom" => VerticalAlignment::Bottom,
                                            "middle" | "center" => VerticalAlignment::Center,
                                            "justify" => VerticalAlignment::Justify,
                                            _ => VerticalAlignment::Top,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    b"style:drawing-page-properties" => {
                        if !current_style_name.is_empty() {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"draw:fill-color" {
                                    if let Ok(val) = std::str::from_utf8(&attr.value) {
                                        current_style.page_background_color = Some(val.to_string());
                                    }
                                } else if attr.key.as_ref() == b"draw:fill-image-name" {
                                    if let Ok(val) = std::str::from_utf8(&attr.value) {
                                        current_style.page_background_image = Some(val.to_string());
                                    }
                                }
                            }
                        }
                    }
                    b"style:graphic-properties" => {
                        if !current_style_name.is_empty() {
                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    b"draw:fill-color" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.fill_color = Some(val.to_string());
                                        }
                                    }
                                    b"svg:stroke-color" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.stroke_color = Some(val.to_string());
                                        }
                                    }
                                    b"svg:stroke-width" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.stroke_width = Some(parse_measure(val));
                                        }
                                    }
                                    b"draw:textarea-vertical-align" => {
                                        if let Ok(val) = std::str::from_utf8(&attr.value) {
                                            current_style.vertical_alignment = Some(match val {
                                                "bottom" => VerticalAlignment::Bottom,
                                                "middle" | "center" => VerticalAlignment::Center,
                                                "justify" => VerticalAlignment::Justify,
                                                _ => VerticalAlignment::Top,
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            Ok(Event::End(ref e)) => {
                if e.name().as_ref() == b"style:style" && !current_style_name.is_empty() {
                    styles.insert(current_style_name.clone(), current_style.clone());
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    styles
}

/// Parse content of a presentation (pages with frames)
#[allow(clippy::too_many_lines)]
fn parse_presentation_content(
    xml: &str,
    styles: &StyleMap,
    fill_images: &HashMap<String, String>,
) -> Vec<Page> {
    let mut pages = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.trim_text(false);
    let mut buf = Vec::new();

    // State machine
    let mut in_page = false;
    let mut page_number = 1;
    let mut current_page_content = Vec::new();

    // Frame state
    let mut in_frame = false;
    let mut current_frame_rect = Rect::default();
    let mut current_frame_style_name = String::new();
    let mut current_image_href = None;

    // Text box state
    let mut in_text_box = false;
    let mut current_text_runs = Vec::new();

    // Text Styling State (nested spans)
    let mut current_span_style_name = String::new();
    let mut current_paragraph_style_name = String::new();
    let mut in_paragraph = false;

    // Iterate through XML
    // Structure: draw:page -> draw:frame (geometry + style) -> draw:text-box -> text:p -> text:span
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) => {
                if e.name().as_ref() == b"draw:image" {
                    // if in_frame {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref().ends_with(b"href") {
                            if let Ok(v) = std::str::from_utf8(&attr.value) {
                                current_image_href = Some(v.to_string());
                            }
                        }
                    }
                    // }
                }
            }
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"draw:page" => {
                    in_page = true;
                    current_page_content.clear();

                    // Check for page background
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"draw:style-name" {
                            if let Ok(style_name) = std::str::from_utf8(&attr.value) {
                                if let Some(style) = styles.get(style_name) {
                                    if let Some(color) = &style.page_background_color {
                                        // Create background shape
                                        let bg_rect = TextBlock {
                                            bounds: Rect::new(0.0, 0.0, 960.0, 540.0), // TODO: parse master page dims
                                            runs: Vec::new(),
                                            paragraph_style: None,
                                            vertical_alignment: None,
                                            style: ShapeStyle {
                                                fill_color: Some(color.clone()),
                                                z_index: Some(-1), // Background
                                                ..ShapeStyle::default()
                                            },
                                            rotation: 0.0,
                                        };
                                        current_page_content.push(ContentBlock::Text(bg_rect));
                                    }

                                    if let Some(img_name) = &style.page_background_image {
                                        if let Some(href) = fill_images.get(img_name) {
                                            let image_block = ImageBlock {
                                                resource_id: href.clone(),
                                                bounds: Rect::new(0.0, 0.0, 960.0, 540.0),
                                                alt_text: None,
                                                format: None,
                                                original_size: None,
                                                style: ShapeStyle {
                                                    z_index: Some(-1),
                                                    ..ShapeStyle::default()
                                                },
                                                rotation: 0.0,
                                            };
                                            current_page_content
                                                .push(ContentBlock::Image(image_block));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                b"draw:frame" => {
                    in_frame = true;
                    // Reset frame info
                    current_frame_rect = Rect::default();
                    current_frame_style_name.clear();
                    current_image_href = None;

                    // Parse attributes for x, y, width, height, style-name
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"svg:x" => {
                                let v = parse_measure(
                                    std::str::from_utf8(&attr.value).unwrap_or("0cm"),
                                );
                                current_frame_rect.x = v;
                            }
                            b"svg:y" => {
                                let v = parse_measure(
                                    std::str::from_utf8(&attr.value).unwrap_or("0cm"),
                                );
                                current_frame_rect.y = v;
                            }
                            b"svg:width" => {
                                let v = parse_measure(
                                    std::str::from_utf8(&attr.value).unwrap_or("0cm"),
                                );
                                current_frame_rect.width = v;
                            }
                            b"svg:height" => {
                                let v = parse_measure(
                                    std::str::from_utf8(&attr.value).unwrap_or("0cm"),
                                );
                                current_frame_rect.height = v;
                            }
                            b"draw:style-name" => {
                                if let Ok(v) = std::str::from_utf8(&attr.value) {
                                    current_frame_style_name = v.to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"draw:text-box" => {
                    if in_frame {
                        in_text_box = true;
                        current_text_runs.clear();
                    }
                }
                b"text:p" | b"text:h" => {
                    in_paragraph = true;
                    // Check for paragraph style
                    current_paragraph_style_name.clear();
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"text:style-name" {
                            if let Ok(v) = std::str::from_utf8(&attr.value) {
                                current_paragraph_style_name = v.to_string();
                            }
                        }
                    }

                    // Start new paragraph (maybe add newline if not first)
                    if in_text_box && !current_text_runs.is_empty() {
                        current_text_runs.push(TextRun::new("\n"));
                    }
                }
                b"text:span" => {
                    // Start styling span
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"text:style-name" {
                            if let Ok(v) = std::str::from_utf8(&attr.value) {
                                current_span_style_name = v.to_string();
                            }
                        }
                    }
                }
                b"draw:image" => {
                    if in_frame {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref().ends_with(b"href") {
                                if let Ok(v) = std::str::from_utf8(&attr.value) {
                                    current_image_href = Some(v.to_string());
                                }
                            }
                        }
                    }
                }

                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"text:p" | b"text:h" => {
                    in_paragraph = false;
                }
                b"draw:page" => {
                    if in_page {
                        let mut page = Page::new(
                            page_number,
                            Dimensions {
                                width: 960.0,
                                height: 540.0,
                            },
                        ); // Default
                        for block in current_page_content.drain(..) {
                            page.add_content(block);
                        }
                        pages.push(page);
                        page_number += 1;
                        in_page = false;
                    }
                }
                b"draw:frame" => {
                    if in_frame {
                        // End of frame. If we have content, add it.

                        // Check if it's an image
                        if let Some(href) = current_image_href.take() {
                            let block = ImageBlock {
                                bounds: current_frame_rect,
                                resource_id: href,
                                alt_text: None,
                                format: None,
                                original_size: None,
                                style: ShapeStyle::default(),
                                rotation: 0.0,
                            };
                            current_page_content.push(ContentBlock::Image(block));
                        } else {
                            // Text block
                            let mut block = TextBlock::new(current_frame_rect);
                            let mut has_text = false;

                            if !current_text_runs.is_empty() {
                                has_text = true;
                                for run in std::mem::take(&mut current_text_runs) {
                                    block.add_run(run);
                                }
                            }

                            // Lookup style for frame (geometry)
                            let mut valign = None;
                            if let Some(style) = styles.get(&current_frame_style_name) {
                                valign = style.vertical_alignment;
                                if let Some(color) = &style.fill_color {
                                    if color != "none" {
                                        block.style.fill_color = Some(color.clone());
                                    }
                                }
                                if let Some(color) = &style.stroke_color {
                                    block.style.stroke_color = Some(color.clone());
                                }
                                if let Some(width) = style.stroke_width {
                                    block.style.stroke_width = Some(width);
                                }
                            }
                            block.vertical_alignment = valign;

                            // Even if no text, if there is style (like fill color), we should output it
                            // Similar to PPTX behavior
                            if has_text
                                || block.style.fill_color.is_some()
                                || block.style.stroke_color.is_some()
                            {
                                current_page_content.push(ContentBlock::Text(block));
                            }
                        } // End else (text block)

                        in_frame = false;
                    }
                }
                b"draw:text-box" => {
                    in_text_box = false;
                }
                b"text:span" => {
                    current_span_style_name.clear();
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if in_text_box && in_paragraph {
                    if let Ok(t) = e.unescape() {
                        if !t.trim().is_empty() {
                            let mut run = TextRun::new(t.to_string());

                            // Apply current styles. Priority: Span > Paragraph > Frame?
                            // Usually text properties come from span or paragraph style.
                            let style_to_use = if !current_span_style_name.is_empty() {
                                styles.get(&current_span_style_name)
                            } else if !current_paragraph_style_name.is_empty() {
                                styles.get(&current_paragraph_style_name)
                            } else {
                                None
                            };

                            if let Some(style) = style_to_use {
                                if let Some(size) = style.font_size {
                                    run.style.font_size = Some(size);
                                }
                                if let Some(color) = &style.font_color {
                                    run.style.color = Some(color.clone());
                                }
                                if style.bold {
                                    run.style.bold = true;
                                }
                                if style.italic {
                                    run.style.italic = true;
                                }
                                if style.underline {
                                    run.style.underline = true;
                                }
                            }

                            current_text_runs.push(run);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    pages
}

/// Helper to parse measures like "2.5cm", "1in" to points
fn parse_measure(val: &str) -> f64 {
    // Very basic parser. ODF uses cm, in, pt, mm, etc.
    // 1cm = 28.35pt, 1in = 72pt
    let val = val.trim();
    if let Some(v) = val.strip_suffix("cm") {
        let n: f64 = v.parse().unwrap_or(0.0);
        n * 28.3465
    } else if let Some(v) = val.strip_suffix("in") {
        let n: f64 = v.parse().unwrap_or(0.0);
        n * 72.0
    } else if let Some(v) = val.strip_suffix("mm") {
        let n: f64 = v.parse().unwrap_or(0.0);
        n * 2.83465
    } else if let Some(v) = val.strip_suffix("pt") {
        let n: f64 = v.parse().unwrap_or(0.0);
        n
    } else {
        val.parse().unwrap_or(0.0)
    }
}

/// Parse an ODS spreadsheet
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn parse_odf_spreadsheet(data: Bytes, context: ParseContext) -> Result<Document> {
    let cursor = Cursor::new(&data);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| Error::ParseError(format!("Invalid ZIP: {e}")))?;

    // Read content.xml
    let content_xml = read_zip_file(&mut archive, "content.xml")?;

    // Parse spreadsheet structure
    let tables = extract_tables_from_ods_xml(&content_xml)?;

    // Create document
    let mut document = Document::new();

    #[allow(clippy::cast_possible_truncation)]
    for (sheet_num, (sheet_name, rows)) in tables.into_iter().enumerate() {
        let mut page = Page::new((sheet_num + 1) as u32, Dimensions::LETTER);

        // Add sheet name as header
        let header_run = TextRun {
            text: sheet_name,
            style: TextStyle {
                bold: true,
                font_size: Some(14.0),
                ..TextStyle::default()
            },
            bounds: None,
            char_positions: None,
        };
        let header_block = TextBlock {
            runs: vec![header_run],
            paragraph_style: None,
            vertical_alignment: None,
            bounds: Rect::new(50.0, 20.0, 500.0, 30.0),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };
        page.add_content(ContentBlock::Text(header_block));

        // Create table
        if !rows.is_empty() {
            let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
            #[allow(clippy::cast_precision_loss)]
            let table = TableBlock {
                bounds: Rect::new(50.0, 60.0, 500.0, (rows.len() as f64) * 25.0),
                rows: rows
                    .into_iter()
                    .map(|cells| TableRow {
                        cells: cells
                            .into_iter()
                            .map(|text| TableCell {
                                content: vec![ContentBlock::Text(TextBlock {
                                    runs: vec![TextRun::new(&text)],
                                    paragraph_style: None,
                                    vertical_alignment: None,
                                    bounds: Rect::default(),
                                    style: ShapeStyle::default(),
                                    rotation: 0.0,
                                })],
                                col_span: 1,
                                row_span: 1,
                                background_color: None,
                                borders: None,
                            })
                            .collect(),
                        height: None,
                    })
                    .collect(),
                column_count: col_count,
                column_widths: Vec::new(),
                style: ShapeStyle::default(),
                rotation: 0.0,
            };
            page.add_content(ContentBlock::Table(table));
        }

        document.pages.push(page);
    }

    // If no sheets, add empty page
    if document.pages.is_empty() {
        document.pages.push(Page::new(1, Dimensions::LETTER));
    }

    // Set metadata
    let mut metadata = Metadata::default();
    if let Some(ref filename) = context.filename {
        metadata.title = Some(filename.clone());
    }
    metadata.add_custom("format", "ODS");
    document.metadata = metadata;

    Ok(document)
}

/// Read a file from the ZIP archive
fn read_zip_file(archive: &mut ZipArchive<Cursor<&Bytes>>, name: &str) -> Result<String> {
    let mut file = archive
        .by_name(name)
        .map_err(|e| Error::ParseError(format!("Cannot find {name}: {e}")))?;

    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| Error::ParseError(format!("Cannot read {name}: {e}")))?;

    Ok(content)
}

/// Read a file from the ZIP archive as bytes
fn read_zip_file_bytes(archive: &mut ZipArchive<Cursor<&Bytes>>, name: &str) -> Result<Vec<u8>> {
    let mut file = archive
        .by_name(name)
        .map_err(|e| Error::ParseError(format!("Cannot find {name}: {e}")))?;

    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|e| Error::ParseError(format!("Cannot read {name}: {e}")))?;

    Ok(content)
}

/// Extract text from ODF XML (`content.xml`)
#[allow(clippy::unnecessary_wraps)]
fn extract_text_from_odf_xml(xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);

    let mut buf = Vec::new();
    let mut text = String::new();
    let mut in_text_element = false;
    let mut depth: i32 = 0;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "text:p" || name == "text:h" || name == "text:span" {
                    in_text_element = true;
                    depth += 1;
                }
            }
            Ok(Event::End(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "text:p" || name == "text:h" {
                    if in_text_element {
                        text.push('\n');
                    }
                    depth -= 1;
                    if depth <= 0 {
                        in_text_element = false;
                        depth = 0;
                    }
                } else if name == "text:span" {
                    depth -= 1;
                    if depth <= 0 {
                        in_text_element = false;
                        depth = 0;
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_text_element {
                    if let Ok(t) = e.unescape() {
                        text.push_str(&t);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                warn!("Error parsing ODF XML: {e}");
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(text.trim().to_string())
}

/// Extract tables from ODS XML
#[allow(clippy::unnecessary_wraps)]
fn extract_tables_from_ods_xml(xml: &str) -> Result<Vec<(String, Vec<Vec<String>>)>> {
    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);

    let mut buf = Vec::new();
    let mut tables: Vec<(String, Vec<Vec<String>>)> = Vec::new();
    let mut current_sheet_name = String::from("Sheet");
    let mut current_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell_text = String::new();
    let mut in_cell = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "table:table" {
                    // Get sheet name from attributes
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"table:name" {
                            if let Ok(val) = std::str::from_utf8(&attr.value) {
                                current_sheet_name = val.to_string();
                            }
                        }
                    }
                } else if name == "table:table-cell" {
                    in_cell = true;
                    current_cell_text.clear();
                }
            }
            Ok(Event::End(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "table:table" {
                    if !current_rows.is_empty() || !current_row.is_empty() {
                        if !current_row.is_empty() {
                            current_rows.push(std::mem::take(&mut current_row));
                        }
                        tables.push((
                            std::mem::take(&mut current_sheet_name),
                            std::mem::take(&mut current_rows),
                        ));
                        current_sheet_name = String::from("Sheet");
                    }
                } else if name == "table:table-row" {
                    if !current_row.is_empty() {
                        current_rows.push(std::mem::take(&mut current_row));
                    }
                } else if name == "table:table-cell" {
                    current_row.push(std::mem::take(&mut current_cell_text));
                    in_cell = false;
                }
            }
            Ok(Event::Text(e)) => {
                if in_cell {
                    if let Ok(t) = e.unescape() {
                        current_cell_text.push_str(&t);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                warn!("Error parsing ODS XML: {e}");
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(tables)
}

/// Create a text page
fn create_text_page(text: &str, page_num: u32) -> Page {
    let text_block = TextBlock {
        runs: vec![TextRun::new(text)],
        paragraph_style: None,
        vertical_alignment: None, // TODO: parse vertical alignment
        bounds: Rect::new(50.0, 50.0, 500.0, 700.0),
        style: ShapeStyle::default(),
        rotation: 0.0,
    };

    let mut page = Page::new(page_num, Dimensions::LETTER);
    page.add_content(ContentBlock::Text(text_block));
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_odt_parser_metadata() {
        let parser = OdtParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "ODT Parser");
    }

    #[test]
    fn test_ods_parser_metadata() {
        let parser = OdsParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "ODS Parser");
    }

    #[test]
    fn test_odp_parser_metadata() {
        let parser = OdpParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "ODP Parser");
    }

    #[test]
    fn test_odt_can_parse() {
        let data = std::fs::read("../../test-files/testPhoneNumberExtractor.odt").unwrap();
        let parser = OdtParser::new();
        assert!(parser.can_parse(&data), "OdtParser should detect ODT file");
    }

    #[test]
    fn test_odp_colors() {
        let xml = r##"
        <office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
            <office:automatic-styles>
                <style:style style:name="gr1" style:family="graphic">
                    <style:graphic-properties draw:fill-color="#ff0000" svg:stroke-color="#0000ff" svg:stroke-width="0.05cm"/>
                </style:style>
                <style:style style:name="P1" style:family="paragraph">
                    <style:text-properties fo:color="#00ff00" fo:font-size="24pt" fo:font-weight="bold"/>
                </style:style>
                <style:style style:name="T1" style:family="text">
                    <style:text-properties fo:color="#ff00ff" fo:font-style="italic"/>
                </style:style>
            </office:automatic-styles>
            <office:body>
                <office:presentation>
                    <draw:page draw:name="page1">
                        <draw:frame draw:style-name="gr1" svg:x="2cm" svg:y="2cm" svg:width="10cm" svg:height="5cm">
                            <draw:text-box>
                                <text:p text:style-name="P1">Hello <text:span text:style-name="T1">World</text:span></text:p>
                            </draw:text-box>
                        </draw:frame>
                    </draw:page>
                </office:presentation>
            </office:body>
        </office:document-content>
        "##;

        let styles = parse_automatic_styles(xml);
        assert!(styles.contains_key("gr1"));
        assert!(styles.contains_key("P1"));
        assert!(styles.contains_key("T1"));

        let pages = parse_presentation_content(xml, &styles, &std::collections::HashMap::new());
        assert_eq!(pages.len(), 1);

        let blocks = &pages[0].content;
        assert_eq!(blocks.len(), 1);

        if let ContentBlock::Text(block) = &blocks[0] {
            // Check box style
            assert_eq!(block.style.fill_color, Some("#ff0000".to_string()));
            assert_eq!(block.style.stroke_color, Some("#0000ff".to_string()));
            assert!(block.style.stroke_width.unwrap() > 1.4);

            // Check text runs
            assert_eq!(block.runs.len(), 2);

            // "Hello " - should inherit P1
            let run1 = &block.runs[0];
            assert_eq!(run1.text, "Hello ");
            assert_eq!(run1.style.color, Some("#00ff00".to_string()));
            assert_eq!(run1.style.font_size, Some(24.0));
            assert!(run1.style.bold);
            assert!(!run1.style.italic);

            // "World" - should inherit T1
            let run2 = &block.runs[1];
            assert_eq!(run2.text, "World");
            assert_eq!(run2.style.color, Some("#ff00ff".to_string()));
            assert!(run2.style.italic);
        } else {
            panic!("Expected TextBlock");
        }
    }

    #[test]
    fn test_odp_images_and_backgrounds() {
        let xml = r##"
        <office:document-content xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:xlink="http://www.w3.org/1999/xlink">
            <office:automatic-styles>
                <style:style style:name="dp1" style:family="drawing-page">
                    <style:drawing-page-properties draw:fill-color="#cccccc"/>
                </style:style>
            </office:automatic-styles>
            <office:body>
                <office:presentation>
                    <draw:page draw:name="page1" draw:style-name="dp1">
                        <!-- Image Frame -->
                        <draw:frame svg:x="1cm" svg:y="1cm" svg:width="5cm" svg:height="5cm">
                            <draw:image xlink:href="Pictures/image1.png"/>
                        </draw:frame>
                    </draw:page>
                </office:presentation>
            </office:body>
        </office:document-content>
        "##;

        let styles = parse_automatic_styles(xml);
        assert!(styles.contains_key("dp1"));
        assert_eq!(
            styles["dp1"].page_background_color,
            Some("#cccccc".to_string())
        );

        let pages = parse_presentation_content(xml, &styles, &std::collections::HashMap::new());
        assert_eq!(pages.len(), 1);

        let blocks = &pages[0].content;
        // assert_eq!(blocks.len(), 2); // Background + Image

        if blocks.len() == 1 {
            if let ContentBlock::Text(bg) = &blocks[0] {
                if bg.style.z_index == Some(-1) {
                    panic!("Found Background, Missing Image");
                } else {
                    panic!("Found TextBlock (not background), Missing Background & Image? {bg:?}",);
                }
            }
            if let ContentBlock::Image(_) = &blocks[0] {
                panic!("Found Image, Missing Background");
            }
        }
        assert_eq!(blocks.len(), 2, "Expected 2 blocks, got {}", blocks.len());

        // 1. Background (first block)
        if let ContentBlock::Text(bg) = &blocks[0] {
            assert_eq!(bg.style.fill_color, Some("#cccccc".to_string()));
            assert_eq!(bg.style.z_index, Some(-1));
        } else {
            panic!("Expected background TextBlock, got {:?}", blocks[0]);
        }

        // 2. Image (second block)
        if let ContentBlock::Image(img) = &blocks[1] {
            assert_eq!(img.resource_id, "Pictures/image1.png");
            let expected_x = crate::office::odf::parse_measure("1cm");
            assert!((img.bounds.x - expected_x).abs() < f64::EPSILON);
        } else {
            panic!("Expected ImageBlock, got {:?}", blocks[1]);
        }
    }

    #[test]
    fn test_odp_background_image() {
        let xml = r#"
        <office:document-content xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0">
            <office:automatic-styles>
                <style:style style:name="dp1" style:family="drawing-page">
                    <style:drawing-page-properties draw:fill-image-name="bitmap1"/>
                </style:style>
            </office:automatic-styles>
            <office:body>
                <office:presentation>
                    <draw:page draw:name="page1" draw:style-name="dp1">
                    </draw:page>
                </office:presentation>
            </office:body>
        </office:document-content>
        "#;

        let styles = parse_automatic_styles(xml);
        assert!(styles.contains_key("dp1"));
        assert_eq!(
            styles["dp1"].page_background_image,
            Some("bitmap1".to_string())
        );

        let mut fill_images = std::collections::HashMap::new();
        fill_images.insert("bitmap1".to_string(), "Pictures/bg.png".to_string());

        let pages = parse_presentation_content(xml, &styles, &fill_images);
        assert_eq!(pages.len(), 1);

        let blocks = &pages[0].content;
        assert_eq!(blocks.len(), 1);

        if let ContentBlock::Image(bg) = &blocks[0] {
            assert_eq!(bg.resource_id, "Pictures/bg.png");
            assert_eq!(bg.style.z_index, Some(-1));
        } else {
            panic!("Expected ImageBlock for background");
        }
    }

    #[test]
    #[ignore = "Run manually: cargo test -- --ignored inspect_odp_structure"]
    fn inspect_odp_structure() {
        use std::fs::File;
        use std::io::Read;
        use zip::ZipArchive;

        let path = "c:/Dev/RustSandbox/Prism/test-files/odp_testWithColors.odp";
        println!("Opening: {path}");
        let file = File::open(path).expect("file open");
        let mut archive = ZipArchive::new(file).expect("zip open");

        {
            let mut content = archive.by_name("content.xml").expect("content.xml");
            let mut xml = String::new();
            content.read_to_string(&mut xml).unwrap();
            println!("--- content.xml ---\n{xml}\n-------------------");
        }

        {
            let mut styles = archive.by_name("styles.xml").expect("styles.xml");
            let mut xml = String::new();
            styles.read_to_string(&mut xml).unwrap();
            println!("--- styles.xml ---\n{xml}\n------------------");
        }
    }
}
