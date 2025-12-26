// SPDX-License-Identifier: AGPL-3.0-only
//! HTML file parser
//!
//! Parses HTML files into the Unified Document Model.
//! For HTML files, we preserve the raw HTML content and render it directly.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{ContentBlock, Dimensions, Document, Page, Rect, TextBlock, TextRun, TextStyle},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use tracing::{debug, info};

/// HTML file parser
///
/// Parses HTML files into the Unified Document Model.
/// The HTML content is preserved and can be rendered directly.
#[derive(Debug, Clone)]
pub struct HtmlParser;

impl HtmlParser {
    /// Create a new HTML parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Extract title from HTML if present
    fn extract_title(html: &str) -> Option<String> {
        // Simple title extraction - look for <title>...</title>
        if let Some(start_idx) = html.find("<title>") {
            let after_start = &html[start_idx + 7..];
            if let Some(end_idx) = after_start.find("</title>") {
                let title = &after_start[..end_idx];
                return Some(title.trim().to_string());
            }
        }
        None
    }

    /// Check if data starts with common HTML markers
    fn starts_with_html(data: &[u8]) -> bool {
        let text = match std::str::from_utf8(data) {
            Ok(t) => t.trim_start(),
            Err(_) => return false,
        };

        // Check for common HTML start patterns (case-insensitive)
        let text_lower = text.to_lowercase();
        text_lower.starts_with("<!doctype html")
            || text_lower.starts_with("<html")
            || text_lower.starts_with("<!doctype")
    }
}

impl Default for HtmlParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for HtmlParser {
    fn format(&self) -> Format {
        Format::html()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // Check for HTML markers
        if Self::starts_with_html(data) {
            return true;
        }

        // Check for common HTML tags in the first 1KB
        let check_len = data.len().min(1024);
        if let Ok(text) = std::str::from_utf8(&data[..check_len]) {
            let text_lower = text.to_lowercase();
            // Look for common HTML tags
            return text_lower.contains("<html")
                || text_lower.contains("<!doctype")
                || (text_lower.contains("<head") && text_lower.contains("<body"));
        }

        false
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing HTML file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Convert to string
        let html_content = String::from_utf8(data.to_vec())
            .map_err(|e| Error::ParseError(format!("Invalid UTF-8 in HTML file: {}", e)))?;

        // Extract title from HTML if present
        let title = Self::extract_title(&html_content);

        // For HTML, we'll store the raw HTML as a text block
        // The HTML renderer will handle displaying it properly
        let text_run = TextRun {
            text: html_content.clone(),
            style: TextStyle::default(),
            bounds: Some(Rect::default()),
            char_positions: Some(Vec::new()),
        };

        let text_block = TextBlock {
            runs: vec![text_run],
            paragraph_style: None,
            bounds: Rect::default(),
            style: prism_core::document::ShapeStyle::default(),
            rotation: 0.0,
        };

        // Create a single page with the HTML content
        // Use a wide page to accommodate HTML content
        let page = Page {
            number: 1,
            dimensions: Dimensions {
                width: 850.0, // Wider than standard to fit HTML content
                height: 1100.0,
            },
            content: vec![ContentBlock::Text(text_block)],
            metadata: Default::default(),
            annotations: Vec::new(),
        };

        // Create metadata
        let mut metadata = Metadata::default();
        metadata.title = title.or_else(|| context.filename.clone());
        metadata.add_custom("format", "HTML");
        metadata.add_custom("content_type", "text/html");

        // Create document
        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;

        info!("Successfully parsed HTML file");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "HTML Parser".to_string(),
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
    fn test_can_parse_html5() {
        let parser = HtmlParser::new();

        let html =
            b"<!DOCTYPE html>\n<html><head><title>Test</title></head><body>Content</body></html>";
        assert!(parser.can_parse(html));
    }

    #[test]
    fn test_can_parse_html_with_case() {
        let parser = HtmlParser::new();

        let html =
            b"<!doctype html>\n<HTML><HEAD><TITLE>Test</TITLE></HEAD><BODY>Content</BODY></HTML>";
        assert!(parser.can_parse(html));
    }

    #[test]
    fn test_can_parse_html_fragment() {
        let parser = HtmlParser::new();

        let html =
            b"<html>\n<head><title>Test</title></head>\n<body><h1>Hello</h1></body>\n</html>";
        assert!(parser.can_parse(html));
    }

    #[test]
    fn test_cannot_parse_plain_text() {
        let parser = HtmlParser::new();

        let text = b"This is just plain text without any HTML tags";
        assert!(!parser.can_parse(text));
    }

    #[test]
    fn test_extract_title() {
        let html = "<html><head><title>My Page Title</title></head><body>Content</body></html>";
        let title = HtmlParser::extract_title(html);
        assert_eq!(title, Some("My Page Title".to_string()));
    }

    #[test]
    fn test_extract_title_with_whitespace() {
        let html = "<html><head><title>  My Page  </title></head><body>Content</body></html>";
        let title = HtmlParser::extract_title(html);
        assert_eq!(title, Some("My Page".to_string()));
    }

    #[test]
    fn test_extract_no_title() {
        let html = "<html><head></head><body>Content</body></html>";
        let title = HtmlParser::extract_title(html);
        assert_eq!(title, None);
    }

    #[test]
    fn test_parser_metadata() {
        let parser = HtmlParser::new();
        let metadata = parser.metadata();

        assert_eq!(metadata.name, "HTML Parser");
        assert!(!metadata.requires_sandbox);
        assert!(!metadata.features.is_empty());
    }
}
