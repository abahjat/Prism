// SPDX-License-Identifier: AGPL-3.0-only
//! TGA (Truevision) image parser

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

/// TGA (Truevision) image parser
#[derive(Debug, Clone)]
pub struct TgaParser;

impl TgaParser {
    /// Create a new TGA parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data appears to be a TGA file
    ///
    /// TGA files don't have a reliable magic signature at the start.
    /// Some modern TGA files have "TRUEVISION-XFILE" at the end.
    /// We fall back to extension-based detection for reliability.
    #[must_use]
    fn is_tga(data: &[u8]) -> bool {
        // Check for TGA 2.0 footer signature
        if data.len() >= 26 {
            let footer_start = data.len() - 18;
            if &data[footer_start..footer_start + 16] == b"TRUEVISION-XFILE" {
                return true;
            }
        }

        // TGA header validation (less reliable but better than nothing)
        // Byte 2 is image type: 0, 1, 2, 3, 9, 10, 11
        if data.len() >= 18 {
            let image_type = data[2];
            matches!(image_type, 0 | 1 | 2 | 3 | 9 | 10 | 11)
        } else {
            false
        }
    }
}

impl Default for TgaParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for TgaParser {
    fn format(&self) -> Format {
        Format::tga()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_tga(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing TGA image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let cursor = Cursor::new(&data);
        let img = image::load(cursor, ImageFormat::Tga)
            .map_err(|e| Error::ParseError(format!("Failed to decode TGA: {e}")))?;

        let width = img.width();
        let height = img.height();

        debug!("TGA dimensions: {width}x{height}");

        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/x-tga".to_string(),
            data: Some(data.to_vec()),
            url: None,
            width,
            height,
        };

        let image_block = ImageBlock {
            resource_id: resource_id.clone(),
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            alt_text: context.filename.clone(),
            format: Some("TGA".to_string()),
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
        metadata.add_custom("format", "TGA");
        metadata.add_custom("width", width.to_string());
        metadata.add_custom("height", height.to_string());

        let mut document = Document::builder().metadata(metadata).build();
        document.pages.push(page);
        document.resources.images.push(image_resource);

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "TGA Parser".to_string(),
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
    fn test_parser_metadata() {
        let parser = TgaParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "TGA Parser");
    }
}
