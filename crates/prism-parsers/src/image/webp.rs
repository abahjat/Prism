// SPDX-License-Identifier: AGPL-3.0-only
//! WebP image parser
//!
//! Parses WebP images into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use image::ImageFormat;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, ImageBlock, ImageResource, Page, PageMetadata, Rect,
        ShapeStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::debug;

/// WebP image parser
///
/// Parses WebP files into the Unified Document Model.
/// Creates a single-page document containing the image.
#[derive(Debug, Clone)]
pub struct WebpParser;

impl WebpParser {
    /// Create a new WebP parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data has WebP signature (RIFF....WEBP)
    #[must_use]
    fn is_webp(data: &[u8]) -> bool {
        if data.len() < 12 {
            return false;
        }
        // WebP has RIFF at offset 0 and WEBP at offset 8
        data.starts_with(b"RIFF") && &data[8..12] == b"WEBP"
    }
}

impl Default for WebpParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for WebpParser {
    fn format(&self) -> Format {
        Format::webp()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_webp(data)
    }

    /// # Errors
    ///
    /// Returns an error if the WebP data is invalid or cannot be decoded.
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing WebP image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Validate WebP signature
        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid WebP signature".to_string()));
        }

        // Decode WebP image to get dimensions
        let cursor = Cursor::new(&data);
        let img = image::load(cursor, ImageFormat::WebP)
            .map_err(|e| Error::ParseError(format!("Failed to decode WebP: {e}")))?;

        let width = img.width();
        let height = img.height();

        debug!("WebP dimensions: {width}x{height}");

        // Create resource ID for the image
        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        // Create image resource
        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/webp".to_string(),
            data: Some(data.to_vec()),
            url: None,
            width,
            height,
        };

        // Create image block
        let image_block = ImageBlock {
            resource_id: resource_id.clone(),
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            alt_text: context.filename.clone(),
            format: Some("WebP".to_string()),
            original_size: Some(Dimensions {
                width: f64::from(width),
                height: f64::from(height),
            }),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        // Create page with image dimensions
        let dimensions = Dimensions {
            width: f64::from(width),
            height: f64::from(height),
        };

        let page = Page {
            number: 1,
            dimensions,
            content: vec![ContentBlock::Image(image_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        // Build metadata
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "WebP");
        metadata.add_custom("width", width.to_string());
        metadata.add_custom("height", height.to_string());

        // Build document
        let mut document = Document::builder().metadata(metadata).build();
        document.pages.push(page);
        document.resources.images.push(image_resource);

        debug!("WebP parsing complete");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "WebP Parser".to_string(),
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
    fn test_is_webp() {
        // Valid WebP header: RIFF + 4 bytes size + WEBP
        let valid = b"RIFF\x00\x00\x00\x00WEBP";
        assert!(WebpParser::is_webp(valid));

        // Invalid - just RIFF
        assert!(!WebpParser::is_webp(b"RIFF\x00\x00\x00\x00WAVE"));
        assert!(!WebpParser::is_webp(b"RIFF"));
        assert!(!WebpParser::is_webp(b""));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = WebpParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "WebP Parser");
        assert!(!meta.requires_sandbox);
    }
}
