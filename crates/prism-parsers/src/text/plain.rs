// SPDX-License-Identifier: AGPL-3.0-only
//! Plain text file parser
//!
//! Parses plain text files (.txt, .log, .json, .xml, .csv, .md, etc.) into the Unified Document Model.
//! Creates a single-page document with text content that wraps properly.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, ShapeStyle, TextBlock,
        TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use tracing::{debug, info};

/// Plain text parser
///
/// Parses plain text files into the Unified Document Model.
/// Supports: .txt, .log
#[derive(Debug, Clone)]
pub struct TextParser;

/// JSON file parser
#[derive(Debug, Clone)]
pub struct JsonParser;

/// XML file parser
#[derive(Debug, Clone)]
pub struct XmlParser;

/// CSV file parser
#[derive(Debug, Clone)]
pub struct CsvParser;

/// Markdown file parser
#[derive(Debug, Clone)]
pub struct MarkdownParser;

/// Log file parser
#[derive(Debug, Clone)]
pub struct LogParser;

impl TextParser {
    /// Create a new text parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Detect if content is likely UTF-8 text
    fn is_likely_text(data: &[u8]) -> bool {
        // Check if it's valid UTF-8
        if std::str::from_utf8(data).is_err() {
            return false;
        }

        // Additional heuristics: check for common text patterns
        // Reject if too many null bytes or control characters (except newlines, tabs)
        let control_char_count = data
            .iter()
            .filter(|&&b| b < 32 && b != b'\n' && b != b'\r' && b != b'\t')
            .count();
        let ratio = control_char_count as f64 / data.len() as f64;

        // If more than 10% are non-text control characters, probably binary
        ratio < 0.1
    }

    /// Get the appropriate format based on file extension
    fn format_for_extension(extension: &str) -> Format {
        match extension.to_lowercase().as_str() {
            "txt" => Format {
                mime_type: "text/plain".to_string(),
                extension: "txt".to_string(),
                family: prism_core::format::FormatFamily::Text,
                name: "Plain Text".to_string(),
                is_container: false,
            },
            "log" => Format {
                mime_type: "text/plain".to_string(),
                extension: "log".to_string(),
                family: prism_core::format::FormatFamily::Text,
                name: "Log File".to_string(),
                is_container: false,
            },
            "json" => Format {
                mime_type: "application/json".to_string(),
                extension: "json".to_string(),
                family: prism_core::format::FormatFamily::Text,
                name: "JSON".to_string(),
                is_container: false,
            },
            "xml" => Format {
                mime_type: "application/xml".to_string(),
                extension: "xml".to_string(),
                family: prism_core::format::FormatFamily::Text,
                name: "XML".to_string(),
                is_container: false,
            },
            "csv" => Format {
                mime_type: "text/csv".to_string(),
                extension: "csv".to_string(),
                family: prism_core::format::FormatFamily::Text,
                name: "CSV".to_string(),
                is_container: false,
            },
            "md" | "markdown" => Format {
                mime_type: "text/markdown".to_string(),
                extension: "md".to_string(),
                family: prism_core::format::FormatFamily::Text,
                name: "Markdown".to_string(),
                is_container: false,
            },
            _ => Format {
                mime_type: "text/plain".to_string(),
                extension: extension.to_string(),
                family: prism_core::format::FormatFamily::Text,
                name: "Text File".to_string(),
                is_container: false,
            },
        }
    }
}

impl Default for TextParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for TextParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            family: prism_core::format::FormatFamily::Text,
            name: "Plain Text".to_string(),
            is_container: false,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // Accept if it's valid UTF-8 text
        Self::is_likely_text(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing text file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Convert bytes to UTF-8 string
        let text = std::str::from_utf8(&data)
            .map_err(|e| Error::ParseError(format!("Invalid UTF-8: {}", e)))?
            .to_string();

        let char_count = text.len();
        debug!("Successfully decoded {} characters of text", char_count);

        // Create a single text run with all the content
        let text_run = TextRun {
            text,
            style: TextStyle::default(),
            bounds: None,
            char_positions: None,
        };

        // Create text block with wrapping enabled (no specific bounds means it will wrap)
        let text_block = TextBlock {
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0, // 0 width signals to renderer to use full width with wrapping
                height: 0.0,
            },
            runs: vec![text_run],
            paragraph_style: None,
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        // Create single page
        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: vec![ContentBlock::Text(text_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        // Create metadata
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());

            // Determine format based on extension
            if let Some(extension) = filename.rsplit('.').next() {
                metadata.add_custom("file_extension", extension.to_string());
            }
        }

        metadata.add_custom("character_count", char_count as i64);
        metadata.add_custom(
            "line_count",
            data.iter().filter(|&&b| b == b'\n').count() as i64,
        );

        // Build document
        let mut document = Document::builder().metadata(metadata).build();

        document.pages = vec![page];

        info!(
            "Successfully parsed text file with {} characters",
            context.size
        );

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "Text Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

// Macro to implement Parser for text-based formats that all use the same logic
macro_rules! impl_text_parser {
    ($parser:ident, $format_fn:expr, $name:expr) => {
        impl $parser {
            #[must_use]
            pub fn new() -> Self {
                Self
            }
        }

        impl Default for $parser {
            fn default() -> Self {
                Self::new()
            }
        }

        #[async_trait]
        impl Parser for $parser {
            fn format(&self) -> Format {
                $format_fn()
            }

            fn can_parse(&self, data: &[u8]) -> bool {
                TextParser::is_likely_text(data)
            }

            async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
                // Reuse TextParser's parse logic
                TextParser::new().parse(data, context).await
            }

            fn metadata(&self) -> ParserMetadata {
                ParserMetadata {
                    name: $name.to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    features: vec![
                        ParserFeature::TextExtraction,
                        ParserFeature::MetadataExtraction,
                    ],
                    requires_sandbox: false,
                }
            }
        }
    };
}

impl_text_parser!(JsonParser, Format::json, "JSON Parser");
impl_text_parser!(XmlParser, Format::xml, "XML Parser");
// CsvParser moved to csv.rs
impl_text_parser!(MarkdownParser, Format::markdown, "Markdown Parser");
impl_text_parser!(LogParser, Format::log, "Log Parser");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_likely_text() {
        // Valid text
        assert!(TextParser::is_likely_text(b"Hello, world!"));
        assert!(TextParser::is_likely_text(b"Line 1\nLine 2\nLine 3"));
        assert!(TextParser::is_likely_text(b"{\"key\": \"value\"}"));

        // Binary data (has null bytes)
        let binary = [0x00, 0x01, 0x02, 0x03, 0xFF];
        assert!(!TextParser::is_likely_text(&binary));

        // Invalid UTF-8
        let invalid_utf8 = [0xFF, 0xFE, 0xFD];
        assert!(!TextParser::is_likely_text(&invalid_utf8));
    }

    #[test]
    fn test_can_parse() {
        let parser = TextParser::new();

        assert!(parser.can_parse(b"Hello, world!"));
        assert!(parser.can_parse(b"# Markdown\n## Header"));
        assert!(!parser.can_parse(&[0x00, 0x01, 0x02]));
    }

    #[tokio::test]
    async fn test_parse_simple_text() {
        let parser = TextParser::new();
        let content = "Hello, world!\nThis is a test.";
        let data = Bytes::from(content);

        let context = ParseContext {
            format: parser.format(),
            filename: Some("test.txt".to_string()),
            size: data.len(),
            options: Default::default(),
        };

        let result = parser.parse(data, context).await;
        assert!(result.is_ok());

        let document = result.unwrap();
        assert_eq!(document.page_count(), 1);
        assert_eq!(document.pages[0].content.len(), 1);

        // Verify it's a text block
        match &document.pages[0].content[0] {
            ContentBlock::Text(text_block) => {
                assert_eq!(text_block.runs.len(), 1);
                assert_eq!(text_block.runs[0].text, content);
            }
            _ => panic!("Expected text block"),
        }
    }

    #[tokio::test]
    async fn test_parse_json() {
        let parser = TextParser::new();
        let content = r#"{"name": "test", "value": 123}"#;
        let data = Bytes::from(content);

        let context = ParseContext {
            format: parser.format(),
            filename: Some("data.json".to_string()),
            size: data.len(),
            options: Default::default(),
        };

        let result = parser.parse(data, context).await;
        assert!(result.is_ok());

        let document = result.unwrap();
        assert_eq!(document.metadata.title, Some("data.json".to_string()));
    }

    #[tokio::test]
    async fn test_parse_multiline() {
        let parser = TextParser::new();
        let content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5";
        let data = Bytes::from(content);

        let context = ParseContext {
            format: parser.format(),
            filename: Some("test.log".to_string()),
            size: data.len(),
            options: Default::default(),
        };

        let result = parser.parse(data, context).await;
        assert!(result.is_ok());

        let document = result.unwrap();

        // Check metadata includes line count
        let line_count = document.metadata.get_custom("line_count");
        assert!(line_count.is_some());
    }

    #[test]
    fn test_parser_metadata() {
        let parser = TextParser::new();
        let metadata = parser.metadata();

        assert_eq!(metadata.name, "Text Parser");
        assert!(!metadata.requires_sandbox);
        assert!(metadata.features.contains(&ParserFeature::TextExtraction));
    }
}
