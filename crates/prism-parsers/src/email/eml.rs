//! EML (Email Message) parser
//!
//! Parses .EML files (RFC 822/MIME email messages) into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use chrono::DateTime;
use mail_parser::MessageParser;
use prism_core::{
    document::{ContentBlock, Dimensions, Document, Page, TextBlock, TextRun, TextStyle},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{Parser, ParseContext, ParserFeature, ParserMetadata},
};
use tracing::{debug, info};

/// EML email parser
#[derive(Debug, Clone)]
pub struct EmlParser;

impl EmlParser {
    /// Create a new EML parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Format email headers as text content
    fn format_email_header(&self, label: &str, value: &str) -> TextRun {
        TextRun {
            text: format!("{}: {}\n", label, value),
            style: TextStyle {
                bold: label == "From" || label == "To" || label == "Subject",
                ..Default::default()
            },
            bounds: None,
            char_positions: None,
        }
    }
}

impl Default for EmlParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for EmlParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "message/rfc822".to_string(),
            extension: "eml".to_string(),
            family: prism_core::format::FormatFamily::Email,
            name: "Email Message".to_string(),
            is_container: false,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // Check for common email headers
        let text = String::from_utf8_lossy(&data[..data.len().min(1024)]);
        text.contains("From:") && (text.contains("To:") || text.contains("Subject:"))
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing EML email, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Parse email using mail-parser
        let message = MessageParser::default()
            .parse(&data[..])
            .ok_or_else(|| Error::ParseError("Failed to parse EML file".to_string()))?;

        let mut text_runs = Vec::new();

        // Extract From header
        if let Some(from) = message.from() {
            let from_str = from
                .first()
                .map(|addr| {
                    if let Some(name) = &addr.name {
                        let email = addr.address.as_ref().map(|a| a.to_string()).unwrap_or_default();
                        format!("{} <{}>", name, email)
                    } else {
                        addr.address.as_ref().map(|a| a.to_string()).unwrap_or_default()
                    }
                })
                .unwrap_or_default();
            text_runs.push(self.format_email_header("From", &from_str));
        }

        // Extract Sent date
        if let Some(date) = message.date() {
            text_runs.push(self.format_email_header("Sent", &date.to_rfc3339()));
        }

        // Extract To header
        if let Some(to) = message.to() {
            let to_str = to
                .iter()
                .map(|addr| {
                    if let Some(name) = &addr.name {
                        let email = addr.address.as_ref().map(|a| a.to_string()).unwrap_or_default();
                        format!("{} <{}>", name, email)
                    } else {
                        addr.address.as_ref().map(|a| a.to_string()).unwrap_or_default()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            text_runs.push(self.format_email_header("To", &to_str));
        }

        // Extract Subject
        if let Some(subject) = message.subject() {
            text_runs.push(self.format_email_header("Subject", subject));
        }

        // Add empty line separator
        text_runs.push(TextRun {
            text: "\n".to_string(),
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });

        // Extract body text
        let body_text = if let Some(text_body) = message.body_text(0) {
            text_body.to_string()
        } else if let Some(html_body) = message.body_html(0) {
            // If only HTML body, strip tags (basic)
            html_body.to_string()
        } else {
            String::from("[No message body]")
        };

        text_runs.push(TextRun {
            text: body_text,
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });

        // Create text block with all runs
        let text_block = TextBlock {
            runs: text_runs,
            bounds: prism_core::document::Rect {
                x: 0.0,
                y: 0.0,
                width: Dimensions::LETTER.width,
                height: Dimensions::LETTER.height,
            },
            paragraph_style: None,
        };

        // Create page
        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: vec![ContentBlock::Text(text_block)],
            metadata: Default::default(),
            annotations: Vec::new(),
        };

        // Create metadata
        let mut metadata = Metadata::default();
        if let Some(subject) = message.subject() {
            metadata.title = Some(subject.to_string());
        }
        if let Some(from) = message.from() {
            if let Some(addr) = from.first() {
                if let Some(name) = &addr.name {
                    metadata.author = Some(name.to_string());
                }
            }
        }
        if let Some(date) = message.date() {
            if let Some(dt) = DateTime::from_timestamp(date.to_timestamp(), 0) {
                metadata.created = Some(dt);
            }
        }
        metadata.add_custom("format", "EML");

        // Create document
        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;

        info!("Successfully parsed EML email");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "EML Parser".to_string(),
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
    fn test_can_parse_eml() {
        let parser = EmlParser::new();
        let eml_data = b"From: sender@example.com\r\nTo: recipient@example.com\r\nSubject: Test\r\n\r\nBody";
        assert!(parser.can_parse(eml_data));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = EmlParser::new();
        let metadata = parser.metadata();
        assert_eq!(metadata.name, "EML Parser");
        assert!(!metadata.requires_sandbox);
    }
}
