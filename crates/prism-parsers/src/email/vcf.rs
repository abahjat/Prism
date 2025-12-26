// SPDX-License-Identifier: AGPL-3.0-only
//! VCF (vCard) parser
//!
//! Parses .VCF/.VCARD files (virtual contact cards) into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use ical::parser::vcard::component::VcardContact;
use ical::VcardParser;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, Rect, ShapeStyle, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::{debug, info};

/// VCF vCard parser
#[derive(Debug, Clone)]
pub struct VcfParser;

impl VcfParser {
    /// Create a new VCF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Format contact field as text content
    fn format_field(&self, label: &str, value: &str, bold: bool) -> TextRun {
        TextRun {
            text: format!("{}: {}\n", label, value),
            style: TextStyle {
                bold,
                ..Default::default()
            },
            bounds: None,
            char_positions: None,
        }
    }

    /// Parse a single vCard contact
    fn parse_vcard(&self, contact: &VcardContact) -> Vec<TextRun> {
        let mut text_runs = Vec::new();

        // Full name (FN property)
        for prop in &contact.properties {
            match prop.name.as_str() {
                "FN" => {
                    if let Some(value) = prop.value.as_ref() {
                        text_runs.push(self.format_field("Name", value, true));
                    }
                }
                "ORG" => {
                    if let Some(value) = prop.value.as_ref() {
                        text_runs.push(self.format_field("Organization", value, false));
                    }
                }
                "TITLE" => {
                    if let Some(value) = prop.value.as_ref() {
                        text_runs.push(self.format_field("Title", value, false));
                    }
                }
                "EMAIL" => {
                    if let Some(value) = prop.value.as_ref() {
                        text_runs.push(self.format_field("Email", value, false));
                    }
                }
                "TEL" => {
                    if let Some(value) = prop.value.as_ref() {
                        text_runs.push(self.format_field("Phone", value, false));
                    }
                }
                "ADR" => {
                    if let Some(value) = prop.value.as_ref() {
                        // Address format: PO Box;Extended;Street;City;State;Postal;Country
                        let parts: Vec<&str> = value.split(';').filter(|s| !s.is_empty()).collect();
                        if !parts.is_empty() {
                            text_runs.push(self.format_field("Address", &parts.join(", "), false));
                        }
                    }
                }
                "URL" => {
                    if let Some(value) = prop.value.as_ref() {
                        text_runs.push(self.format_field("Website", value, false));
                    }
                }
                "NOTE" => {
                    if let Some(value) = prop.value.as_ref() {
                        text_runs.push(TextRun {
                            text: "\nNote:\n".to_string(),
                            style: TextStyle {
                                bold: true,
                                ..Default::default()
                            },
                            bounds: None,
                            char_positions: None,
                        });
                        text_runs.push(TextRun {
                            text: format!("{}\n", value),
                            style: Default::default(),
                            bounds: None,
                            char_positions: None,
                        });
                    }
                }
                _ => {} // Ignore other properties
            }
        }

        text_runs
    }
}

impl Default for VcfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for VcfParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "text/vcard".to_string(),
            extension: "vcf".to_string(),
            family: prism_core::format::FormatFamily::Contact,
            name: "vCard Contact".to_string(),
            is_container: false,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // vCard files start with "BEGIN:VCARD"
        let text = String::from_utf8_lossy(&data[..data.len().min(512)]);
        text.contains("BEGIN:VCARD")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing VCF vCard, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let cursor = Cursor::new(&data[..]);
        let parser = VcardParser::new(cursor);

        let mut vcards = Vec::new();
        for contact_result in parser {
            match contact_result {
                Ok(contact) => vcards.push(contact),
                Err(e) => {
                    debug!("Failed to parse vCard: {:?}", e);
                    continue;
                }
            }
        }

        if vcards.is_empty() {
            return Err(Error::ParseError("No valid vCards found".to_string()));
        }

        let mut pages = Vec::new();
        let mut contact_name = None;

        for (page_number, vcard) in vcards.iter().enumerate() {
            let text_runs = self.parse_vcard(vcard);

            // Save first contact name for metadata
            if page_number == 0 {
                for prop in &vcard.properties {
                    if prop.name == "FN" {
                        contact_name = prop.value.clone();
                        break;
                    }
                }
            }

            let text_block = TextBlock {
                bounds: Rect::new(0.0, 0.0, 0.0, 0.0), // No layout info in VCF
                runs: text_runs,
                paragraph_style: None,
                style: ShapeStyle::default(),
                rotation: 0.0,
            };

            let page = Page {
                number: (page_number + 1) as u32,
                dimensions: Dimensions::LETTER,
                content: vec![ContentBlock::Text(text_block)],
                metadata: Default::default(),
                annotations: Vec::new(),
            };

            pages.push(page);
        }

        // Create metadata
        let mut metadata = Metadata::default();
        if let Some(name) = contact_name {
            metadata.title = Some(name);
        } else if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "VCF");
        metadata.add_custom("contact_count", pages.len() as i64);

        // Create document
        let mut document = Document::new();
        document.pages = pages;
        document.metadata = metadata;

        info!(
            "Successfully parsed VCF with {} contact(s)",
            document.pages.len()
        );

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "VCF Parser".to_string(),
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
    fn test_can_parse_vcf() {
        let parser = VcfParser::new();
        let vcf_data = b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:John Doe\r\nEND:VCARD\r\n";
        assert!(parser.can_parse(vcf_data));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = VcfParser::new();
        let metadata = parser.metadata();
        assert_eq!(metadata.name, "VCF Parser");
        assert!(!metadata.requires_sandbox);
    }
}
