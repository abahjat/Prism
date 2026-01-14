// SPDX-License-Identifier: AGPL-3.0-only
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
use psd::Psd;
use std::io::Cursor;

/// PSD image parser
#[derive(Debug, Clone)]
pub struct PsdParser;

impl PsdParser {
    /// Create a new PSD parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for PsdParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PsdParser {
    fn format(&self) -> Format {
        Format::psd()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // PSD magic: 8BPS
        if data.len() < 4 {
            return false;
        }
        data[0] == 0x38 && data[1] == 0x42 && data[2] == 0x50 && data[3] == 0x53
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let psd = Psd::from_bytes(&data).map_err(|e| Error::ParseError(e.to_string()))?;

        let width = psd.width();
        let height = psd.height();
        let rgba = psd.rgba();

        // Convert to PNG for web display
        let image = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            Error::ParseError("Failed to create RGBA image from PSD data".to_string())
        })?;

        let dynamic_image = image::DynamicImage::ImageRgba8(image);
        let mut png_data = Vec::new();
        dynamic_image
            .write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)
            .map_err(|e| Error::ParseError(format!("Failed to encode PSD as PNG: {e}")))?;

        let resource_id = "psd_preview".to_string();

        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/png".to_string(),
            data: Some(png_data),
            url: None,
            width,
            height,
        };

        #[allow(clippy::cast_precision_loss)]
        let image_block = ImageBlock {
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            resource_id: resource_id.clone(),
            alt_text: Some("PSD Preview".to_string()),
            format: Some("image/vnd.adobe.photoshop".to_string()),
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
        metadata.add_custom("format", "PSD");

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.resources.images = vec![image_resource];

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PSD Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::ImageExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}
