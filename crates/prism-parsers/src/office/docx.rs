//! DOCX (Microsoft Word) parser
//!
//! Parses DOCX files into the Unified Document Model with formatting preservation.

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
/// Preserves text formatting including bold, italic, underline, font, size, and color.
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

    /// Extract text and formatting from document.xml using proper XML parsing
    fn extract_formatted_text(xml_content: &str) -> Vec<TextBlock> {
        let mut blocks = Vec::new();
        let mut current_runs = Vec::new();
        let mut current_text = String::new();

        let mut in_paragraph = false;
        let mut in_run = false;
        let mut in_text = false;
        let mut in_run_properties = false;

        // Current run formatting state
        let mut is_bold = false;
        let mut is_italic = false;
        let mut is_underline = false;
        let mut font_size: Option<f64> = None;
        let mut font_family: Option<String> = None;
        let mut color: Option<String> = None;

        let mut paragraph_count = 0;

        let mut reader = Reader::from_str(xml_content);
        reader.trim_text(false); // Don't trim to preserve spaces

        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let name = e.name();

                    // Paragraph start
                    if name.as_ref() == b"w:p" {
                        paragraph_count += 1;
                        // Save previous paragraph if it has content
                        if in_paragraph && !current_runs.is_empty() {
                            blocks.push(TextBlock {
                                runs: current_runs.clone(),
                                paragraph_style: None,
                                bounds: prism_core::document::Rect::default(),
                            });
                            current_runs.clear();
                        }
                        in_paragraph = true;
                    }
                    // Run start (text run with formatting)
                    else if name.as_ref() == b"w:r" {
                        // Save previous run if it has text
                        if in_run && !current_text.is_empty() {
                            let style = TextStyle {
                                font_family: font_family.clone(),
                                font_size,
                                bold: is_bold,
                                italic: is_italic,
                                underline: is_underline,
                                strikethrough: false,
                                color: color.clone(),
                                background_color: None,
                            };
                            current_runs.push(TextRun {
                                text: current_text.clone(),
                                style,
                                bounds: None,
                                char_positions: None,
                            });
                            current_text.clear();
                        }
                        in_run = true;
                        // Reset styling for new run
                        is_bold = false;
                        is_italic = false;
                        is_underline = false;
                        font_size = None;
                        font_family = None;
                        color = None;
                    }
                    // Run properties
                    else if name.as_ref() == b"w:rPr" {
                        in_run_properties = true;
                    }
                    // Bold
                    else if name.as_ref() == b"w:b" && in_run_properties {
                        is_bold = true;
                    }
                    // Italic
                    else if name.as_ref() == b"w:i" && in_run_properties {
                        is_italic = true;
                    }
                    // Underline
                    else if name.as_ref() == b"w:u" && in_run_properties {
                        is_underline = true;
                    }
                    // Font size (w:sz val="24" means 12pt - value is in half-points)
                    else if name.as_ref() == b"w:sz" && in_run_properties {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"w:val" {
                                if let Ok(val) = String::from_utf8(attr.value.to_vec()) {
                                    if let Ok(half_points) = val.parse::<f64>() {
                                        font_size = Some(half_points / 2.0);
                                    }
                                }
                            }
                        }
                    }
                    // Font family
                    else if name.as_ref() == b"w:rFonts" && in_run_properties {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"w:ascii" {
                                if let Ok(val) = String::from_utf8(attr.value.to_vec()) {
                                    font_family = Some(val);
                                }
                            }
                        }
                    }
                    // Color
                    else if name.as_ref() == b"w:color" && in_run_properties {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"w:val" {
                                if let Ok(val) = String::from_utf8(attr.value.to_vec()) {
                                    if val != "auto" {
                                        color = Some(format!("#{}", val));
                                    }
                                }
                            }
                        }
                    }
                    // Text element
                    else if name.as_ref() == b"w:t" {
                        in_text = true;
                    }
                }
                Ok(Event::End(e)) => {
                    let name = e.name();

                    if name.as_ref() == b"w:p" {
                        // Save run if it has text
                        if in_run && !current_text.is_empty() {
                            let style = TextStyle {
                                font_family: font_family.clone(),
                                font_size,
                                bold: is_bold,
                                italic: is_italic,
                                underline: is_underline,
                                strikethrough: false,
                                color: color.clone(),
                                background_color: None,
                            };
                            current_runs.push(TextRun {
                                text: current_text.clone(),
                                style,
                                bounds: None,
                                char_positions: None,
                            });
                            current_text.clear();
                        }

                        // Save paragraph
                        if !current_runs.is_empty() {
                            blocks.push(TextBlock {
                                runs: current_runs.clone(),
                                paragraph_style: None,
                                bounds: prism_core::document::Rect::default(),
                            });
                            current_runs.clear();
                        }
                        in_paragraph = false;
                        in_run = false;
                    }
                    else if name.as_ref() == b"w:r" {
                        // Save run when it ends
                        if !current_text.is_empty() {
                            let style = TextStyle {
                                font_family: font_family.clone(),
                                font_size,
                                bold: is_bold,
                                italic: is_italic,
                                underline: is_underline,
                                strikethrough: false,
                                color: color.clone(),
                                background_color: None,
                            };
                            current_runs.push(TextRun {
                                text: current_text.clone(),
                                style,
                                bounds: None,
                                char_positions: None,
                            });
                            current_text.clear();
                        }
                        in_run = false;
                    }
                    else if name.as_ref() == b"w:rPr" {
                        in_run_properties = false;
                    }
                    else if name.as_ref() == b"w:t" {
                        in_text = false;
                    }
                }
                Ok(Event::Text(e)) => {
                    if in_text && in_run {
                        if let Ok(text) = e.unescape() {
                            current_text.push_str(&text);
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

        // Add final elements if needed
        if in_run && !current_text.is_empty() {
            let style = TextStyle {
                font_family,
                font_size,
                bold: is_bold,
                italic: is_italic,
                underline: is_underline,
                strikethrough: false,
                color,
                background_color: None,
            };
            current_runs.push(TextRun {
                text: current_text,
                style,
                bounds: None,
                char_positions: None,
            });
        }

        if !current_runs.is_empty() {
            blocks.push(TextBlock {
                runs: current_runs,
                paragraph_style: None,
                bounds: prism_core::document::Rect::default(),
            });
        }

        debug!("Found {} w:p tags, extracted {} text blocks with formatting", paragraph_count, blocks.len());
        blocks
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

        // Extract text blocks with formatting
        let text_blocks = Self::extract_formatted_text(&document_xml);
        debug!("Extracted {} text blocks from document", text_blocks.len());

        // Create pages with text blocks
        let mut pages = Vec::new();
        let mut content_blocks = Vec::new();

        for (idx, text_block) in text_blocks.iter().enumerate() {
            content_blocks.push(ContentBlock::Text(text_block.clone()));

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
        metadata.add_custom("paragraph_count", text_blocks.len() as i64);

        // Build document
        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;

        info!(
            "Successfully parsed DOCX with {} pages, {} paragraphs",
            document.page_count(),
            text_blocks.len()
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
    fn test_extract_formatted_text() {
        let xml = r#"
            <w:p>
                <w:r><w:rPr><w:b/></w:rPr><w:t>Bold text</w:t></w:r>
                <w:r><w:t> Normal text</w:t></w:r>
            </w:p>
        "#;

        let blocks = DocxParser::extract_formatted_text(xml);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].runs.len(), 2);
        assert!(blocks[0].runs[0].style.bold);
        assert!(!blocks[0].runs[1].style.bold);
    }
}
