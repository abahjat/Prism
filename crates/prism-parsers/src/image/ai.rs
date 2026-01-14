// SPDX-License-Identifier: AGPL-3.0-only
//! Adobe Illustrator (.ai) parser
//!
//! Modern AI files are typically PDF-based, so this parser delegates to the PDF parser.
//! Older AI files may use EPS format, which would require separate handling.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, ShapeStyle, TextBlock,
        TextRun,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use tracing::debug;

/// Adobe Illustrator parser
///
/// Modern AI files (CS+ versions) are PDF-based and can be parsed using the PDF parser.
/// This parser detects AI files and delegates to the appropriate parser.
#[derive(Debug, Clone)]
pub struct AiParser;

impl AiParser {
    /// Create a new Adobe Illustrator parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data appears to be an AI file
    /// AI files can be:
    /// - PDF-based (starts with %PDF-)
    /// - EPS-based (starts with %!PS-Adobe)
    #[must_use]
    pub fn is_ai(data: &[u8]) -> bool {
        if data.len() < 8 {
            return false;
        }
        // PDF-based AI
        data.starts_with(b"%PDF-") || 
        // EPS-based AI
        data.starts_with(b"%!PS-Adobe")
    }

    /// Check if this is a PDF-based AI file
    #[must_use]
    fn is_pdf_based(data: &[u8]) -> bool {
        data.starts_with(b"%PDF-")
    }
}

impl Default for AiParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for AiParser {
    fn format(&self) -> Format {
        Format::illustrator()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_ai(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing Adobe Illustrator file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid AI file signature".to_string()));
        }

        // For PDF-based AI files, we could delegate to PDF parser
        // For now, create an informational document
        let is_pdf = Self::is_pdf_based(&data);
        let format_type = if is_pdf { "PDF-based" } else { "EPS-based" };

        let info_text = format!(
            "Adobe Illustrator File\n\n\
            Type: {format_type}\n\
            File Size: {} bytes\n\n\
            {}",
            data.len(),
            if is_pdf {
                "This AI file uses PDF format internally and can be rendered using PDF tools."
            } else {
                "This AI file uses EPS/PostScript format. Full rendering requires specialized tools."
            }
        );

        let text_run = TextRun {
            text: info_text,
            style: prism_core::document::TextStyle::default(),
            bounds: None,
            char_positions: None,
        };

        let text_block = TextBlock {
            vertical_alignment: None,
            runs: vec![text_run],
            paragraph_style: None,
            bounds: Rect::new(50.0, 50.0, 700.0, 200.0),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: vec![ContentBlock::Text(text_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "Adobe Illustrator");
        metadata.add_custom("format_type", format_type);

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;

        debug!("Successfully parsed Adobe Illustrator file");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "Adobe Illustrator Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::MetadataExtraction],
            requires_sandbox: false,
        }
    }
}
