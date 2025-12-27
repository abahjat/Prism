// SPDX-License-Identifier: AGPL-3.0-only
//! BMP image parser

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

/// BMP image parser
#[derive(Debug, Clone)]
pub struct BmpParser;

impl BmpParser {
    /// Create a new BMP parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data has BMP signature
    #[must_use]
    fn is_bmp(data: &[u8]) -> bool {
        data.len() >= 2 && data[0] == b'B' && data[1] == b'M'
    }
}

impl Default for BmpParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for BmpParser {
    fn format(&self) -> Format {
        Format::bmp()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_bmp(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing BMP image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid BMP signature".to_string()));
        }

        let cursor = Cursor::new(&data);
        let img = image::load(cursor, ImageFormat::Bmp)
            .map_err(|e| Error::ParseError(format!("Failed to decode BMP: {e}")))?;

        let width = img.width();
        let height = img.height();

        debug!("BMP dimensions: {width}x{height}");

        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/bmp".to_string(),
            data: Some(data.to_vec()),
            url: None,
            width,
            height,
        };

        let image_block = ImageBlock {
            resource_id: resource_id.clone(),
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            alt_text: context.filename.clone(),
            format: Some("BMP".to_string()),
            original_size: Some(Dimensions {
                width: f64::from(width),
                height: f64::from(height),
            }),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        let page = Page {
            number: 1,
            dimensions: Dimensions {
                width: f64::from(width),
                height: f64::from(height),
            },
            content: vec![ContentBlock::Image(image_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "BMP");
        metadata.add_custom("width", width.to_string());
        metadata.add_custom("height", height.to_string());

        let mut document = Document::builder().metadata(metadata).build();
        document.pages.push(page);
        document.resources.images.push(image_resource);

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "BMP Parser".to_string(),
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
    fn test_is_bmp() {
        assert!(BmpParser::is_bmp(b"BM some data"));
        assert!(!BmpParser::is_bmp(b"PNG data"));
        assert!(!BmpParser::is_bmp(b"B"));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = BmpParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "BMP Parser");
    }
}
