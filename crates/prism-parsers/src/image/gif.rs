// SPDX-License-Identifier: AGPL-3.0-only
//! GIF image parser
//!
//! Parses GIF (Graphics Interchange Format) images into the Unified Document Model.

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

/// GIF image parser
///
/// Parses GIF (Graphics Interchange Format) files into the Unified Document Model.
/// Creates a single-page document containing the image. Animated GIFs are supported
/// by embedding the full GIF data.
#[derive(Debug, Clone)]
pub struct GifParser;

impl GifParser {
    /// Create a new GIF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data has GIF signature
    #[must_use]
    fn is_gif(data: &[u8]) -> bool {
        if data.len() < 6 {
            return false;
        }
        data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")
    }
}

impl Default for GifParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for GifParser {
    fn format(&self) -> Format {
        Format::gif()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_gif(data)
    }

    /// # Errors
    ///
    /// Returns an error if the GIF data is invalid or cannot be decoded.
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing GIF image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Validate GIF signature
        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid GIF signature".to_string()));
        }

        // Decode GIF image to get dimensions
        let cursor = Cursor::new(&data);
        let img = image::load(cursor, ImageFormat::Gif)
            .map_err(|e| Error::ParseError(format!("Failed to decode GIF: {e}")))?;

        let width = img.width();
        let height = img.height();

        debug!("GIF dimensions: {width}x{height}");

        // Create resource ID for the image
        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        // Create image resource - embed the original GIF data to preserve animation
        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/gif".to_string(),
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
            format: Some("GIF".to_string()),
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
        metadata.add_custom("format", "GIF");
        metadata.add_custom("width", width.to_string());
        metadata.add_custom("height", height.to_string());

        // Build document
        let mut document = Document::builder().metadata(metadata).build();
        document.pages.push(page);
        document.resources.images.push(image_resource);

        debug!("GIF parsing complete");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "GIF Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction, // Not really, but for consistency
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
    fn test_is_gif() {
        assert!(GifParser::is_gif(b"GIF87a test"));
        assert!(GifParser::is_gif(b"GIF89a test"));
        assert!(!GifParser::is_gif(b"PNG test"));
        assert!(!GifParser::is_gif(b"GIF"));
        assert!(!GifParser::is_gif(b""));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = GifParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "GIF Parser");
        assert!(!meta.requires_sandbox);
    }
}
