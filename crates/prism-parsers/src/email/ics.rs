// SPDX-License-Identifier: AGPL-3.0-only
//! ICS (iCalendar) parser
//!
//! Parses .ICS files (iCalendar format) into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use ical::parser::ical::component::IcalCalendar;
use ical::IcalParser;
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

/// ICS iCalendar parser
#[derive(Debug, Clone)]
pub struct IcsParser;

impl IcsParser {
    /// Create a new ICS parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Format event field as text content
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

    /// Parse a single calendar event
    fn parse_event(&self, calendar: &IcalCalendar) -> Vec<TextRun> {
        let mut text_runs = Vec::new();

        // Parse events from the calendar
        for event in &calendar.events {
            for prop in &event.properties {
                match prop.name.as_str() {
                    "SUMMARY" => {
                        if let Some(value) = prop.value.as_ref() {
                            text_runs.push(self.format_field("Event", value, true));
                        }
                    }
                    "DTSTART" => {
                        if let Some(value) = prop.value.as_ref() {
                            text_runs.push(self.format_field("Start", value, false));
                        }
                    }
                    "DTEND" => {
                        if let Some(value) = prop.value.as_ref() {
                            text_runs.push(self.format_field("End", value, false));
                        }
                    }
                    "LOCATION" => {
                        if let Some(value) = prop.value.as_ref() {
                            text_runs.push(self.format_field("Location", value, false));
                        }
                    }
                    "DESCRIPTION" => {
                        if let Some(value) = prop.value.as_ref() {
                            text_runs.push(TextRun {
                                text: "\nDescription:\n".to_string(),
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
                    "ORGANIZER" => {
                        if let Some(value) = prop.value.as_ref() {
                            text_runs.push(self.format_field("Organizer", value, false));
                        }
                    }
                    "ATTENDEE" => {
                        if let Some(value) = prop.value.as_ref() {
                            text_runs.push(self.format_field("Attendee", value, false));
                        }
                    }
                    _ => {} // Ignore other properties
                }
            }

            // Add separator between events
            text_runs.push(TextRun {
                text: "\n---\n\n".to_string(),
                style: Default::default(),
                bounds: None,
                char_positions: None,
            });
        }

        text_runs
    }
}

impl Default for IcsParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for IcsParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "text/calendar".to_string(),
            extension: "ics".to_string(),
            family: prism_core::format::FormatFamily::Email,
            name: "iCalendar".to_string(),
            is_container: false,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // iCalendar files start with "BEGIN:VCALENDAR"
        let text = String::from_utf8_lossy(&data[..data.len().min(512)]);
        text.contains("BEGIN:VCALENDAR")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing ICS iCalendar, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let cursor = Cursor::new(&data[..]);
        let parser = IcalParser::new(cursor);

        let mut calendars = Vec::new();
        for calendar_result in parser {
            match calendar_result {
                Ok(calendar) => calendars.push(calendar),
                Err(e) => {
                    debug!("Failed to parse iCalendar: {:?}", e);
                    continue;
                }
            }
        }

        if calendars.is_empty() {
            return Err(Error::ParseError("No valid calendars found".to_string()));
        }

        let mut pages = Vec::new();
        let mut calendar_title = None;

        for (page_number, calendar) in calendars.iter().enumerate() {
            let text_runs = self.parse_event(calendar);

            // Save calendar title for metadata
            if page_number == 0 {
                for prop in &calendar.properties {
                    if prop.name == "X-WR-CALNAME" || prop.name == "NAME" {
                        calendar_title = prop.value.clone();
                        break;
                    }
                }
            }

            let text_block = TextBlock {
                bounds: Rect::new(0.0, 0.0, 0.0, 0.0), // No layout info in ICS
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
        if let Some(title) = calendar_title {
            metadata.title = Some(title);
        } else if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "ICS");
        metadata.add_custom("calendar_count", pages.len() as i64);

        // Create document
        let mut document = Document::new();
        document.pages = pages;
        document.metadata = metadata;

        info!(
            "Successfully parsed ICS with {} calendar(s)",
            document.pages.len()
        );

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "ICS Parser".to_string(),
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
    fn test_can_parse_ics() {
        let parser = IcsParser::new();
        let ics_data = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nSUMMARY:Test Event\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(parser.can_parse(ics_data));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = IcsParser::new();
        let metadata = parser.metadata();
        assert_eq!(metadata.name, "ICS Parser");
        assert!(!metadata.requires_sandbox);
    }
}
