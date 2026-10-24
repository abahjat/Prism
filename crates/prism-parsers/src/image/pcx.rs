// SPDX-License-Identifier: AGPL-3.0-only
//! PCX (Paintbrush) image parser

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

/// PCX (Paintbrush) image parser
///
/// Parses PCX/DCX image files into the Unified Document Model.
#[derive(Debug, Clone)]
pub struct PcxParser;

impl PcxParser {
    /// Create a new PCX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Validate PCX header
    fn is_valid_pcx(data: &[u8]) -> bool {
        if data.len() < 128 {
            return false;
        }

        // Byte 0: manufacturer (0x0A = ZSoft)
        // Byte 1: version (0-5)
        // Byte 2: encoding (1 = RLE)
        // Byte 3: bits per pixel (1, 2, 4, 8)
        data[0] == 0x0A && data[1] <= 5 && data[2] <= 1 && matches!(data[3], 1 | 2 | 4 | 8)
    }
}

impl Default for PcxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PcxParser {
    fn format(&self) -> Format {
        Format::pcx()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_valid_pcx(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing PCX image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid PCX header".to_string()));
        }

        // Parse PCX header for dimensions
        // Bytes 4-5: xmin, 6-7: ymin, 8-9: xmax, 10-11: ymax (little-endian)
        let xmin = u16::from_le_bytes([data[4], data[5]]);
        let ymin = u16::from_le_bytes([data[6], data[7]]);
        let xmax = u16::from_le_bytes([data[8], data[9]]);
        let ymax = u16::from_le_bytes([data[10], data[11]]);

        // The window is inclusive; a maximum below its minimum is malformed, and 0..=65535
        // is 65536 wide, so the sizes are computed in u32.
        if xmax < xmin || ymax < ymin {
            return Err(Error::ParseError(
                "PCX image window has its maximum below its minimum".to_string(),
            ));
        }
        let width = u32::from(xmax) - u32::from(xmin) + 1;
        let height = u32::from(ymax) - u32::from(ymin) + 1;

        debug!("PCX dimensions: {width}x{height}");

        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/x-pcx".to_string(),
            data: Some(data.to_vec()),
            url: None,
            width,
            height,
        };

        let image_block = ImageBlock {
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            resource_id: resource_id.clone(),
            alt_text: Some("PCX Image".to_string()),
            format: Some("image/x-pcx".to_string()),
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
        metadata.add_custom("format", "PCX");
        metadata.add_custom("version", format!("{}", data[1]));

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.resources.images.push(image_resource);

        debug!("Successfully parsed PCX image");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PCX Parser".to_string(),
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
    use prism_core::parser::ParseOptions;

    fn context(len: usize) -> ParseContext {
        ParseContext {
            format: Format::pcx(),
            filename: None,
            size: len,
            options: ParseOptions::default(),
        }
    }

    fn header(xmin: u16, ymin: u16, xmax: u16, ymax: u16) -> Bytes {
        let mut d = vec![0u8; 128];
        d[..4].copy_from_slice(&[0x0A, 5, 1, 8]);
        d[4..6].copy_from_slice(&xmin.to_le_bytes());
        d[6..8].copy_from_slice(&ymin.to_le_bytes());
        d[8..10].copy_from_slice(&xmax.to_le_bytes());
        d[10..12].copy_from_slice(&ymax.to_le_bytes());
        Bytes::from(d)
    }

    #[tokio::test]
    async fn window_bounds_are_checked_not_overflowed() {
        // `parse_any` fuzz finds: a maximum below the minimum, and the full 0..=65535 range,
        // both overflowed the u16 size arithmetic.
        let inverted = header(10, 0, 5, 0);
        assert!(
            PcxParser::new()
                .parse(inverted, context(128))
                .await
                .is_err()
        );
        let full = header(0, 0, u16::MAX, 0);
        let doc = PcxParser::new().parse(full, context(128)).await.unwrap();
        assert_eq!(doc.resources.images[0].width, 65_536);
    }
}
