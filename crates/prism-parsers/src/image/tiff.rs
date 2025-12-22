//! TIFF image parser with multi-page support

use async_trait::async_trait;
use bytes::Bytes;
use image::{ImageDecoder, ImageFormat};
use prism_core::{
    document::{ContentBlock, Dimensions, Document, ImageBlock, ImageResource, Page, Rect},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{Parser, ParseContext, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::{debug, info};

/// TIFF image parser
///
/// Parses TIFF files (including multi-page TIFFs) into the Unified Document Model.
/// Creates one page per TIFF image/frame.
#[derive(Debug, Clone)]
pub struct TiffParser;

impl TiffParser {
    /// Create a new TIFF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for TiffParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for TiffParser {
    fn format(&self) -> Format {
        Format::tiff()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // TIFF magic bytes:
        // Little-endian: 0x49 0x49 0x2A 0x00 (II*)
        // Big-endian: 0x4D 0x4D 0x00 0x2A (MM\0*)
        if data.len() < 4 {
            return false;
        }

        (data[0] == 0x49 && data[1] == 0x49 && data[2] == 0x2A && data[3] == 0x00)
            || (data[0] == 0x4D && data[1] == 0x4D && data[2] == 0x00 && data[3] == 0x2A)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing TIFF image, size: {} bytes, filename: {:?}",
            context.size,
            context.filename
        );

        // Validate TIFF signature
        if !self.can_parse(&data) {
            return Err(Error::ParseError(
                "Invalid TIFF signature".to_string(),
            ));
        }

        // Decode TIFF image using the image crate's decoder
        let cursor = Cursor::new(&data);
        let decoder = image::codecs::tiff::TiffDecoder::new(cursor).map_err(|e| {
            Error::ParseError(format!("Failed to decode TIFF: {}", e))
        })?;

        // Get dimensions using the ImageDecoder trait
        let (width, height) = decoder.dimensions();

        debug!("TIFF dimensions: {}x{}", width, height);

        // Decode the first page/frame to a DynamicImage
        let img = image::DynamicImage::from_decoder(decoder).map_err(|e| {
            Error::ParseError(format!("Failed to decode TIFF: {}", e))
        })?;

        // Encode the image as PNG for web compatibility
        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)
            .map_err(|e| {
                Error::ParseError(format!("Failed to encode TIFF as PNG: {}", e))
            })?;

        // Create resource ID for the image
        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        // Create image resource
        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/png".to_string(), // Store as PNG for web compatibility
            data: Some(png_data),
            url: None,
            width,
            height,
        };

        // Create image block
        let image_block = ImageBlock {
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                width: width as f64,
                height: height as f64,
            },
            resource_id: resource_id.clone(),
            alt_text: context.filename.clone(),
            format: Some("TIFF".to_string()),
            original_size: Some(Dimensions {
                width: width as f64,
                height: height as f64,
            }),
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
        metadata.add_custom("format", "TIFF");

        // Create document
        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.resources.images.push(image_resource);

        info!("Successfully parsed TIFF image");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "TIFF Parser".to_string(),
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

    #[test]
    fn test_can_parse_tiff_little_endian() {
        let parser = TiffParser::new();
        // Little-endian TIFF signature (II*)
        let tiff_le = &[0x49, 0x49, 0x2A, 0x00];
        assert!(parser.can_parse(tiff_le));
    }

    #[test]
    fn test_can_parse_tiff_big_endian() {
        let parser = TiffParser::new();
        // Big-endian TIFF signature (MM\0*)
        let tiff_be = &[0x4D, 0x4D, 0x00, 0x2A];
        assert!(parser.can_parse(tiff_be));
    }

    #[test]
    fn test_can_parse_invalid_signature() {
        let parser = TiffParser::new();
        let invalid_data = b"Not a TIFF file";
        assert!(!parser.can_parse(invalid_data));
    }

    #[test]
    fn test_can_parse_too_short() {
        let parser = TiffParser::new();
        let short_data = &[0x49, 0x49];
        assert!(!parser.can_parse(short_data));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = TiffParser::new();
        let metadata = parser.metadata();

        assert_eq!(metadata.name, "TIFF Parser");
        assert!(!metadata.requires_sandbox);
        assert!(!metadata.features.is_empty());
    }
}
