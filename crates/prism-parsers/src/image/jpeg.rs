//! JPEG image parser

use async_trait::async_trait;
use bytes::Bytes;
use image::ImageFormat;
use prism_core::{
    document::{ContentBlock, Dimensions, Document, ImageBlock, ImageResource, Page, Rect},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{Parser, ParseContext, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::debug;

/// JPEG image parser
///
/// Parses JPEG/JPG files into the Unified Document Model.
/// Creates a single-page document containing the image.
#[derive(Debug, Clone)]
pub struct JpegParser;

impl JpegParser {
    /// Create a new JPEG parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for JpegParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for JpegParser {
    fn format(&self) -> Format {
        Format::jpeg()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // JPEG magic bytes: 0xFF 0xD8 0xFF
        // The 4th byte can vary (0xE0 for JFIF, 0xE1 for Exif, etc.)
        if data.len() < 3 {
            return false;
        }

        data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing JPEG image, size: {} bytes, filename: {:?}",
            context.size,
            context.filename
        );

        // Validate JPEG signature
        if !self.can_parse(&data) {
            return Err(Error::ParseError(
                "Invalid JPEG signature".to_string(),
            ));
        }

        // Decode JPEG image to get dimensions
        let cursor = Cursor::new(&data);
        let img = image::load(cursor, ImageFormat::Jpeg).map_err(|e| {
            Error::ParseError(format!("Failed to decode JPEG: {}", e))
        })?;

        let width = img.width();
        let height = img.height();

        debug!("JPEG dimensions: {}x{}", width, height);

        // Create resource ID for the image
        let resource_id = format!("img_{}", uuid::Uuid::new_v4());

        // Create image resource
        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/jpeg".to_string(),
            data: Some(data.to_vec()),
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
            format: Some("JPEG".to_string()),
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

        // Create document
        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.resources.images.push(image_resource);

        debug!("Successfully parsed JPEG image");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "JPEG Parser".to_string(),
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
    fn test_can_parse_jpeg_signature() {
        let parser = JpegParser::new();

        // JFIF JPEG signature
        let jpeg_jfif = &[0xFF, 0xD8, 0xFF, 0xE0];
        assert!(parser.can_parse(jpeg_jfif));

        // Exif JPEG signature
        let jpeg_exif = &[0xFF, 0xD8, 0xFF, 0xE1];
        assert!(parser.can_parse(jpeg_exif));
    }

    #[test]
    fn test_can_parse_invalid_signature() {
        let parser = JpegParser::new();
        let invalid_data = b"Not a JPEG file";
        assert!(!parser.can_parse(invalid_data));
    }

    #[test]
    fn test_can_parse_too_short() {
        let parser = JpegParser::new();
        let short_data = &[0xFF, 0xD8];
        assert!(!parser.can_parse(short_data));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = JpegParser::new();
        let metadata = parser.metadata();

        assert_eq!(metadata.name, "JPEG Parser");
        assert!(!metadata.requires_sandbox);
        assert!(!metadata.features.is_empty());
    }
}
