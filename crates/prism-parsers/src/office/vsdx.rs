// SPDX-License-Identifier: AGPL-3.0-only
//! Microsoft Visio parser
//!
//! Parses Microsoft Visio `.vsdx` files into the Unified Document Model.
//! VSDX files are ZIP archives containing XML (Open Packaging Conventions).

use async_trait::async_trait;
use bytes::Bytes;
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
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::{Cursor, Read};
use tracing::{debug, warn};
use zip::ZipArchive;

/// Microsoft Visio (VSDX) parser
#[derive(Debug, Clone)]
pub struct VsdxParser;

impl VsdxParser {
    /// Create a new VSDX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data is a valid VSDX file (ZIP with visio/ directory)
    #[must_use]
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    fn is_vsdx_zip(data: &[u8]) -> bool {
        if data.len() < 4 || &data[0..2] != b"PK" {
            return false;
        }

        let cursor = Cursor::new(data);
        if let Ok(mut archive) = ZipArchive::new(cursor) {
            for i in 0..archive.len() {
                if let Ok(file) = archive.by_index(i) {
                    let file_name = file.name().to_string();
                    // VSDX files have visio/pages/ directory with page XML files
                    if file_name.starts_with("visio/pages/page") && file_name.ends_with(".xml") {
                        return true;
                    }
                }
            }
            // Check for Content_Types with visio MIME type
            if let Ok(mut f) = archive.by_name("[Content_Types].xml") {
                let mut content = String::new();
                if f.read_to_string(&mut content).is_ok()
                    && content.contains("application/vnd.ms-visio")
                {
                    return true;
                }
            }
        }
        false
    }
}

impl Default for VsdxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for VsdxParser {
    fn format(&self) -> Format {
        Format::vsdx()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_vsdx_zip(data)
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing VSDX file: {:?}", context.filename);

        let cursor = Cursor::new(&data[..]);
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open VSDX as ZIP: {e}")))?;

        let mut pages = Vec::new();
        let mut all_text = Vec::new();

        // Find all page XML files
        let page_files: Vec<String> = (0..archive.len())
            .filter_map(|i| {
                archive.by_index(i).ok().and_then(|f| {
                    let name = f.name().to_string();
                    if name.starts_with("visio/pages/page") && name.ends_with(".xml") {
                        Some(name)
                    } else {
                        None
                    }
                })
            })
            .collect();

        debug!("Found {} Visio pages", page_files.len());

        // Parse each page
        for (page_idx, page_file) in page_files.iter().enumerate() {
            let page_number = u32::try_from(page_idx + 1).unwrap_or(1);

            let mut page_xml = String::new();
            if let Ok(mut file) = archive.by_name(page_file) {
                let _ = file.read_to_string(&mut page_xml);
            }

            if page_xml.is_empty() {
                continue;
            }

            // Extract text from page XML
            let page_text_parts = extract_text_from_visio_xml(&page_xml);
            let page_text = page_text_parts.join("\n");
            all_text.extend(page_text_parts.clone());

            let page_title = format!("Page {page_number}");

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

        // If no pages with content, create a summary page
        if pages.is_empty() || all_text.is_empty() {
            warn!("No text content found in Visio file");
            let info_text = "Visio diagram parsed successfully.\n\n\
                No text content was extracted from shapes.\n\n\
                The diagram may contain only graphical elements.";

            let text_run = TextRun {
                text: info_text.to_string(),
                style: prism_core::document::TextStyle::default(),
                bounds: None,
                char_positions: None,
            };

            let text_block = TextBlock {
                runs: vec![text_run],
                paragraph_style: None,
                bounds: Rect::new(50.0, 50.0, 600.0, 300.0),
                style: ShapeStyle::default(),
                rotation: 0.0,
            };

            if pages.is_empty() {
                pages.push(Page {
                    number: 1,
                    dimensions: Dimensions::LETTER,
                    content: vec![ContentBlock::Text(text_block)],
                    metadata: PageMetadata::default(),
                    annotations: Vec::new(),
                });
            }
        }

        // Extract metadata from docProps/core.xml if available
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }

        if let Ok(mut core_file) = archive.by_name("docProps/core.xml") {
            let mut core_xml = String::new();
            if core_file.read_to_string(&mut core_xml).is_ok() {
                extract_vsdx_metadata(&core_xml, &mut metadata);
            }
        }

        metadata.add_custom("format", "Microsoft Visio");
        metadata.add_custom("pages", pages.len().to_string());

        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;

        debug!(
            "Successfully parsed VSDX file with {} pages",
            document.pages.len()
        );

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "Visio Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// Extract text content from Visio page XML
fn extract_text_from_visio_xml(xml: &str) -> Vec<String> {
    let mut texts = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);

    let mut in_text = false;
    let mut in_value = false;
    let mut current_text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local_name = e.name().local_name();
                let local = local_name.as_ref();
                // Look for Text elements in Visio XML
                if local == b"Text" || local == b"t" {
                    in_text = true;
                    current_text.clear();
                }
                // Also look for Value elements which can contain labels
                if local == b"Value" {
                    in_value = true;
                    current_text.clear();
                }
            }
            Ok(Event::Text(e)) => {
                if in_text || in_value {
                    if let Ok(text) = e.unescape() {
                        let text = text.trim();
                        if !text.is_empty() {
                            current_text.push_str(text);
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let local_name = e.name().local_name();
                let local = local_name.as_ref();
                if local == b"Text" || local == b"t" {
                    if !current_text.is_empty() {
                        texts.push(current_text.clone());
                    }
                    in_text = false;
                }
                if local == b"Value" {
                    if !current_text.is_empty() && !texts.contains(&current_text) {
                        texts.push(current_text.clone());
                    }
                    in_value = false;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    texts
}

/// Extract metadata from docProps/core.xml
fn extract_vsdx_metadata(xml: &str, metadata: &mut Metadata) {
    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);

    let mut current_element = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                current_element = String::from_utf8_lossy(name.local_name().as_ref()).to_string();
            }
            Ok(Event::Text(e)) => {
                if let Ok(text) = e.unescape() {
                    let text = text.trim();
                    if !text.is_empty() {
                        match current_element.as_str() {
                            "title" => metadata.title = Some(text.to_string()),
                            "creator" => metadata.author = Some(text.to_string()),
                            "subject" | "description" => {
                                metadata.subject = Some(text.to_string());
                            }
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                current_element.clear();
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_metadata() {
        let parser = VsdxParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "Visio Parser");
    }

    #[test]
    fn test_is_vsdx_empty() {
        assert!(!VsdxParser::is_vsdx_zip(&[]));
        assert!(!VsdxParser::is_vsdx_zip(&[0u8; 10]));
    }

    #[test]
    fn test_extract_text_basic() {
        let xml = r"<Page><Text>Hello World</Text></Page>";
        let texts = extract_text_from_visio_xml(xml);
        assert_eq!(texts, vec!["Hello World"]);
    }
}
