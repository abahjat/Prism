// SPDX-License-Identifier: AGPL-3.0-only
//! RTF (Rich Text Format) parser
//!
//! Parses RTF documents and extracts text content with basic formatting.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use rtf_parser::{Lexer, Parser as RtfLibParser, StyleBlock};
use tracing::debug;

/// RTF document parser
#[derive(Debug, Clone)]
pub struct RtfParser;

impl RtfParser {
    /// Create a new RTF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data starts with RTF signature
    #[must_use]
    fn is_rtf(data: &[u8]) -> bool {
        data.starts_with(b"{\\rtf")
    }

    /// Convert RTF style to Prism `TextStyle`
    fn convert_style(style: &StyleBlock) -> TextStyle {
        TextStyle {
            bold: style.painter.bold,
            italic: style.painter.italic,
            underline: style.painter.underline,
            font_size: if style.painter.font_size > 0 {
                #[allow(clippy::cast_precision_loss)]
                Some(f64::from(style.painter.font_size) / 2.0) // RTF uses half-points
            } else {
                None
            },
            font_family: None, // Font name would require looking up in font table
            color: None,       // Color would require looking up in color table
            ..TextStyle::default()
        }
    }
}

impl Default for RtfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for RtfParser {
    fn format(&self) -> Format {
        Format::rtf()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_rtf(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing RTF file, size: {} bytes, filename: {:?}",
            data.len(),
            context.filename
        );

        // Convert to string (RTF is ASCII-based with escape sequences)
        let rtf_text = std::str::from_utf8(&data)
            .map_err(|e| Error::ParseError(format!("Invalid RTF encoding: {e}")))?;

        // Tokenize RTF
        let tokens = Lexer::scan(rtf_text)
            .map_err(|e| Error::ParseError(format!("RTF lexer error: {e:?}")))?;

        // Parse tokens into document
        let rtf_doc = RtfLibParser::new(tokens)
            .parse()
            .map_err(|e| Error::ParseError(format!("RTF parser error: {e:?}")))?;

        debug!(
            "Parsed RTF document with {} styled blocks",
            rtf_doc.body.len()
        );

        // Convert RTF body to TextRuns
        let mut text_runs = Vec::new();

        for block in &rtf_doc.body {
            let style = Self::convert_style(block);
            let run = TextRun {
                text: block.text.clone(),
                style,
                bounds: None,
                char_positions: None,
            };
            text_runs.push(run);
        }

        // If no styled blocks, try to extract plain text
        if text_runs.is_empty() {
            let plain_text = rtf_doc
                .body
                .iter()
                .map(|b| b.text.as_str())
                .collect::<String>();
            if !plain_text.is_empty() {
                text_runs.push(TextRun {
                    text: plain_text,
                    style: TextStyle::default(),
                    bounds: None,
                    char_positions: None,
                });
            }
        }

        // Create text block
        let mut text_block = TextBlock::new(Rect::default());
        for run in text_runs {
            text_block.add_run(run);
        }

        // Create page
        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: vec![ContentBlock::Text(text_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        // Build metadata
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "RTF");

        // Build document
        let mut document = Document::builder().metadata(metadata).build();
        document.pages.push(page);

        debug!("RTF parsing complete, created 1 page");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "RTF Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
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
    fn test_is_rtf() {
        assert!(RtfParser::is_rtf(b"{\\rtf1\\ansi test}"));
        assert!(RtfParser::is_rtf(b"{\\rtf1 test}"));
        assert!(!RtfParser::is_rtf(b"not rtf"));
        assert!(!RtfParser::is_rtf(b""));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = RtfParser::new();
        let meta = parser.metadata();
        assert_eq!(meta.name, "RTF Parser");
        assert!(!meta.requires_sandbox);
    }

    #[tokio::test]
    async fn test_parse_simple_rtf() {
        let parser = RtfParser::new();
        let rtf_content = b"{\\rtf1\\ansi Hello, World!}";

        let context = ParseContext {
            format: Format::rtf(),
            filename: Some("test.rtf".to_string()),
            size: rtf_content.len(),
            options: prism_core::parser::ParseOptions::default(),
        };

        let result = parser.parse(Bytes::from(&rtf_content[..]), context).await;
        assert!(result.is_ok(), "Parse failed: {:?}", result.err());

        let doc = result.unwrap();
        assert_eq!(doc.pages.len(), 1);
    }
}
