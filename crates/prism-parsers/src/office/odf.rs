// SPDX-License-Identifier: AGPL-3.0-only
//! `OpenDocument` format parsers (ODT, ODS, ODP)
//!
//! Parses `OpenDocument` files (used by `LibreOffice`, `OpenOffice`) into the
//! Unified Document Model. These formats are ZIP archives containing XML.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, Rect, ShapeStyle, TableBlock, TableCell,
        TableRow, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use quick_xml::events::Event;
use quick_xml::Reader;
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
pub struct OdpParser;

impl OdpParser {
    /// Create a new ODP parser
    #[must_use]
    pub fn new() -> Self {
        Self
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
        Format::odp()
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

/// Parse an ODF text or presentation document
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn parse_odf_document(data: Bytes, format_name: &str, context: ParseContext) -> Result<Document> {
    let cursor = Cursor::new(&data);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| Error::ParseError(format!("Invalid ZIP: {e}")))?;

    // Read content.xml
    let content_xml = read_zip_file(&mut archive, "content.xml")?;

    // Parse the XML to extract text
    let text_content = extract_text_from_odf_xml(&content_xml)?;

    // Create document
    let mut document = Document::new();

    // Create pages from paragraphs (split at ~2000 chars for pagination)
    let paragraphs: Vec<&str> = text_content.split('\n').collect();
    let mut current_page_text = String::new();
    let mut page_num: u32 = 1;

    for para in paragraphs {
        if current_page_text.len() + para.len() > 2000 && !current_page_text.is_empty() {
            // Create page
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

    // Add final page
    if !current_page_text.is_empty() {
        document
            .pages
            .push(create_text_page(&current_page_text, page_num));
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
                                    bounds: Rect::default(),
                                    style: ShapeStyle::default(),
                                    rotation: 0.0,
                                })],
                                col_span: 1,
                                row_span: 1,
                                background_color: None,
                            })
                            .collect(),
                        height: None,
                    })
                    .collect(),
                column_count: col_count,
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
}
