//! PDF document parser
//!
//! Parses PDF files by embedding raw PDF data for client-side rendering with PDF.js

use async_trait::async_trait;
use bytes::Bytes;
use lopdf::Document as LopdfDocument;
use prism_core::{
    document::{ContentBlock, Dimensions, Document, Page, Rect, TextBlock, TextRun, TextStyle},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{Parser, ParseContext, ParserFeature, ParserMetadata},
};
use tracing::{debug, info, warn};

/// PDF document parser
#[derive(Debug, Clone)]
pub struct PdfParser;

impl PdfParser {
    /// Create a new PDF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Extract metadata from PDF
    fn extract_metadata(data: &[u8]) -> Metadata {
        let mut metadata = Metadata::default();
        let cursor = std::io::Cursor::new(data);
        if let Ok(pdf_doc) = LopdfDocument::load_from(cursor) {
            if let Ok(info) = pdf_doc.trailer.get(b"Info") {
                if let Ok(info_dict) = info.as_dict() {
                    if let Ok(title) = info_dict.get(b"Title") {
                        if let Ok(title_bytes) = title.as_str() {
                            if let Ok(title_str) = String::from_utf8(title_bytes.to_vec()) {
                                metadata.title = Some(title_str);
                            }
                        }
                    }
                    if let Ok(author) = info_dict.get(b"Author") {
                        if let Ok(author_bytes) = author.as_str() {
                            if let Ok(author_str) = String::from_utf8(author_bytes.to_vec()) {
                                metadata.author = Some(author_str);
                            }
                        }
                    }
                }
            }
        }
        metadata.add_custom("format", "PDF");
        metadata
    }

    fn get_page_count(data: &[u8]) -> usize {
        let cursor = std::io::Cursor::new(data);
        if let Ok(pdf_doc) = LopdfDocument::load_from(cursor) {
            pdf_doc.get_pages().len()
        } else {
            1
        }
    }
}

impl Default for PdfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PdfParser {
    fn format(&self) -> Format {
        Format::pdf()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        if data.len() < 5 {
            return false;
        }
        data[0] == b'%' && data[1] == b'P' && data[2] == b'D' && data[3] == b'F' && data[4] == b'-'
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing PDF, size: {} bytes", context.size);

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid PDF signature".to_string()));
        }

        let page_count = Self::get_page_count(&data);
        if page_count == 0 {
            return Err(Error::ParseError("PDF has no pages".to_string()));
        }

        // Embed PDF as base64
        let pdf_base64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
        
        let text_run = TextRun {
            text: format!("__PDF_DATA__:{}", pdf_base64),
            style: TextStyle::default(),
            bounds: Some(Rect::default()),
            char_positions: Some(Vec::new()),
        };

        let page = Page {
            number: 1,
            dimensions: Dimensions { width: 612.0, height: 792.0 },
            content: vec![ContentBlock::Text(TextBlock {
                runs: vec![text_run],
                paragraph_style: None,
                bounds: Rect::default(),
            })],
            metadata: Default::default(),
            annotations: Vec::new(),
        };

        let mut metadata = Self::extract_metadata(&data);
        if let Some(ref filename) = context.filename {
            if metadata.title.is_none() {
                metadata.title = Some(filename.clone());
            }
        }
        metadata.add_custom("page_count", page_count as i64);

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;

        info!("Prepared PDF with {} pages for client rendering", page_count);
        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PDF Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::MetadataExtraction],
            requires_sandbox: false,
        }
    }
}
