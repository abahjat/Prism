// SPDX-License-Identifier: AGPL-3.0-only
//! ICO (Windows Icon) image parser

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

/// ICO (Windows Icon) image parser
#[derive(Debug, Clone)]
pub struct IcoParser;

impl IcoParser {
    /// Create a new ICO parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data has ICO signature
    /// ICO files start with: 00 00 01 00 (reserved=0, type=1 for icon)
    #[must_use]
    fn is_ico(data: &[u8]) -> bool {
        data.len() >= 4 && data[0] == 0 && data[1] == 0 && data[2] == 1 && data[3] == 0
    }
}

impl Default for IcoParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for IcoParser {
    fn format(&self) -> Format {
        Format::ico()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_ico(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing ICO image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid ICO signature".to_string()));
        }

        let cursor = Cursor::new(&data);
        let img = image::load(cursor, ImageFormat::Ico)
            .map_err(|e| Error::ParseError(format!("Failed to decode ICO: {e}")))?;

        let width = img.width();
        let height = img.height();

        debug!("ICO dimensions: {width}x{height}");

        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/x-icon".to_string(),
            data: Some(data.to_vec()),
            url: None,
            width,
            height,
        };

        let image_block = ImageBlock {
            resource_id: resource_id.clone(),
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            alt_text: context.filename.clone(),
            format: Some("ICO".to_string()),
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
        metadata.add_custom("format", "ICO");
        metadata.add_custom("width", width.to_string());
        metadata.add_custom("height", height.to_string());

        let mut document = Document::builder().metadata(metadata).build();
        document.pages.push(page);
        document.resources.images.push(image_resource);

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "ICO Parser".to_string(),
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
    fn test_is_ico() {
        // Valid ICO header
        assert!(IcoParser::is_ico(&[0, 0, 1, 0, 1, 0]));
        // Invalid
        assert!(!IcoParser::is_ico(b"PNG data"));
        assert!(!IcoParser::is_ico(&[0, 0, 2, 0])); // type 2 is CUR, not ICO
    }

    #[test]
    fn test_parser_metadata() {
        let parser = IcoParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "ICO Parser");
    }
}
