//! DOCX (Microsoft Word) parser
//!
//! Parses DOCX files into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::Cursor;
use tracing::{debug, info};
use zip::ZipArchive;

/// DOCX parser
///
/// Parses Microsoft Word DOCX files into the Unified Document Model.
#[derive(Debug, Clone)]
pub struct DocxParser;

impl DocxParser {
    /// Create a new DOCX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data is a valid DOCX file (ZIP with word/ directory)
    fn is_docx_zip(data: &[u8]) -> bool {
        // Check ZIP signature: PK (0x504B)
        if data.len() < 4 {
            return false;
        }

        if &data[0..2] != b"PK" {
            return false;
        }

        // Try to open as ZIP and check for word/ directory
        let cursor = std::io::Cursor::new(data);
        if let Ok(mut archive) = ZipArchive::new(cursor) {
            // Check for word/document.xml which is present in DOCX files
            for i in 0..archive.len() {
                if let Ok(file) = archive.by_index(i) {
                    let name = file.name();
                    if name == "word/document.xml" {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Extract text from document.xml using proper XML parsing
    fn extract_text_from_xml(xml_content: &str) -> Vec<String> {
        let mut paragraphs = Vec::new();
        let mut current_paragraph = String::new();
        let mut in_paragraph = false;
        let mut in_text = false;

        let mut reader = Reader::from_str(xml_content);
        reader.trim_text(true);

        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let name = e.name();
                    // Check for paragraph start (w:p)
                    if name.as_ref() == b"w:p" {
                        // Save previous paragraph if it has content
                        if in_paragraph && !current_paragraph.trim().is_empty() {
                            paragraphs.push(current_paragraph.clone());
                        }
                        current_paragraph.clear();
                        in_paragraph = true;
                    }
                    // Check for text element (w:t)
                    else if name.as_ref() == b"w:t" {
                        in_text = true;
                    }
                }
                Ok(Event::End(e)) => {
                    let name = e.name();
                    if name.as_ref() == b"w:p" {
                        in_paragraph = false;
                    } else if name.as_ref() == b"w:t" {
                        in_text = false;
                    }
                }
                Ok(Event::Text(e)) => {
                    if in_text && in_paragraph {
                        if let Ok(text) = e.unescape() {
                            current_paragraph.push_str(&text);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    debug!("XML parsing error at position {}: {:?}", reader.buffer_position(), e);
                    break;
                }
                _ => {}
            }
            buf.clear();
        }

        // Add final paragraph if it has content
        if !current_paragraph.trim().is_empty() {
            paragraphs.push(current_paragraph);
        }

        paragraphs
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

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing DOCX file, size: {} bytes, filename: {:?}",
            context.size,
            context.filename
        );

        // Open DOCX as ZIP archive
        let cursor = Cursor::new(data.as_ref());
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open DOCX as ZIP: {}", e)))?;

        // Find and read word/document.xml
        let mut document_xml = String::new();
        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| Error::ParseError(format!("Failed to read ZIP entry: {}", e)))?;

            if file.name() == "word/document.xml" {
                use std::io::Read;
                file.read_to_string(&mut document_xml)
                    .map_err(|e| Error::ParseError(format!("Failed to read document.xml: {}", e)))?;
                break;
            }
        }

        if document_xml.is_empty() {
            return Err(Error::ParseError(
                "document.xml not found in DOCX file".to_string(),
            ));
        }

        debug!("Successfully read document.xml, size: {} bytes", document_xml.len());

        // Extract paragraphs from XML
        let paragraphs = Self::extract_text_from_xml(&document_xml);
        debug!("Extracted {} paragraphs from document", paragraphs.len());

        // Create pages with text blocks
        let mut pages = Vec::new();
        let mut content_blocks = Vec::new();

        for (idx, paragraph_text) in paragraphs.iter().enumerate() {
            if paragraph_text.is_empty() {
                continue;
            }

            let text_run = TextRun {
                text: paragraph_text.clone(),
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

            // Create a new page every 50 paragraphs (rough pagination)
            if (idx + 1) % 50 == 0 {
                let page = Page {
                    number: (pages.len() + 1) as u32,
                    dimensions: Dimensions::LETTER,
                    content: content_blocks.clone(),
                    annotations: vec![],
                    metadata: PageMetadata {
                        label: None,
                        rotation: 0,
                    },
                };
                pages.push(page);
                content_blocks.clear();
            }
        }

        // Add remaining content as the last page
        if !content_blocks.is_empty() {
            let page = Page {
                number: (pages.len() + 1) as u32,
                dimensions: Dimensions::LETTER,
                content: content_blocks,
                annotations: vec![],
                metadata: PageMetadata {
                    label: None,
                    rotation: 0,
                },
            };
            pages.push(page);
        }

        // If no pages were created, create one empty page
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

        // Create document metadata
        let mut metadata = Metadata::new();
        if let Some(filename) = context.filename {
            metadata.title = Some(filename);
        }
        metadata.add_custom("format", "DOCX");
        metadata.add_custom("paragraph_count", paragraphs.len() as i64);

        // Build document
        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;

        info!(
            "Successfully parsed DOCX with {} pages, {} paragraphs",
            document.page_count(),
            paragraphs.len()
        );

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

    #[test]
    fn test_can_parse() {
        let parser = DocxParser::new();

        // ZIP signature
        let zip_data = b"PK\x03\x04";
        assert!(!parser.can_parse(zip_data)); // Not a DOCX without word/document.xml

        // Not a ZIP
        assert!(!parser.can_parse(b"Not a ZIP file"));
    }

    #[test]
    fn test_extract_text() {
        let xml = r#"
            <w:p>
                <w:r><w:t>Hello</w:t></w:r>
                <w:r><w:t> World</w:t></w:r>
            </w:p>
            <w:p>
                <w:r><w:t>Second paragraph</w:t></w:r>
            </w:p>
        "#;

        let paragraphs = DocxParser::extract_text_from_xml(xml);
        assert_eq!(paragraphs.len(), 2);
        assert!(paragraphs[0].contains("Hello"));
        assert!(paragraphs[0].contains("World"));
        assert!(paragraphs[1].contains("Second paragraph"));
    }
}
