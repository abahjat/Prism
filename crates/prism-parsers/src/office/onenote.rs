// SPDX-License-Identifier: AGPL-3.0-only
//! Microsoft `OneNote` parser
//!
//! Parses Microsoft `OneNote` section files (`.one`) into the Unified Document Model.
//! Uses the `onenote_parser` crate to extract pages and content.

use async_trait::async_trait;
use bytes::Bytes;
use onenote_parser::contents::{Content, OutlineItem};
use onenote_parser::page::PageContent;
use onenote_parser::Parser as OneNoteFileParser;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, ShapeStyle, TextBlock,
        TextRun,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::path::Path;
use tracing::{debug, warn};

/// Microsoft `OneNote` section parser
#[derive(Debug, Clone)]
pub struct OneNoteParser;

impl OneNoteParser {
    /// Create a new `OneNote` parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data appears to be a `OneNote` file
    ///
    /// `OneNote` files use the FSSHTTPB (File Synchronization via SOAP over HTTP)
    /// or `OneStore` format. We check for common signatures.
    #[must_use]
    fn is_onenote(data: &[u8]) -> bool {
        if data.len() < 16 {
            return false;
        }

        // Check for OneStore File Header GUID:
        // E4 52 5C 7B CE BB 4D 4A A4 5F 32 92 04 71 62 F4
        let onenote_guid = [
            0xE4, 0x52, 0x5C, 0x7B, 0xCE, 0xBB, 0x4D, 0x4A, 0xA4, 0x5F, 0x32, 0x92, 0x04, 0x71,
            0x62, 0xF4,
        ];
        data[0..16] == onenote_guid
    }
}

impl Default for OneNoteParser {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(clippy::too_many_lines)]
#[async_trait]
impl Parser for OneNoteParser {
    fn format(&self) -> Format {
        Format::onenote()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_onenote(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing OneNote file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Get filename for the parser (required by API)
        let filename = context.filename.as_deref().unwrap_or("unknown.one");
        let file_path = Path::new(filename);

        // Parse the OneNote file
        let mut onenote_parser = OneNoteFileParser::new();

        let section = onenote_parser
            .parse_section_buffer(&data, file_path)
            .map_err(|e| Error::ParseError(format!("Failed to parse OneNote section: {e}")))?;

        let mut pages = Vec::new();
        let section_name = section.display_name().to_string();

        debug!("Parsing OneNote section: {}", section_name);

        // Iterate over page series and extract pages
        let mut page_number = 0u32;
        for page_series in section.page_series() {
            for onenote_page in page_series.pages() {
                page_number += 1;

                // Get page title
                let page_title = onenote_page
                    .title_text()
                    .map_or_else(|| format!("Page {page_number}"), String::from);

                debug!("Processing OneNote page {}: {}", page_number, page_title);

                // Extract text content from page
                let mut text_parts: Vec<String> = Vec::new();
                text_parts.push(page_title.clone());
                text_parts.push(String::new());

                // Extract content from page contents
                for content in onenote_page.contents() {
                    extract_page_content_text(content, &mut text_parts);
                }

                let page_text = text_parts.join("\n");
                let mut page_content = Vec::new();

                if !page_text.is_empty() {
                    let text_run = TextRun {
                        text: page_text,
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

                    page_content.push(ContentBlock::Text(text_block));
                }

                let page = Page {
                    number: page_number,
                    dimensions: Dimensions::LETTER,
                    content: page_content,
                    metadata: PageMetadata {
                        label: Some(page_title),
                        ..PageMetadata::default()
                    },
                    annotations: Vec::new(),
                };

                pages.push(page);
            }
        }

        // If no pages were extracted, create an info page
        if pages.is_empty() {
            warn!("No pages found in OneNote file");
            let info_text = format!(
                "OneNote Section: {section_name}\n\n\
                No page content was extracted.\n\n\
                This may be due to:\n\
                - Empty sections\n\
                - Unsupported OneNote format variant\n\
                - File created with OneNote 2016 desktop (limited support)"
            );

            let text_run = TextRun {
                text: info_text,
                style: prism_core::document::TextStyle::default(),
                bounds: None,
                char_positions: None,
            };

            let text_block = TextBlock {
                runs: vec![text_run],
                paragraph_style: None,
                vertical_alignment: None,
                bounds: Rect::new(50.0, 50.0, 600.0, 300.0),
                style: ShapeStyle::default(),
                rotation: 0.0,
            };

            pages.push(Page {
                number: 1,
                dimensions: Dimensions::LETTER,
                content: vec![ContentBlock::Text(text_block)],
                metadata: PageMetadata::default(),
                annotations: Vec::new(),
            });
        }

        // Create metadata
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "OneNote");
        metadata.add_custom("section_name", section_name);
        metadata.add_custom("pages", pages.len().to_string());

        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;

        debug!(
            "Successfully parsed OneNote file with {} pages",
            document.pages.len()
        );

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "OneNote Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// Extract text from `PageContent` enum variants
fn extract_page_content_text(content: &PageContent, parts: &mut Vec<String>) {
    match content {
        PageContent::Outline(outline) => {
            // Extract text from outline items
            for item in outline.items() {
                extract_outline_item_text(item, parts);
            }
        }
        PageContent::Image(_) => {
            parts.push("[Image]".to_string());
        }
        PageContent::EmbeddedFile(file) => {
            let name = file.filename();
            if !name.is_empty() {
                parts.push(format!("[Embedded: {name}]"));
            }
        }
        PageContent::Ink(_) => {
            parts.push("[Handwritten content]".to_string());
        }
        PageContent::Unknown => {
            // Skip unknown content
        }
    }
}

/// Extract text from outline items recursively
fn extract_outline_item_text(item: &OutlineItem, parts: &mut Vec<String>) {
    match item {
        OutlineItem::Group(group) => {
            // Groups contain outline items, recurse into them
            for outline in group.outlines() {
                extract_outline_item_text(outline, parts);
            }
        }
        OutlineItem::Element(element) => {
            // Elements contain actual content
            for content in element.contents() {
                extract_content_text(content, parts);
            }
            // Recursively handle children
            for child in element.children() {
                extract_outline_item_text(child, parts);
            }
        }
    }
}

/// Extract text from Content enum (`RichText`, Table, Image, etc.)
fn extract_content_text(content: &Content, parts: &mut Vec<String>) {
    match content {
        Content::RichText(rich_text) => {
            let text = rich_text.text();
            if !text.is_empty() {
                parts.push(text.to_string());
            }
        }
        Content::Table(_) => {
            // Tables have complex structure - add placeholder for now
            parts.push("[Table]".to_string());
        }
        Content::Image(_) => {
            parts.push("[Image]".to_string());
        }
        Content::EmbeddedFile(file) => {
            let name = file.filename();
            if !name.is_empty() {
                parts.push(format!("[Embedded: {name}]"));
            }
        }
        Content::Ink(_) => {
            parts.push("[Handwritten content]".to_string());
        }
        Content::Unknown => {
            // Skip unknown content
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_metadata() {
        let parser = OneNoteParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "OneNote Parser");
    }

    #[test]
    fn test_is_onenote_empty() {
        assert!(!OneNoteParser::is_onenote(&[]));
        assert!(!OneNoteParser::is_onenote(&[0u8; 10]));
    }

    #[test]
    fn test_is_onenote_signature() {
        // Test with OneStore GUID signature
        let onenote_header = [
            0xE4, 0x52, 0x5C, 0x7B, 0xCE, 0xBB, 0x4D, 0x4A, 0xA4, 0x5F, 0x32, 0x92, 0x04, 0x71,
            0x62, 0xF4, 0x00, 0x00, 0x00, 0x00,
        ];
        assert!(OneNoteParser::is_onenote(&onenote_header));
    }
}
