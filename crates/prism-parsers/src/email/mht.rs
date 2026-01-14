// SPDX-License-Identifier: AGPL-3.0-only
//! MHT (MIME HTML) parser
//!
//! Parses .MHT/.MHTML files (MIME HTML archives) into the Unified Document Model.
//! Extracts the main HTML content and displays it.

use async_trait::async_trait;
use bytes::Bytes;
use mail_parser::MessageParser;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use tracing::{debug, info};

/// MHT file parser
#[derive(Debug, Clone)]
pub struct MhtParser;

impl MhtParser {
    /// Create a new MHT parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for MhtParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for MhtParser {
    fn format(&self) -> Format {
        Format::mht()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // Check for MIME structure and "multipart/related" or MHTML hints
        let text = String::from_utf8_lossy(&data[..data.len().min(4096)]);

        // Basic MIME check
        if !text.contains("MIME-Version:") {
            return false;
        }

        // Look for MHT specific headers or Content-Type
        text.contains("multipart/related")
            || text.contains("Content-Location:")
            || text.contains("Snapshot-Content-Location:")
    }

    #[allow(clippy::too_many_lines)]
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing MHT file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Parse MIME message
        let message = MessageParser::default()
            .parse(&data[..])
            .ok_or_else(|| Error::ParseError("Failed to parse MHT structure".to_string()))?;

        // MHT files usually have a main HTML body
        // We prioritize HTML body
        let html_content = if let Some(html) = message.body_html(0) {
            html.to_string()
        } else if let Some(text) = message.body_text(0) {
            // Fallback to text if no HTML found
            // Wrap in basic HTML structure
            format!("<html><body><pre>{text}</pre></body></html>")
        } else {
            return Err(Error::ParseError(
                "No content found in MHT file".to_string(),
            ));
        };

        // Extract title
        let title = if let Some(subject) = message.subject() {
            Some(subject.to_string())
        } else {
            // Try to extract title from HTML
            html_content.find("<title>").and_then(|start| {
                let rest = &html_content[start + 7..];
                rest.find("</title>")
                    .map(|end| rest[..end].trim().to_string())
            })
        };

        // For MHT, we follow the same pattern as HTML parser:
        // Pass raw HTML with the special prefix so the renderer knows to render it.
        // Note: Embedded images (cid:) likely won't resolve without resolving logic.
        // For MVP, we provide the HTML as-is.

        let text_run = TextRun {
            text: format!("__HTML_RAW__:{html_content}"),
            style: TextStyle::default(),
            bounds: Some(Rect::default()),
            char_positions: Some(Vec::new()),
        };

        let text_block = TextBlock {
            runs: vec![text_run],
            bounds: Rect::default(),
            paragraph_style: None,
            vertical_alignment: None,
            style: prism_core::document::ShapeStyle::default(),
            rotation: 0.0,
        };

        // Page setup
        let page = Page {
            number: 1,
            dimensions: Dimensions {
                width: 850.0,
                height: 1100.0,
            },
            content: vec![ContentBlock::Text(text_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        // Metadata
        let mut metadata = Metadata {
            title: title.or_else(|| context.filename.clone()),
            ..Metadata::default()
        };
        metadata.add_custom("format", "MHT");
        metadata.add_custom("content_type", "multipart/related");

        if let Some(date) = message.date() {
            metadata.add_custom("date", date.to_rfc3339());
        }

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;

        info!("Successfully parsed MHT file");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "MHT Parser".to_string(),
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
    fn test_can_parse_mht() {
        let parser = MhtParser::new();
        let data = b"MIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"boundary\"\r\n\r\n--boundary\r\nContent-Type: text/html\r\n\r\n<html><body>Test</body></html>\r\n--boundary--";
        assert!(parser.can_parse(data));
    }
}
