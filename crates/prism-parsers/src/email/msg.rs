//! MSG (Outlook Message) parser
//!
//! Parses .MSG files (Microsoft Outlook message format) into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use cfb::CompoundFile;
use prism_core::{
    document::{ContentBlock, Dimensions, Document, Page, TextBlock, TextRun, TextStyle},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::{debug, info};

/// MSG Outlook message parser
#[derive(Debug, Clone)]
pub struct MsgParser;

impl MsgParser {
    /// Create a new MSG parser
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

    /// Extract string property from MSG file
    fn extract_string_property(
        &self,
        comp: &mut CompoundFile<Cursor<&[u8]>>,
        prop_path: &str,
    ) -> Option<String> {
        comp.open_stream(prop_path).ok().and_then(|mut stream| {
            use std::io::Read;
            let mut buffer = Vec::new();
            stream.read_to_end(&mut buffer).ok()?;

            // MSG properties are often UTF-16LE encoded
            if buffer.len() >= 2 && buffer.len() % 2 == 0 {
                // Try UTF-16LE first
                let utf16_chars: Vec<u16> = buffer
                    .chunks_exact(2)
                    .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                    .take_while(|&c| c != 0) // Stop at null terminator
                    .collect();

                if let Ok(s) = String::from_utf16(&utf16_chars) {
                    return Some(s);
                }
            }

            // Fallback to UTF-8
            String::from_utf8(buffer.into_iter().take_while(|&b| b != 0).collect()).ok()
        })
    }

    /// Extract attachments from MSG file
    fn extract_attachments(
        &self,
        comp: &mut CompoundFile<Cursor<&[u8]>>,
    ) -> Vec<prism_core::document::Attachment> {
        let mut attachments = Vec::new();

        // CFB crate doesn't easily list root entries matching a pattern in a way that gives us the list of attachments.
        // We have to iterate through entries.
        // Typically attachments are named "__attach_version1.0_#" where # is the index.
        // We can try probing for indices 0..N until we fail.

        for i in 0..100 {
            // Limit to 100 attachments for sanity
            let attach_storage_name = format!("__attach_version1.0_{:08}", i);

            // Check if this storage exists (by trying to read a property inside it?)
            // Or just check if valid directory.
            if comp.is_storage(&attach_storage_name) {
                // Open the storage path, but `cfb` crate access is usually absolute paths.
                // We can extract properties relative to this storage.

                let base = &attach_storage_name;

                // Filename: 0x3707 (Long) or 0x3704 (Short)
                let filename = self
                    .extract_string_property(comp, &format!("{}/__substg1.0_3707001F", base))
                    .or_else(|| {
                        self.extract_string_property(
                            comp,
                            &format!("{}/__substg1.0_3704001F", base),
                        )
                    })
                    .unwrap_or_else(|| format!("attachment_{}", i));

                // Mime Type: 0x370E
                let mime_type =
                    self.extract_string_property(comp, &format!("{}/__substg1.0_370E001F", base));

                // Data: 0x3701 (Binary - 0102)
                let data_path = format!("{}/__substg1.0_37010102", base);
                let data = if let Ok(mut stream) = comp.open_stream(&data_path) {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    if stream.read_to_end(&mut buf).is_ok() {
                        buf
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                if !data.is_empty() {
                    attachments.push(prism_core::document::Attachment {
                        filename,
                        mime_type,
                        description: None,
                        data,
                        created: None,
                        modified: None,
                    });
                }
            } else {
                // Attachments are sequential, if 0 exists but 1 doesn't, we are done?
                // Mostly yes, but some implementations might be sparse. checking a few ahead?
                // Standard is sequential.
                break;
            }
        }

        attachments
    }
}

impl Default for MsgParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for MsgParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "application/vnd.ms-outlook".to_string(),
            extension: "msg".to_string(),
            family: prism_core::format::FormatFamily::Email,
            name: "Outlook Message".to_string(),
            is_container: false,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // MSG files use CFB (Compound File Binary) format
        // Check for CFB signature
        data.len() >= 8 && &data[0..8] == b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1"
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing MSG email, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Open as CFB file
        let cursor = Cursor::new(&data[..]);
        let mut comp = CompoundFile::open(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open MSG as CFB: {}", e)))?;

        let mut text_runs = Vec::new();

        // Extract common MSG properties
        // Property paths in MSG files follow the pattern: __substg1.0_XXXXYYYY
        // where XXXX is the property tag and YYYY is the data type

        // Sender name (0x0C1A - SENDER_NAME, 001F = Unicode string)
        if let Some(sender_name) = self.extract_string_property(&mut comp, "__substg1.0_0C1A001F") {
            text_runs.push(self.format_email_header("From", &sender_name));
        } else if let Some(sender_email) =
            self.extract_string_property(&mut comp, "__substg1.0_0C1F001F")
        {
            text_runs.push(self.format_email_header("From", &sender_email));
        }

        // Sent time (0x0039 - CLIENT_SUBMIT_TIME)
        if let Some(sent_time) = self.extract_string_property(&mut comp, "__substg1.0_00390040") {
            text_runs.push(self.format_email_header("Sent", &sent_time));
        }

        // Recipient (0x0E04 - DISPLAY_TO, 001F = Unicode string)
        if let Some(to) = self.extract_string_property(&mut comp, "__substg1.0_0E04001F") {
            text_runs.push(self.format_email_header("To", &to));
        }

        // CC (0x0E03 - DISPLAY_CC)
        if let Some(cc) = self.extract_string_property(&mut comp, "__substg1.0_0E03001F") {
            text_runs.push(self.format_email_header("Cc", &cc));
        }

        // BCC (0x0E02 - DISPLAY_BCC)
        if let Some(bcc) = self.extract_string_property(&mut comp, "__substg1.0_0E02001F") {
            text_runs.push(self.format_email_header("Bcc", &bcc));
        }

        // Subject (0x0037 - SUBJECT, 001F = Unicode string)
        if let Some(subject) = self.extract_string_property(&mut comp, "__substg1.0_0037001F") {
            text_runs.push(self.format_email_header("Subject", &subject));
        }

        // Add empty line separator
        text_runs.push(TextRun {
            text: "\n".to_string(),
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });

        // Body (0x1000 - BODY, 001F = Unicode string)
        let body_text = if let Some(body) =
            self.extract_string_property(&mut comp, "__substg1.0_1000001F")
        {
            body
        } else if let Some(body) = self.extract_string_property(&mut comp, "__substg1.0_10130102") {
            // HTML body (0x1013, 0102 = binary) - simplified handling for now, raw string fallback
            body
        } else {
            String::from("[No message body]")
        };

        text_runs.push(TextRun {
            text: body_text.clone(),
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });

        // Extract Attachments
        let attachments = self.extract_attachments(&mut comp);

        // Create text block
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
            // attachments can also be linked here? No, they are document level in UDM.
        };

        // Create metadata
        let mut metadata = Metadata::default();
        if let Some(subject) = self.extract_string_property(&mut comp, "__substg1.0_0037001F") {
            metadata.title = Some(subject);
        }
        if let Some(sender) = self.extract_string_property(&mut comp, "__substg1.0_0C1A001F") {
            metadata.author = Some(sender);
        }
        metadata.add_custom("format", "MSG");
        metadata.add_custom("attachment_count", attachments.len() as i64);

        // Create document
        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.attachments = attachments;

        info!("Successfully parsed MSG email");

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "MSG Parser".to_string(),
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
    fn test_can_parse_msg() {
        let parser = MsgParser::new();
        let msg_header = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
        assert!(parser.can_parse(msg_header));
    }

    #[test]
    fn test_parser_metadata() {
        let parser = MsgParser::new();
        let metadata = parser.metadata();
        assert_eq!(metadata.name, "MSG Parser");
        assert!(!metadata.requires_sandbox);
    }
}
