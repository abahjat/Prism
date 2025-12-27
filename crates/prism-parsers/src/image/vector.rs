// SPDX-License-Identifier: AGPL-3.0-only
//! Vector/metafile image parsers (EMF, EMZ, WMF, EPS)
//!
//! Basic parsers for Windows Metafile formats and EPS.
//! These formats are complex and full rendering is not supported.
//! The parsers extract metadata and provide basic format information.

use async_trait::async_trait;
use bytes::Bytes;
use flate2::read::GzDecoder;
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
use std::io::Read;
use tracing::debug;

/// EMF (Enhanced Metafile) parser
#[derive(Debug, Clone)]
pub struct EmfParser;

impl EmfParser {
    /// Create a new EMF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check for EMF signature (0x00000001 at offset 0, "EMF" at offset 40)
    #[must_use]
    fn is_emf(data: &[u8]) -> bool {
        if data.len() < 44 {
            return false;
        }
        // Check record type (0x00000001 = EMR_HEADER)
        data[0..4] == [0x01, 0x00, 0x00, 0x00] && &data[40..43] == b" EM"
    }
}

impl Default for EmfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for EmfParser {
    fn format(&self) -> Format {
        Format::emf()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_emf(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing EMF image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        create_vector_info_document("EMF", "Enhanced Windows Metafile", data.len(), context)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "EMF Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::MetadataExtraction],
            requires_sandbox: false,
        }
    }
}

/// EMZ (Compressed Enhanced Metafile) parser
#[derive(Debug, Clone)]
pub struct EmzParser;

impl EmzParser {
    /// Create a new EMZ parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check for gzip signature (files are gzip-compressed EMF)
    #[must_use]
    fn is_emz(data: &[u8]) -> bool {
        // Gzip magic number
        data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
    }

    /// Decompress EMZ to EMF
    fn decompress(data: &[u8]) -> Option<Vec<u8>> {
        let mut decoder = GzDecoder::new(data);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).ok()?;
        Some(decompressed)
    }
}

impl Default for EmzParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for EmzParser {
    fn format(&self) -> Format {
        Format::emz()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        if !Self::is_emz(data) {
            return false;
        }
        // Verify it decompresses to EMF
        if let Some(decompressed) = Self::decompress(data) {
            return EmfParser::is_emf(&decompressed);
        }
        false
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing EMZ image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let decompressed = Self::decompress(&data)
            .ok_or_else(|| Error::ParseError("Failed to decompress EMZ".to_string()))?;

        create_vector_info_document(
            "EMZ",
            "Compressed Enhanced Metafile",
            decompressed.len(),
            context,
        )
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "EMZ Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::MetadataExtraction],
            requires_sandbox: false,
        }
    }
}

/// WMF (Windows Metafile) parser
#[derive(Debug, Clone)]
pub struct WmfParser;

impl WmfParser {
    /// Create a new WMF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check for WMF signature
    #[must_use]
    fn is_wmf(data: &[u8]) -> bool {
        if data.len() < 6 {
            return false;
        }
        // Placeable WMF header magic number
        (data[0..4] == [0xD7, 0xCD, 0xC6, 0x9A])
            // Or standard WMF with type 1 or 2
            || (data[0..2] == [0x01, 0x00] || data[0..2] == [0x02, 0x00])
    }
}

impl Default for WmfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for WmfParser {
    fn format(&self) -> Format {
        Format::wmf()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_wmf(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing WMF image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        create_vector_info_document("WMF", "Windows Metafile", data.len(), context)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "WMF Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::MetadataExtraction],
            requires_sandbox: false,
        }
    }
}

/// EPS (Encapsulated PostScript) parser
#[derive(Debug, Clone)]
pub struct EpsParser;

impl EpsParser {
    /// Create a new EPS parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check for EPS signature
    #[must_use]
    fn is_eps(data: &[u8]) -> bool {
        // ASCII EPS starts with %!PS or %!Adobe
        if data.len() >= 4
            && (&data[0..4] == b"%!PS" || (data.len() >= 7 && &data[0..7] == b"%!Adobe"))
        {
            return true;
        }
        // Binary EPS has magic bytes
        data.len() >= 4 && data[0..4] == [0xC5, 0xD0, 0xD3, 0xC6]
    }
}

impl Default for EpsParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for EpsParser {
    fn format(&self) -> Format {
        Format::eps()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_eps(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing EPS image, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        create_vector_info_document("EPS", "Encapsulated PostScript", data.len(), context)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "EPS Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::MetadataExtraction],
            requires_sandbox: false,
        }
    }
}

/// Create an informational document for vector formats
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn create_vector_info_document(
    format_name: &str,
    format_description: &str,
    size: usize,
    context: ParseContext,
) -> Result<Document> {
    let info_text = format!(
        "{format_name} Format Information\n\n\
        Format: {format_name} ({format_description})\n\
        File Size: {size} bytes\n\n\
        Note: This is a vector graphics format. \
        Full rendering requires specialized software."
    );

    let text_run = TextRun {
        text: info_text,
        style: prism_core::document::TextStyle::default(),
        bounds: None,
        char_positions: None,
    };

    let text_block = TextBlock {
        runs: vec![text_run],
        paragraph_style: None,
        bounds: Rect::new(50.0, 50.0, 500.0, 200.0),
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
    metadata.add_custom("format", format_name);
    metadata.add_custom("format_description", format_description);

    let mut document = Document::builder().metadata(metadata).build();
    document.pages.push(page);

    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emf_parser_metadata() {
        let parser = EmfParser::new();
        assert_eq!(parser.metadata().name, "EMF Parser");
    }

    #[test]
    fn test_emz_parser_metadata() {
        let parser = EmzParser::new();
        assert_eq!(parser.metadata().name, "EMZ Parser");
    }

    #[test]
    fn test_wmf_parser_metadata() {
        let parser = WmfParser::new();
        assert_eq!(parser.metadata().name, "WMF Parser");
    }

    #[test]
    fn test_eps_parser_metadata() {
        let parser = EpsParser::new();
        assert_eq!(parser.metadata().name, "EPS Parser");
    }

    #[test]
    fn test_is_eps() {
        assert!(EpsParser::is_eps(b"%!PS-Adobe"));
        assert!(EpsParser::is_eps(b"%!Adobe-something"));
        assert!(!EpsParser::is_eps(b"Not EPS"));
    }
}
