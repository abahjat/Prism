// SPDX-License-Identifier: AGPL-3.0-only
//! MBOX (Email Mailbox) parser
//!
//! Parses .MBOX files (mailbox containing multiple emails) into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use mail_parser::MessageParser;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, Rect, ShapeStyle, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use tracing::{debug, info};

/// MBOX mailbox parser
#[derive(Debug, Clone)]
pub struct MboxParser;

impl MboxParser {
    /// Create a new MBOX parser
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

    /// Parse a single message from MBOX format
    fn parse_message(&self, message_data: &[u8]) -> Result<Vec<TextRun>> {
        let message = MessageParser::default()
            .parse(message_data)
            .ok_or_else(|| Error::ParseError("Failed to parse message".to_string()))?;

        let mut text_runs = Vec::new();

        // Extract From header
        if let Some(from) = message.from() {
            let from_str = from
                .first()
                .map(|addr| {
                    if let Some(name) = &addr.name {
                        format!(
                            "{} <{}>",
                            name,
                            addr.address
                                .as_ref()
                                .map(|a| a.to_string())
                                .unwrap_or_default()
                        )
                    } else {
                        addr.address
                            .as_ref()
                            .map(|a| a.to_string())
                            .unwrap_or_default()
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
                        format!(
                            "{} <{}>",
                            name,
                            addr.address
                                .as_ref()
                                .map(|a| a.to_string())
                                .unwrap_or_default()
                        )
                    } else {
                        addr.address
                            .as_ref()
                            .map(|a| a.to_string())
                            .unwrap_or_default()
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
            text: "
"
            .to_string(),
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });

        // Extract body text
        let body_text = if let Some(text_body) = message.body_text(0) {
            text_body.to_string()
        } else if let Some(html_body) = message.body_html(0) {
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

        Ok(text_runs)
    }
}

impl Default for MboxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for MboxParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "application/mbox".to_string(),
            extension: "mbox".to_string(),
            family: prism_core::format::FormatFamily::Email,
            name: "Email Mailbox".to_string(),
            is_container: true,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // MBOX files start with "From " (note the space)
        data.starts_with(b"From ")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing MBOX mailbox, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let content = String::from_utf8_lossy(&data);
        let mut pages = Vec::new();
        let mut page_number = 1;

        // Split by "From " lines (MBOX delimiter)
        let messages: Vec<&str> = content
            .split("\nFrom ")
            .enumerate()
            .filter_map(|(i, msg)| {
                if i == 0 {
                    // First message might not have leading newline
                    if msg.starts_with("From ") {
                        Some(msg)
                    } else {
                        None
                    }
                } else {
                    // Add back the "From " prefix that was removed by split
                    Some(msg)
                }
            })
            .collect();

        info!("Found {} messages in MBOX", messages.len());

        for message_text in messages {
            // Skip the "From " envelope line and parse the actual message
            if let Some(msg_start) = message_text.find("\n") {
                let message_data = &message_text[msg_start + 1..];

                match self.parse_message(message_data.as_bytes()) {
                    Ok(text_runs) => {
                        let text_block = TextBlock {
                            bounds: Rect::new(0.0, 0.0, 0.0, 0.0), // No layout info in MBOX
                            runs: text_runs,
                            paragraph_style: None,
                            style: ShapeStyle::default(),
                            rotation: 0.0,
                        };

                        let page = Page {
                            number: page_number,
                            dimensions: Dimensions::LETTER,
                            content: vec![ContentBlock::Text(text_block)],
                            metadata: Default::default(),
                            annotations: Vec::new(),
                        };

                        pages.push(page);
                        page_number += 1;
                    }
                    Err(e) => {
                        debug!("Failed to parse message in MBOX: {}", e);
                        // Continue with other messages
                    }
                }
            }
        }

        if pages.is_empty() {
            return Err(Error::ParseError(
                "No valid messages found in MBOX".to_string(),
            ));
        }

        // Create metadata
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "MBOX");
        metadata.add_custom("message_count", pages.len() as i64);

        // Create document
        let mut document = Document::new();
        document.pages = pages;
        document.metadata = metadata;

        info!(
            "Successfully parsed MBOX with {} message(s)",
            document.pages.len()
        );

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "MBOX Parser".to_string(),
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
    fn test_can_parse_mbox() {
        let parser = MboxParser::new();
        let mbox_data =
            b"From sender@example.com Mon Jan 01 00:00:00 2024\r\nFrom: sender@example.com\r\n";
        assert!(parser.can_parse(mbox_data));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = MboxParser::new();
        let metadata = parser.metadata();
        assert_eq!(metadata.name, "MBOX Parser");
        assert!(!metadata.requires_sandbox);
    }
}
