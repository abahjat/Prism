// SPDX-License-Identifier: AGPL-3.0-only
//! WBMP (Wireless Bitmap) image parser

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

/// WBMP (Wireless Bitmap) image parser
///
/// Parses WBMP (Wireless Application Protocol Bitmap) files.
/// WBMP is a simple 1-bit monochrome image format for mobile devices.
#[derive(Debug, Clone)]
pub struct WbmpParser;

impl WbmpParser {
    /// Create a new WBMP parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Decode a multi-byte integer (used in WBMP for dimensions)
    fn decode_multibyte(data: &[u8], offset: &mut usize) -> Option<u32> {
        let mut value: u32 = 0;
        loop {
            if *offset >= data.len() {
                return None;
            }
            let byte = data[*offset];
            *offset += 1;
            value = (value << 7) | u32::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                break;
            }
        }
        Some(value)
    }
}

impl Default for WbmpParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for WbmpParser {
    fn format(&self) -> Format {
        Format::wbmp()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // WBMP type 0 starts with 0x00
        // Minimal header: type (1 byte), fixed header (1 byte), width, height
        if data.len() < 4 {
            return false;
        }
        // Type 0 WBMP
        data[0] == 0x00
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing WBMP image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid WBMP header".to_string()));
        }

        // Parse WBMP header
        // Byte 0: Type (0x00 for type 0)
        // Byte 1: Fixed header (0x00)
        // Following bytes: width (multi-byte), height (multi-byte)
        let mut offset = 2; // Skip type and fixed header

        let width = Self::decode_multibyte(&data, &mut offset)
            .ok_or_else(|| Error::ParseError("Failed to parse WBMP width".to_string()))?;
        let height = Self::decode_multibyte(&data, &mut offset)
            .ok_or_else(|| Error::ParseError("Failed to parse WBMP height".to_string()))?;

        debug!("WBMP dimensions: {width}x{height}");

        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/vnd.wap.wbmp".to_string(),
            data: Some(data.to_vec()),
            url: None,
            width,
            height,
        };

        let image_block = ImageBlock {
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            resource_id: resource_id.clone(),
            alt_text: Some("Wireless Bitmap".to_string()),
            format: Some("image/vnd.wap.wbmp".to_string()),
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
        metadata.add_custom("format", "WBMP");

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.resources.images.push(image_resource);

        debug!("Successfully parsed WBMP image");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "WBMP Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::ImageExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}
