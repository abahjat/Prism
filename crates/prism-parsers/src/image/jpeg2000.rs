// SPDX-License-Identifier: AGPL-3.0-only
//! JPEG 2000 image parser

use async_trait::async_trait;
use bytes::Bytes;
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
use tracing::debug;

/// JPEG 2000 image parser
///
/// Parses JPEG 2000 (.jp2, .j2k, .jpx) files into the Unified Document Model.
/// Note: Full JPEG2000 decoding requires specialized libraries; this uses basic support.
#[derive(Debug, Clone)]
pub struct Jpeg2000Parser;

impl Jpeg2000Parser {
    /// Create a new JPEG 2000 parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for Jpeg2000Parser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for Jpeg2000Parser {
    fn format(&self) -> Format {
        Format::jpeg2000()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // JPEG 2000 signature: 00 00 00 0C 6A 50 20 20 0D 0A 87 0A
        if data.len() < 12 {
            return false;
        }

        data.starts_with(&[
            0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
        ])
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing JPEG 2000 image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid JPEG 2000 signature".to_string()));
        }

        // JPEG 2000 parsing is limited without specialized crate
        // For now, create a placeholder document indicating the format
        // Users would need `jpeg2000` or `openjpeg` bindings for full support

        // Create resource ID for the image
        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        // Default dimensions (JPEG 2000 header parsing would be complex)
        let width = 800_u32;
        let height = 600_u32;

        // Create image resource (store raw data for potential external processing)
        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/jp2".to_string(),
            data: Some(data.to_vec()),
            url: None,
            width,
            height,
        };

        // Create image block
        let image_block = ImageBlock {
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            resource_id: resource_id.clone(),
            alt_text: Some("JPEG 2000 Image".to_string()),
            format: Some("image/jp2".to_string()),
            original_size: Some(Dimensions::new(f64::from(width), f64::from(height))),
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
        metadata.add_custom("format", "JPEG 2000");

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.resources.images.push(image_resource);

        debug!("Successfully parsed JPEG 2000 image");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "JPEG 2000 Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::ImageExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}
