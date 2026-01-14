// SPDX-License-Identifier: AGPL-3.0-only
//! Legacy Office format parsers (DOC, XLS, PPT, MPP)
//!
//! Parses legacy Microsoft Office files that use OLE2/CFB format.

use async_trait::async_trait;
use bytes::Bytes;
use calamine::Reader;
use cfb::CompoundFile;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, ShapeStyle, TextBlock,
        TextRun, TextStyle,
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
    /// Create a new DOC parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if the data has OLE2/CFB signature (Word 97-2003)
    #[must_use]
    fn is_ole2_file(data: &[u8]) -> bool {
        data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
    }

    /// Check if the data has Word 2.0/Word for Windows signature
    #[must_use]
    fn is_word2_file(data: &[u8]) -> bool {
        // Word 2.0/6.0 magic bytes: DB A5 2D 00
        data.starts_with(&[0xDB, 0xA5, 0x2D, 0x00])
    }

    /// Extract text parts from DOC stream
    ///
    /// # Errors
    /// Returns an error if the OLE2 file cannot be opened or parsed.
    fn extract_text_from_doc(data: &[u8]) -> Result<Vec<String>> {
        let cursor = Cursor::new(data);
        let mut comp = CompoundFile::open(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open OLE2 file: {e}")))?;

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

    /// Extract text from Word 2.0/6.0 format (non-OLE2)
    ///
    /// Word 2.0 files store text directly after a header. This is a basic extraction.
    #[must_use]
    fn extract_text_from_word2(data: &[u8]) -> Vec<String> {
        // Word 2.0 stores text after a header. The text usually starts around offset 128.
        // We'll use the general printable text extraction with some heuristics.
        let mut text_parts = Vec::new();

        // Skip the header (roughly 128 bytes) and extract printable text
        let start_offset = 128.min(data.len());
        let text = extract_printable_text(&data[start_offset..]);

        if text.is_empty() {
            // Fallback: try extracting from the entire file
            let text = extract_printable_text(data);
            if !text.is_empty() {
                text_parts.push(text);
            }
        } else {
            text_parts.push(text);
        }

        if text_parts.is_empty() {
            text_parts.push("Unable to extract text from Word 2.0 file.".to_string());
        }

        text_parts
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
        // Accept Word 2.0/6.0 format (non-OLE2)
        if Self::is_word2_file(data) {
            return true;
        }

        // Accept OLE2/CFB format (Word 97-2003)
        if !Self::is_ole2_file(data) {
            return false;
        }

        // Check if it's likely a Word document by looking for WordDocument stream
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

        // Choose extraction method based on file format
        let text_parts = if Self::is_word2_file(&data) {
            info!("Detected Word 2.0/6.0 format, using legacy extraction");
            Self::extract_text_from_word2(&data)
        } else {
            Self::extract_text_from_doc(&data)?
        };

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
                vertical_alignment: None,
                bounds: prism_core::document::Rect::default(),
                style: ShapeStyle::default(),
                rotation: 0.0,
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
    /// Create a new XLS parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if the data has OLE2/CFB signature
    #[must_use]
    fn is_ole2_file(data: &[u8]) -> bool {
        data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
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
                let sheet_names = workbook.sheet_names().clone();
                let mut pages = Vec::new();

                for (idx, name) in sheet_names.iter().enumerate() {
                    if let Ok(range) = workbook.worksheet_range(name) {
                        // Create table-like content from cells
                        let mut content_blocks = Vec::new();

                        for row in range.rows() {
                            let mut row_text = String::new();
                            for cell in row {
                                let cell_text = format!("{cell}\t");
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
                                    vertical_alignment: None,
                                    bounds: prism_core::document::Rect::default(),
                                    style: ShapeStyle::default(),
                                    rotation: 0.0,
                                };

                                content_blocks.push(ContentBlock::Text(text_block));
                            }
                        }

                        #[allow(clippy::cast_possible_truncation)]
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
                #[allow(clippy::cast_possible_wrap)]
                metadata.add_custom("sheet_count", page_count as i64);

                let mut document = Document::builder().metadata(metadata).build();
                document.pages = pages;

                info!("Successfully parsed XLS with {} sheets", page_count);

                Ok(document)
            }
            Err(e) => {
                warn!("Failed to parse XLS with calamine: {e}");
                Err(Error::ParseError(format!("Failed to parse XLS: {e}")))
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

/// Legacy PPT parser (`PowerPoint` 97-2003)
#[derive(Debug, Clone)]
pub struct PptParser;

impl PptParser {
    /// Create a new PPT parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if the data has OLE2/CFB signature
    #[must_use]
    fn is_ole2_file(data: &[u8]) -> bool {
        data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
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
            .map_err(|e| Error::ParseError(format!("Failed to open PPT file: {e}")))?;

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
                vertical_alignment: None,
                bounds: prism_core::document::Rect::default(),
                style: ShapeStyle::default(),
                rotation: 0.0,
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

/// Microsoft Project (MPP) parser
#[derive(Debug, Clone)]
pub struct MppParser;

impl MppParser {
    /// Create a new MPP parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if the data has OLE2/CFB signature
    #[must_use]
    fn is_ole2_file(data: &[u8]) -> bool {
        data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
    }

    /// Check if OLE2 file is a Project file by looking for Project streams
    #[must_use]
    fn is_mpp_file(data: &[u8]) -> bool {
        let cursor = Cursor::new(data);
        if let Ok(comp) = CompoundFile::open(cursor) {
            // MPP files typically have these streams
            for entry in comp.walk() {
                let name = entry.name();
                // Common MPP stream names - look for Props or specific MPP markers
                if name == "Props" || name.contains("MSProject") || name == "Props12" {
                    return true;
                }
            }
        }
        false
    }

    /// Extract project info from MPP streams
    fn extract_project_info(data: &[u8]) -> Result<Vec<String>> {
        let cursor = Cursor::new(data);
        let comp = CompoundFile::open(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open OLE2 file: {e}")))?;

        let mut info_parts = Vec::new();

        // List all streams for debugging
        let stream_names: Vec<String> = comp.walk().map(|e| e.name().to_string()).collect();

        info_parts.push("Microsoft Project File".to_string());
        info_parts.push(String::new());
        info_parts.push(format!("Streams found: {}", stream_names.len()));

        // Try to extract any readable text from various streams
        for entry in comp.walk() {
            if entry.is_stream() {
                let name = entry.name().to_string();
                // Re-open to read
                let cursor = Cursor::new(data);
                if let Ok(mut comp) = CompoundFile::open(cursor) {
                    if let Ok(mut stream) = comp.open_stream(entry.path()) {
                        use std::io::Read;
                        let mut buffer = Vec::new();
                        if stream.read_to_end(&mut buffer).is_ok() && !buffer.is_empty() {
                            let text = extract_printable_text(&buffer);
                            if text.len() > 20 && !text.trim().is_empty() {
                                info_parts.push(format!("\n--- {name} ---"));
                                // Limit text per stream
                                let truncated: String = text.chars().take(500).collect();
                                info_parts.push(truncated);
                            }
                        }
                    }
                }
            }
        }

        if info_parts.len() <= 3 {
            info_parts.push(String::new());
            info_parts.push("Note: MPP format is complex and proprietary.".to_string());
            info_parts.push(
                "Full project data extraction requires specialized tools like MPXJ.".to_string(),
            );
        }

        Ok(info_parts)
    }
}

impl Default for MppParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for MppParser {
    fn format(&self) -> Format {
        Format::mpp()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_ole2_file(data) && Self::is_mpp_file(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing MPP file: {:?}", context.filename);

        let info_parts = Self::extract_project_info(&data)?;
        let content_text = info_parts.join("\n");

        // Create a single page with extracted content
        let text_run = TextRun {
            text: content_text,
            style: prism_core::document::TextStyle::default(),
            bounds: None,
            char_positions: None,
        };

        let text_block = TextBlock {
            runs: vec![text_run],
            paragraph_style: None,
            vertical_alignment: None,
            bounds: Rect::new(50.0, 50.0, 700.0, 900.0),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: vec![ContentBlock::Text(text_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "MPP");
        metadata.add_custom("legacy_format", true);

        let mut document = Document::builder().metadata(metadata).build();
        document.pages = vec![page];

        info!("Successfully parsed MPP file");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "MPP Parser (Microsoft Project)".to_string(),
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
#[must_use]
fn extract_printable_text(data: &[u8]) -> String {
    let mut text = String::new();
    let mut consecutive_printable = 0;
    let mut buffer = String::new();

    for &byte in data {
        if (32..127).contains(&byte) {
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
