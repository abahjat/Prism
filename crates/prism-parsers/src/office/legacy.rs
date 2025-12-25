//! Legacy Office format parsers (DOC, XLS, PPT)
//!
//! Parses legacy Microsoft Office files that use OLE2/CFB format.

use async_trait::async_trait;
use bytes::Bytes;
use calamine::Reader;
use cfb::CompoundFile;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::{debug, info, warn};

/// Legacy DOC parser (Word 97-2003)
#[derive(Debug, Clone)]
pub struct DocParser;

impl DocParser {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn is_ole2_file(data: &[u8]) -> bool {
        data.len() >= 8 && &data[0..8] == &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]
    }

    fn extract_text_from_doc(data: &[u8]) -> Result<Vec<String>> {
        let cursor = Cursor::new(data);
        let mut comp = CompoundFile::open(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open OLE2 file: {}", e)))?;

        // Try to find WordDocument stream
        let mut text_parts = Vec::new();

        // DOC files store text in various streams, primarily "WordDocument"
        // This is a simplified extraction - full DOC parsing is very complex
        if let Ok(mut stream) = comp.open_stream("WordDocument") {
            use std::io::Read;
            let mut buffer = Vec::new();
            if stream.read_to_end(&mut buffer).is_ok() {
                // Extract printable ASCII/UTF-8 text (very basic)
                let text = extract_printable_text(&buffer);
                if !text.is_empty() {
                    text_parts.push(text);
                }
            }
        }

        // Also try "0Table" and "1Table" streams
        for table_name in &["0Table", "1Table"] {
            if let Ok(mut stream) = comp.open_stream(table_name) {
                use std::io::Read;
                let mut buffer = Vec::new();
                if stream.read_to_end(&mut buffer).is_ok() {
                    let text = extract_printable_text(&buffer);
                    if !text.is_empty() {
                        text_parts.push(text);
                    }
                }
            }
        }

        if text_parts.is_empty() {
            text_parts.push(
                "Unable to extract text from DOC file. Legacy format requires full parser."
                    .to_string(),
            );
        }

        Ok(text_parts)
    }
}

impl Default for DocParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for DocParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "application/msword".to_string(),
            extension: "doc".to_string(),
            family: prism_core::format::FormatFamily::Office,
            name: "Microsoft Word 97-2003 (DOC)".to_string(),
            is_container: true,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        if !Self::is_ole2_file(data) {
            return false;
        }

        // Check if it's likely a Word document by looking for Word-specific markers
        let cursor = Cursor::new(data);
        if let Ok(comp) = CompoundFile::open(cursor) {
            // Word documents have a "WordDocument" stream
            return comp.exists("WordDocument");
        }

        false
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing DOC file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let text_parts = Self::extract_text_from_doc(&data)?;

        // Create pages with extracted text
        let mut content_blocks = Vec::new();
        for text in &text_parts {
            if text.trim().is_empty() {
                continue;
            }

            let text_run = TextRun {
                text: text.clone(),
                style: TextStyle::default(),
                bounds: None,
                char_positions: None,
            };

            let text_block = TextBlock {
                runs: vec![text_run],
                paragraph_style: None,
                bounds: prism_core::document::Rect::default(),
            };

            content_blocks.push(ContentBlock::Text(text_block));
        }

        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: content_blocks,
            annotations: vec![],
            metadata: PageMetadata {
                label: None,
                rotation: 0,
            },
        };

        let mut metadata = Metadata::new();
        if let Some(filename) = context.filename {
            metadata.title = Some(filename);
        }
        metadata.add_custom("format", "DOC");
        metadata.add_custom("legacy_format", true);

        let mut document = Document::builder().metadata(metadata).build();
        document.pages = vec![page];

        info!("Successfully parsed DOC file (legacy format)");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "DOC Parser (Legacy)".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// Legacy XLS parser (Excel 97-2003)
#[derive(Debug, Clone)]
pub struct XlsParser;

impl XlsParser {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn is_ole2_file(data: &[u8]) -> bool {
        data.len() >= 8 && &data[0..8] == &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]
    }
}

impl Default for XlsParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for XlsParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "application/vnd.ms-excel".to_string(),
            extension: "xls".to_string(),
            family: prism_core::format::FormatFamily::Office,
            name: "Microsoft Excel 97-2003 (XLS)".to_string(),
            is_container: true,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        if !Self::is_ole2_file(data) {
            return false;
        }

        let cursor = Cursor::new(data);
        if let Ok(comp) = CompoundFile::open(cursor) {
            // Excel documents typically have "Workbook" or "Book" stream
            return comp.exists("Workbook") || comp.exists("Book");
        }

        false
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing XLS file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Try using calamine which supports XLS format
        let cursor = Cursor::new(data.as_ref());
        match calamine::open_workbook_auto_from_rs(cursor) {
            Ok(mut workbook) => {
                // Use the same logic as XLSX parser
                let sheet_names = workbook.sheet_names().to_vec();
                let mut pages = Vec::new();

                for (idx, name) in sheet_names.iter().enumerate() {
                    if let Ok(range) = workbook.worksheet_range(name) {
                        // Create table-like content from cells
                        let mut content_blocks = Vec::new();

                        for row in range.rows() {
                            let mut row_text = String::new();
                            for cell in row {
                                let cell_text = format!("{}\t", cell);
                                row_text.push_str(&cell_text);
                            }

                            if !row_text.trim().is_empty() {
                                let text_run = TextRun {
                                    text: row_text,
                                    style: TextStyle::default(),
                                    bounds: None,
                                    char_positions: None,
                                };

                                let text_block = TextBlock {
                                    runs: vec![text_run],
                                    paragraph_style: None,
                                    bounds: prism_core::document::Rect::default(),
                                };

                                content_blocks.push(ContentBlock::Text(text_block));
                            }
                        }

                        let page = Page {
                            number: (idx + 1) as u32,
                            dimensions: Dimensions::LETTER,
                            content: content_blocks,
                            annotations: vec![],
                            metadata: PageMetadata {
                                label: Some(name.clone()),
                                rotation: 0,
                            },
                        };

                        pages.push(page);
                    }
                }

                if pages.is_empty() {
                    pages.push(Page {
                        number: 1,
                        dimensions: Dimensions::LETTER,
                        content: vec![],
                        annotations: vec![],
                        metadata: PageMetadata {
                            label: None,
                            rotation: 0,
                        },
                    });
                }

                let mut metadata = Metadata::new();
                if let Some(filename) = context.filename {
                    metadata.title = Some(filename);
                }
                metadata.add_custom("format", "XLS");
                metadata.add_custom("legacy_format", true);

                let page_count = pages.len();
                metadata.add_custom("sheet_count", page_count as i64);

                let mut document = Document::builder().metadata(metadata).build();
                document.pages = pages;

                info!("Successfully parsed XLS with {} sheets", page_count);

                Ok(document)
            }
            Err(e) => {
                warn!("Failed to parse XLS with calamine: {}", e);
                Err(Error::ParseError(format!("Failed to parse XLS: {}", e)))
            }
        }
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "XLS Parser (Legacy)".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::TableExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// Legacy PPT parser (PowerPoint 97-2003)
#[derive(Debug, Clone)]
pub struct PptParser;

impl PptParser {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn is_ole2_file(data: &[u8]) -> bool {
        data.len() >= 8 && &data[0..8] == &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]
    }
}

impl Default for PptParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PptParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "application/vnd.ms-powerpoint".to_string(),
            extension: "ppt".to_string(),
            family: prism_core::format::FormatFamily::Office,
            name: "Microsoft PowerPoint 97-2003 (PPT)".to_string(),
            is_container: true,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        if !Self::is_ole2_file(data) {
            return false;
        }

        let cursor = Cursor::new(data);
        if let Ok(comp) = CompoundFile::open(cursor) {
            // PowerPoint documents have "PowerPoint Document" or "Current User" stream
            return comp.exists("PowerPoint Document") || comp.exists("Current User");
        }

        false
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing PPT file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let cursor = Cursor::new(data.as_ref());
        let mut comp = CompoundFile::open(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open PPT file: {}", e)))?;

        // Extract basic text - PPT format is very complex
        let mut text_parts = Vec::new();

        if let Ok(mut stream) = comp.open_stream("PowerPoint Document") {
            use std::io::Read;
            let mut buffer = Vec::new();
            if stream.read_to_end(&mut buffer).is_ok() {
                let text = extract_printable_text(&buffer);
                if !text.is_empty() {
                    text_parts.push(text);
                }
            }
        }

        if text_parts.is_empty() {
            text_parts.push(
                "Unable to extract text from PPT file. Legacy format requires full parser."
                    .to_string(),
            );
        }

        let mut content_blocks = Vec::new();
        for text in &text_parts {
            if text.trim().is_empty() {
                continue;
            }

            let text_run = TextRun {
                text: text.clone(),
                style: TextStyle::default(),
                bounds: None,
                char_positions: None,
            };

            let text_block = TextBlock {
                runs: vec![text_run],
                paragraph_style: None,
                bounds: prism_core::document::Rect::default(),
            };

            content_blocks.push(ContentBlock::Text(text_block));
        }

        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: content_blocks,
            annotations: vec![],
            metadata: PageMetadata {
                label: Some("Slide 1".to_string()),
                rotation: 0,
            },
        };

        let mut metadata = Metadata::new();
        if let Some(filename) = context.filename {
            metadata.title = Some(filename);
        }
        metadata.add_custom("format", "PPT");
        metadata.add_custom("legacy_format", true);

        let mut document = Document::builder().metadata(metadata).build();
        document.pages = vec![page];

        info!("Successfully parsed PPT file (legacy format)");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PPT Parser (Legacy)".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// Extract printable text from binary data
fn extract_printable_text(data: &[u8]) -> String {
    let mut text = String::new();
    let mut consecutive_printable = 0;
    let mut buffer = String::new();

    for &byte in data {
        if byte >= 32 && byte < 127 {
            // Printable ASCII
            buffer.push(byte as char);
            consecutive_printable += 1;
        } else if byte == b'\n' || byte == b'\r' || byte == b'\t' {
            buffer.push(byte as char);
            consecutive_printable += 1;
        } else {
            // Non-printable byte
            if consecutive_printable >= 4 {
                // Only keep runs of 4+ consecutive printable chars
                text.push_str(&buffer);
                text.push(' ');
            }
            buffer.clear();
            consecutive_printable = 0;
        }
    }

    // Add final buffer
    if consecutive_printable >= 4 {
        text.push_str(&buffer);
    }

    // Clean up the text
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| c.is_ascii() || c.is_whitespace())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_ole2() {
        let ole2_sig = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        assert!(DocParser::is_ole2_file(&ole2_sig));
        assert!(!DocParser::is_ole2_file(b"Not OLE2"));
    }

    #[test]
    fn test_extract_printable_text() {
        let data = b"Hello\x00\x00\x00World\x01\x02Test";
        let text = extract_printable_text(data);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(text.contains("Test"));
    }
}
