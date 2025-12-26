// SPDX-License-Identifier: AGPL-3.0-only
//! PNG image parser

use async_trait::async_trait;
use bytes::Bytes;
use image::ImageFormat;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, ImageBlock, ImageResource, Page, Rect, ShapeStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::debug;

/// PNG image parser
///
/// Parses PNG (Portable Network Graphics) files into the Unified Document Model.
/// Creates a single-page document containing the image.
#[derive(Debug, Clone)]
pub struct PngParser;

impl PngParser {
    /// Create a new PNG parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for PngParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PngParser {
    fn format(&self) -> Format {
        Format::png()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // PNG magic bytes: 0x89 0x50 0x4E 0x47 0x0D 0x0A 0x1A 0x0A
        if data.len() < 8 {
            return false;
        }

        data[0] == 0x89
            && data[1] == 0x50
            && data[2] == 0x4E
            && data[3] == 0x47
            && data[4] == 0x0D
            && data[5] == 0x0A
            && data[6] == 0x1A
            && data[7] == 0x0A
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing PNG image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Validate PNG signature
        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid PNG signature".to_string()));
        }

        // Decode PNG image to get dimensions
        let cursor = Cursor::new(&data);
        let img = image::load(cursor, ImageFormat::Png)
            .map_err(|e| Error::ParseError(format!("Failed to decode PNG: {}", e)))?;

        let width = img.width();
        let height = img.height();

        debug!("PNG dimensions: {}x{}", width, height);

        // Create resource ID for the image
        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        // Create image resource
        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/png".to_string(),
            data: Some(data.to_vec()),
            url: None,
            width,
            height,
        };

        // Create image block
        let image_block = ImageBlock {
            bounds: Rect::new(0.0, 0.0, width as f64, height as f64),
            resource_id: resource_id.clone(),
            alt_text: None,
            format: Some("image/png".to_string()),
            original_size: Some(Dimensions::new(width as f64, height as f64)),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        // Create single page with the image
        let page = Page {
            number: 1,
            dimensions: Dimensions {
                width: width as f64,
                height: height as f64,
            },
            content: vec![ContentBlock::Image(image_block)],
            metadata: Default::default(),
            annotations: Vec::new(),
        };

        // Create basic metadata
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }

        // Create document
        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.resources.images.push(image_resource);

        debug!("Successfully parsed PNG image");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PNG Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::ImageExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid 1x1 PNG (67 bytes)
    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1 dimensions
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, // bit depth, color type, CRC
        0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, // IDAT chunk
        0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, // compressed data
        0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, // CRC
        0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, // IEND chunk
        0x42, 0x60, 0x82,
    ];

    #[test]
    fn test_can_parse_valid_png() {
        let parser = PngParser::new();
        assert!(parser.can_parse(MINIMAL_PNG));
    }

    #[test]
    fn test_can_parse_invalid_signature() {
        let parser = PngParser::new();
        let invalid_data = b"Not a PNG file";
        assert!(!parser.can_parse(invalid_data));
    }

    #[test]
    fn test_can_parse_too_short() {
        let parser = PngParser::new();
        let short_data = &[0x89, 0x50, 0x4E];
        assert!(!parser.can_parse(short_data));
    }

    #[tokio::test]
    async fn test_parse_minimal_png() {
        let parser = PngParser::new();
        let data = Bytes::from(MINIMAL_PNG);
        let data_len = data.len();

        let context = ParseContext {
            format: Format::png(),
            filename: Some("test.png".to_string()),
            size: data_len,
            options: Default::default(),
        };

        let result = parser.parse(data, context).await;
        assert!(result.is_ok(), "Failed to parse minimal PNG: {:?}", result);

        let document = result.unwrap();
        assert_eq!(document.page_count(), 1);
        assert!((document.pages[0].dimensions.width - 1.0).abs() < 0.01);
        assert!((document.pages[0].dimensions.height - 1.0).abs() < 0.01);
        assert_eq!(document.pages[0].content.len(), 1);

        // Verify it's an image block
        match &document.pages[0].content[0] {
            ContentBlock::Image(img) => {
                assert!((img.bounds.width - 1.0).abs() < 0.01);
                assert!((img.bounds.height - 1.0).abs() < 0.01);
            }
            _ => panic!("Expected image block"),
        }
    }

    #[tokio::test]
    async fn test_parse_invalid_png() {
        let parser = PngParser::new();
        let invalid_data = Bytes::from("Not a PNG file");

        let context = ParseContext {
            format: Format::png(),
            filename: Some("invalid.png".to_string()),
            size: invalid_data.len(),
            options: Default::default(),
        };

        let result = parser.parse(invalid_data, context).await;
        assert!(result.is_err(), "Should fail to parse invalid PNG");
    }

    #[test]
    fn test_parser_metadata() {
        let parser = PngParser::new();
        let metadata = parser.metadata();

        assert_eq!(metadata.name, "PNG Parser");
        assert!(!metadata.requires_sandbox);
        assert!(!metadata.features.is_empty());
    }
}
