// SPDX-License-Identifier: AGPL-3.0-only
//! SVG and SVGZ image parsers
//!
//! Parses SVG (Scalable Vector Graphics) and SVGZ (compressed SVG) files
//! into the Unified Document Model. SVG content is passed through for
//! direct browser rendering.

use async_trait::async_trait;
use bytes::Bytes;
use flate2::read::GzDecoder;
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
use tracing::debug;

/// SVG image parser
#[derive(Debug, Clone)]
pub struct SvgParser;

impl SvgParser {
    /// Create a new SVG parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data starts with SVG markers
    #[must_use]
    fn is_svg(data: &[u8]) -> bool {
        if let Ok(text) = std::str::from_utf8(data) {
            let text_lower = text.to_lowercase();
            let text_trimmed = text_lower.trim_start();
            text_trimmed.starts_with("<?xml") && text_lower.contains("<svg")
                || text_trimmed.starts_with("<svg")
        } else {
            false
        }
    }
}

impl Default for SvgParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for SvgParser {
    fn format(&self) -> Format {
        Format::svg()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_svg(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing SVG image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid SVG content".to_string()));
        }

        let svg_content = String::from_utf8(data.to_vec())
            .map_err(|e| Error::ParseError(format!("Invalid UTF-8 in SVG: {e}")))?;

        // Extract viewBox dimensions if present
        let (width, height) = extract_svg_dimensions(&svg_content);

        // Pass through SVG content for direct rendering
        // Use special marker similar to HTML raw passthrough
        let text_run = TextRun {
            text: format!("__SVG_RAW__:{svg_content}"),
            style: prism_core::document::TextStyle::default(),
            bounds: Some(Rect::default()),
            char_positions: Some(Vec::new()),
        };

        let text_block = TextBlock {
            vertical_alignment: None,
            runs: vec![text_run],
            paragraph_style: None,
            bounds: Rect::default(),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        let page = Page {
            number: 1,
            dimensions: Dimensions { width, height },
            content: vec![ContentBlock::Text(text_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "SVG");

        let mut document = Document::builder().metadata(metadata).build();
        document.pages.push(page);

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "SVG Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::MetadataExtraction],
            requires_sandbox: false,
        }
    }
}

/// Extract width and height from SVG
fn extract_svg_dimensions(svg: &str) -> (f64, f64) {
    // Try to find width and height attributes
    let svg_lower = svg.to_lowercase();
    if let Some(start) = svg_lower.find("<svg") {
        let svg_tag = &svg[start..];
        if let Some(end) = svg_tag.find('>') {
            let tag_content = &svg_tag[..end];

            let width = extract_numeric_attr(tag_content, "width").unwrap_or(800.0);
            let height = extract_numeric_attr(tag_content, "height").unwrap_or(600.0);
            return (width, height);
        }
    }
    (800.0, 600.0) // Default dimensions
}

/// Extract a numeric attribute value
fn extract_numeric_attr(tag: &str, attr_name: &str) -> Option<f64> {
    let pattern = format!("{attr_name}=\"");
    if let Some(start) = tag.to_lowercase().find(&pattern) {
        let after = &tag[start + pattern.len()..];
        if let Some(end) = after.find('"') {
            let value = &after[..end];
            // Remove units like "px", "pt", etc.
            let numeric: String = value
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            return numeric.parse().ok();
        }
    }
    None
}

/// SVGZ (Gzip-compressed SVG) parser
#[derive(Debug, Clone)]
pub struct SvgzParser;

impl SvgzParser {
    /// Create a new SVGZ parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data starts with gzip signature
    #[must_use]
    fn is_gzip(data: &[u8]) -> bool {
        data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
    }
}

impl Default for SvgzParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for SvgzParser {
    fn format(&self) -> Format {
        Format::svgz()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_gzip(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        use std::io::Read;

        debug!(
            "Parsing SVGZ image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !Self::is_gzip(&data) {
            return Err(Error::ParseError(
                "Invalid SVGZ (gzip) signature".to_string(),
            ));
        }

        // Decompress the gzipped data
        let mut decoder = GzDecoder::new(&data[..]);
        let mut svg_content = String::new();
        decoder
            .read_to_string(&mut svg_content)
            .map_err(|e| Error::ParseError(format!("Failed to decompress SVGZ: {e}")))?;

        // Validate it's actually SVG
        if !SvgParser::is_svg(svg_content.as_bytes()) {
            return Err(Error::ParseError(
                "Decompressed content is not valid SVG".to_string(),
            ));
        }

        // Extract dimensions
        let (width, height) = extract_svg_dimensions(&svg_content);

        // Create SVG content block
        let text_run = TextRun {
            text: format!("__SVG_RAW__:{svg_content}"),
            style: prism_core::document::TextStyle::default(),
            bounds: Some(Rect::default()),
            char_positions: Some(Vec::new()),
        };

        let text_block = TextBlock {
            vertical_alignment: None,
            runs: vec![text_run],
            paragraph_style: None,
            bounds: Rect::default(),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        let page = Page {
            number: 1,
            dimensions: Dimensions { width, height },
            content: vec![ContentBlock::Text(text_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "SVGZ");

        let mut document = Document::builder().metadata(metadata).build();
        document.pages.push(page);

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "SVGZ Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::MetadataExtraction],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_svg() {
        assert!(SvgParser::is_svg(b"<svg xmlns=\"...\"></svg>"));
        assert!(SvgParser::is_svg(b"<?xml version=\"1.0\"?><svg></svg>"));
        assert!(!SvgParser::is_svg(b"<html></html>"));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = SvgParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "SVG Parser");
    }

    #[test]
    fn test_svgz_is_gzip() {
        assert!(SvgzParser::is_gzip(&[0x1f, 0x8b, 0x08]));
        assert!(!SvgzParser::is_gzip(b"<svg>"));
    }

    #[test]
    fn test_svgz_parser_metadata() {
        let parser = SvgzParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "SVGZ Parser");
    }
}
