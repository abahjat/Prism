//! TIFF image parser with multi-page support

use async_trait::async_trait;
use bytes::Bytes;
use image::{ImageFormat, RgbaImage};
use prism_core::{
    document::{ContentBlock, Dimensions, Document, ImageBlock, ImageResource, Page, Rect},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{Parser, ParseContext, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tiff::decoder::{Decoder, DecodingResult};
use tracing::{debug, info, warn};

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

        // Create TIFF decoder
        let cursor = Cursor::new(&data[..]);
        let mut decoder = Decoder::new(cursor).map_err(|e| {
            Error::ParseError(format!("Failed to create TIFF decoder: {}", e))
        })?;

        let mut pages = Vec::new();
        let mut image_resources = Vec::new();
        let mut page_number = 1;

        // Iterate through all TIFF pages/directories
        loop {
            // Get dimensions for current page
            let (width, height) = decoder.dimensions().map_err(|e| {
                Error::ParseError(format!("Failed to get TIFF dimensions: {}", e))
            })?;

            debug!("TIFF page {} dimensions: {}x{}", page_number, width, height);

            // Decode the image data for this page
            let decoding_result = decoder.read_image().map_err(|e| {
                Error::ParseError(format!("Failed to decode TIFF page {}: {}", page_number, e))
            })?;

            // Convert to RGBA image for consistent handling
            let rgba_image = match decoding_result {
                DecodingResult::U8(data) => {
                    // Check if this is RGB (3 bytes/pixel) or Grayscale (1 byte/pixel)
                    let pixel_count = (width * height) as usize;
                    if data.len() == pixel_count * 3 {
                        // RGB data - convert to RGBA
                        let mut rgba_data = Vec::with_capacity(pixel_count * 4);
                        for chunk in data.chunks_exact(3) {
                            rgba_data.push(chunk[0]); // R
                            rgba_data.push(chunk[1]); // G
                            rgba_data.push(chunk[2]); // B
                            rgba_data.push(255);      // A
                        }
                        RgbaImage::from_raw(width, height, rgba_data)
                            .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from RGB U8 data for page {}", page_number)))?
                    } else if data.len() == pixel_count * 4 {
                        // Already RGBA
                        RgbaImage::from_raw(width, height, data)
                            .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from RGBA U8 data for page {}", page_number)))?
                    } else {
                        // Grayscale - convert to RGBA
                        RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| [p, p, p, 255]).collect())
                            .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from grayscale U8 data for page {}", page_number)))?
                    }
                }
                DecodingResult::U16(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = (p >> 8) as u8;
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from U16 data for page {}", page_number)))?
                }
                DecodingResult::U32(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = (p >> 24) as u8;
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from U32 data for page {}", page_number)))?
                }
                DecodingResult::U64(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = (p >> 56) as u8;
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from U64 data for page {}", page_number)))?
                }
                DecodingResult::F16(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let float_val = p.to_f32();
                        let byte = (float_val.clamp(0.0, 1.0) * 255.0) as u8;
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from F16 data for page {}", page_number)))?
                }
                DecodingResult::F32(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = (p.clamp(0.0, 1.0) * 255.0) as u8;
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from F32 data for page {}", page_number)))?
                }
                DecodingResult::F64(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = (p.clamp(0.0, 1.0) * 255.0) as u8;
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from F64 data for page {}", page_number)))?
                }
                DecodingResult::I8(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = ((p as i16 + 128) as u8);
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from I8 data for page {}", page_number)))?
                }
                DecodingResult::I16(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = ((p >> 8) as i8 as u8);
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from I16 data for page {}", page_number)))?
                }
                DecodingResult::I32(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = ((p >> 24) as i8 as u8);
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from I32 data for page {}", page_number)))?
                }
                DecodingResult::I64(data) => {
                    RgbaImage::from_raw(width, height, data.into_iter().flat_map(|p| {
                        let byte = ((p >> 56) as i8 as u8);
                        [byte, byte, byte, 255]
                    }).collect())
                        .ok_or_else(|| Error::ParseError(format!("Failed to create RGBA image from I64 data for page {}", page_number)))?
                }
            };

            // Convert to PNG for web compatibility
            let dynamic_img = image::DynamicImage::ImageRgba8(rgba_image);
            let mut png_data = Vec::new();
            dynamic_img.write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)
                .map_err(|e| {
                    Error::ParseError(format!("Failed to encode TIFF page {} as PNG: {}", page_number, e))
                })?;

            // Create resource ID for the image
            let resource_id = format!("img_page_{}", page_number);

            // Create image resource
            let image_resource = ImageResource {
                id: resource_id.clone(),
                mime_type: "image/png".to_string(),
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
                alt_text: context.filename.as_ref().map(|f| format!("{} - Page {}", f, page_number)),
                format: Some("TIFF".to_string()),
                original_size: Some(Dimensions {
                    width: width as f64,
                    height: height as f64,
                }),
            };

            // Create page with the image
            let page = Page {
                number: page_number,
                dimensions: Dimensions {
                    width: width as f64,
                    height: height as f64,
                },
                content: vec![ContentBlock::Image(image_block)],
                metadata: Default::default(),
                annotations: Vec::new(),
            };

            pages.push(page);
            image_resources.push(image_resource);

            // Try to move to next page/directory
            if decoder.more_images() {
                if let Err(e) = decoder.next_image() {
                    warn!("Failed to move to next TIFF page: {}", e);
                    return Err(Error::ParseError(format!("Failed to move to next TIFF page: {}", e)));
                }
                page_number += 1;
            } else {
                break;
            }
        }

        // Create basic metadata
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "TIFF");
        metadata.add_custom("page_count", pages.len() as i64);

        // Create document
        let mut document = Document::new();
        document.pages = pages;
        document.metadata = metadata;
        document.resources.images = image_resources;

        info!("Successfully parsed TIFF with {} page(s)", document.pages.len());

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
