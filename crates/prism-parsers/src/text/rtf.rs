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
    fn convert_style(style: &StyleBlock, header: &rtf_parser::RtfHeader) -> TextStyle {
        let mut text_style = TextStyle {
            bold: style.painter.bold,
            italic: style.painter.italic,
            underline: style.painter.underline,
            font_size: if style.painter.font_size > 0 {
                #[allow(clippy::cast_precision_loss)]
                Some(f64::from(style.painter.font_size) / 2.0)
            } else {
                None
            },
            ..TextStyle::default()
        };

        // Font lookup
        if let Some(font) = header.font_table.get(&style.painter.font_ref) {
            text_style.font_family = Some(font.name.clone());
        }

        // Color lookup
        if let Some(color) = header.color_table.get(&style.painter.color_ref) {
            text_style.color = Some(format!(
                "#{:02x}{:02x}{:02x}",
                color.red, color.green, color.blue
            ));
        }

        text_style
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

        // Convert RTF body to TextRuns/Blocks
        // Group runs into blocks based on newlines
        let mut text_blocks = Vec::new();
        let mut current_runs = Vec::new();

        for block in &rtf_doc.body {
            let style = Self::convert_style(block, &rtf_doc.header);

            // Check for newlines in text
            // RTF parser might return text with \r or \n
            let parts: Vec<&str> = block.text.split(['\r', '\n']).collect();

            for (i, part) in parts.iter().enumerate() {
                if !part.is_empty() {
                    let run = TextRun {
                        text: (*part).to_string(),
                        style: style.clone(),
                        bounds: None,
                        char_positions: None,
                    };
                    current_runs.push(run);
                }

                // If this isn't the last part, it means we hit a split (newline)
                if i < parts.len() - 1 && !current_runs.is_empty() {
                    let mut text_block = TextBlock::new(Rect::default());
                    text_block.runs = current_runs;
                    // Approximate spacing
                    text_block.style.fill_color = None;
                    text_blocks.push(text_block);
                    current_runs = Vec::new();
                }
            }
        }

        // Push remaining runs
        if !current_runs.is_empty() {
            let mut text_block = TextBlock::new(Rect::default());
            text_block.runs = current_runs;
            text_blocks.push(text_block);
        }

        // If no styled blocks, try to extract plain text
        if text_blocks.is_empty() {
            let plain_text = rtf_doc
                .body
                .iter()
                .map(|b| b.text.as_str())
                .collect::<String>();
            if !plain_text.is_empty() {
                let mut text_block = TextBlock::new(Rect::default());
                text_block.add_run(TextRun {
                    text: plain_text,
                    style: TextStyle::default(),
                    bounds: None,
                    char_positions: None,
                });
                text_blocks.push(text_block);
            }
        }

        // Create page
        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: text_blocks.into_iter().map(ContentBlock::Text).collect(),
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

    #[tokio::test]
    async fn test_parse_rtf_with_color() {
        let parser = RtfParser::new();
        // RTF with color table: Red (index 1)
        // \cf1 selects color 1 from table
        let rtf_content =
            b"{\\rtf1\\ansi\\deff0{\\colortbl;\\red255\\green0\\blue0;}\\cf1 Red Text}";

        let context = ParseContext {
            format: Format::rtf(),
            filename: Some("color.rtf".to_string()),
            size: rtf_content.len(),
            options: prism_core::parser::ParseOptions::default(),
        };

        let result = parser.parse(Bytes::from(&rtf_content[..]), context).await;
        assert!(result.is_ok());

        let doc = result.unwrap();
        let page = &doc.pages[0];
        // Should have one text block
        if let ContentBlock::Text(block) = &page.content[0] {
            assert!(!block.runs.is_empty());
            // The parser might split runs or not.
            // We look for a run with proper color.
            let colored_runs: Vec<_> = block
                .runs
                .iter()
                .filter(|r| r.style.color.is_some())
                .collect();
            assert!(!colored_runs.is_empty(), "No colored runs found");

            let run = colored_runs[0];
            assert_eq!(run.style.color, Some("#ff0000".to_string()));
            assert!(run.text.contains("Red Text"));
        } else {
            panic!("Expected TextBlock");
        }
    }
}
