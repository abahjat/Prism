//! PPTX (Microsoft PowerPoint) parser
//!
//! Parses PPTX files into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, TextBlock,
        TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::{debug, info};
use zip::ZipArchive;

/// PPTX parser
///
/// Parses Microsoft PowerPoint PPTX files into the Unified Document Model.
/// Each slide becomes a separate page in the document.
#[derive(Debug, Clone)]
pub struct PptxParser;

impl PptxParser {
    /// Create a new PPTX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data is a valid PPTX file (ZIP with ppt/ directory)
    fn is_pptx_zip(data: &[u8]) -> bool {
        // Check ZIP signature: PK (0x504B)
        if data.len() < 4 {
            return false;
        }

        if &data[0..2] != b"PK" {
            return false;
        }

        // Try to open as ZIP and check for ppt/ directory
        let cursor = std::io::Cursor::new(data);
        if let Ok(mut archive) = ZipArchive::new(cursor) {
            // Check for ppt/presentation.xml which is present in PPTX files
            for i in 0..archive.len() {
                if let Ok(file) = archive.by_index(i) {
                    let name = file.name();
                    if name == "ppt/presentation.xml" || name.starts_with("ppt/slides/slide") {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Extract text from slide XML
    fn extract_text_from_slide_xml(xml_content: &str) -> Vec<String> {
        let mut text_blocks = Vec::new();
        let mut current_text = String::new();
        let mut in_text_tag = false;

        // Parse PowerPoint slide XML - look for <a:t> tags which contain text
        for line in xml_content.lines() {
            let trimmed = line.trim();

            // Extract text from <a:t> tags (text runs)
            if let Some(start_idx) = trimmed.find("<a:t>") {
                if let Some(end_idx) = trimmed.find("</a:t>") {
                    let text = &trimmed[start_idx + 5..end_idx];
                    if !text.is_empty() {
                        current_text.push_str(text);
                        current_text.push(' ');
                    }
                    in_text_tag = false;
                } else {
                    let text = &trimmed[start_idx + 5..];
                    current_text.push_str(text);
                    in_text_tag = true;
                }
            } else if in_text_tag {
                if let Some(end_idx) = trimmed.find("</a:t>") {
                    let text = &trimmed[..end_idx];
                    current_text.push_str(text);
                    current_text.push(' ');
                    in_text_tag = false;
                } else {
                    current_text.push_str(trimmed);
                }
            }

            // Check for paragraph/shape boundaries to create separate text blocks
            if trimmed.contains("</a:p>") || trimmed.contains("</p:sp>") {
                if !current_text.trim().is_empty() {
                    text_blocks.push(current_text.trim().to_string());
                    current_text.clear();
                }
            }
        }

        // Add final text block
        if !current_text.trim().is_empty() {
            text_blocks.push(current_text.trim().to_string());
        }

        text_blocks
    }
}

impl Default for PptxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PptxParser {
    fn format(&self) -> Format {
        Format::pptx()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_pptx_zip(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing PPTX file, size: {} bytes, filename: {:?}",
            context.size,
            context.filename
        );

        // Open PPTX as ZIP archive
        let cursor = Cursor::new(data.as_ref());
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open PPTX as ZIP: {}", e)))?;

        // Find all slide files (ppt/slides/slide1.xml, slide2.xml, etc.)
        let mut slide_files = Vec::new();
        for i in 0..archive.len() {
            let file = archive
                .by_index(i)
                .map_err(|e| Error::ParseError(format!("Failed to read ZIP entry: {}", e)))?;

            let name = file.name().to_string();
            if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                // Extract slide number from filename
                if let Some(num_str) = name
                    .strip_prefix("ppt/slides/slide")
                    .and_then(|s| s.strip_suffix(".xml"))
                {
                    if let Ok(slide_num) = num_str.parse::<usize>() {
                        slide_files.push((slide_num, name));
                    }
                }
            }
        }

        // Sort slides by number
        slide_files.sort_by_key(|(num, _)| *num);

        debug!("Found {} slides in presentation", slide_files.len());

        // Parse each slide
        let mut pages = Vec::new();
        for (slide_num, slide_name) in slide_files {
            // Read slide XML
            let mut slide_xml = String::new();
            for i in 0..archive.len() {
                let mut file = archive
                    .by_index(i)
                    .map_err(|e| Error::ParseError(format!("Failed to read ZIP entry: {}", e)))?;

                if file.name() == slide_name {
                    use std::io::Read;
                    file.read_to_string(&mut slide_xml)
                        .map_err(|e| Error::ParseError(format!("Failed to read slide XML: {}", e)))?;
                    break;
                }
            }

            if slide_xml.is_empty() {
                debug!("Slide {} is empty, skipping", slide_num);
                continue;
            }

            // Extract text from slide
            let text_blocks = Self::extract_text_from_slide_xml(&slide_xml);

            // Create content blocks for this slide
            let mut content_blocks = Vec::new();
            for text in text_blocks {
                if text.is_empty() {
                    continue;
                }

                let text_run = TextRun {
                    text,
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

            // Create page for this slide
            let page = Page {
                number: slide_num as u32,
                dimensions: Dimensions::LETTER, // PowerPoint slides
                content: content_blocks,
                annotations: vec![],
                metadata: PageMetadata {
                    label: Some(format!("Slide {}", slide_num)),
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
                    label: Some("Slide 1".to_string()),
                    rotation: 0,
                },
            });
        }

        // Create document metadata
        let mut metadata = Metadata::new();
        if let Some(filename) = context.filename {
            metadata.title = Some(filename);
        }
        metadata.add_custom("format", "PPTX");
        metadata.add_custom("slide_count", pages.len() as i64);

        // Build document
        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;

        info!(
            "Successfully parsed PPTX with {} slides",
            document.page_count()
        );

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PPTX Parser".to_string(),
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
        let parser = PptxParser::new();

        // ZIP signature
        let zip_data = b"PK\x03\x04";
        assert!(!parser.can_parse(zip_data)); // Not a PPTX without ppt/ directory

        // Not a ZIP
        assert!(!parser.can_parse(b"Not a ZIP file"));
    }

    #[test]
    fn test_extract_text() {
        let xml = r#"
            <a:p>
                <a:r><a:t>Title Text</a:t></a:r>
            </a:p>
            <a:p>
                <a:r><a:t>Body content</a:t></a:r>
            </a:p>
        "#;

        let text_blocks = PptxParser::extract_text_from_slide_xml(xml);
        assert_eq!(text_blocks.len(), 2);
        assert!(text_blocks[0].contains("Title Text"));
        assert!(text_blocks[1].contains("Body content"));
    }
}
