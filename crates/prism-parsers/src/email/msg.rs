// SPDX-License-Identifier: AGPL-3.0-only
//! MSG (Outlook Message) parser
//!
//! Parses .MSG files (Microsoft Outlook message format) into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use cfb::CompoundFile;
use chrono::{DateTime, TimeZone, Utc};
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

    /// Format email header as HTML div
    fn format_header_html(label: &str, value: &str) -> String {
        let value_escaped = html_escape(value);
        format!(
            r#"<div class="header-row" style="margin-bottom: 4px;"><span class="header-label" style="font-weight: bold; display: inline-block; width: 60px; color: #555;">{label}:</span> <span class="header-value">{value_escaped}</span></div>"#
        )
    }

    /// Extract string property from MSG file (tries Unicode then ASCII)
    fn extract_string_property(
        comp: &mut CompoundFile<Cursor<&[u8]>>,
        base_tag: &str,
    ) -> Option<String> {
        Self::extract_raw_string(comp, &format!("__substg1.0_{base_tag}001F")) // Unicode
            .or_else(|| Self::extract_raw_string(comp, &format!("__substg1.0_{base_tag}001E")))
        // ASCII
    }

    /// Extract raw string bytes and handle encoding
    fn extract_raw_string(
        comp: &mut CompoundFile<Cursor<&[u8]>>,
        prop_path: &str,
    ) -> Option<String> {
        use std::io::Read;
        comp.open_stream(prop_path).ok().and_then(|mut stream| {
            let mut buffer = Vec::new();
            stream.read_to_end(&mut buffer).ok()?;

            if prop_path.ends_with("001F") {
                // UTF-16LE
                if buffer.len() >= 2 {
                    let utf16_chars: Vec<u16> = buffer
                        .chunks_exact(2)
                        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                        .take_while(|&c| c != 0)
                        .collect();
                    String::from_utf16(&utf16_chars).ok()
                } else {
                    None
                }
            } else {
                // String8 / Binary stream - Try UTF-8 first
                let bytes: Vec<u8> = buffer.iter().copied().take_while(|&b| b != 0).collect();
                if let Ok(s) = String::from_utf8(bytes) {
                    return Some(s);
                }

                // Fallback trial for Binary (0102) if it contains UTF-16
                if buffer.len() >= 2 && buffer.len() % 2 == 0 {
                    let utf16_chars: Vec<u16> = buffer
                        .chunks_exact(2)
                        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                        .take_while(|&c| c != 0)
                        .collect();
                    if let Ok(s) = String::from_utf16(&utf16_chars) {
                        return Some(s);
                    }
                }
                None
            }
        })
    }

    /// Extract `FILETIME` property and convert to `DateTime`
    fn extract_filetime_property(
        comp: &mut CompoundFile<Cursor<&[u8]>>,
        prop_path: &str,
    ) -> Option<DateTime<Utc>> {
        use std::io::Read;
        const TICKS_PER_SECOND: u64 = 10_000_000;
        const EPOCH_DIFFERENCE: u64 = 11_644_473_600;

        if let Ok(mut stream) = comp.open_stream(prop_path) {
            let mut buffer = [0u8; 8];
            if stream.read_exact(&mut buffer).is_ok() {
                let ticks = u64::from_le_bytes(buffer);
                // `FILETIME` is 100ns intervals since Jan 1, 1601
                // Unix epoch is Jan 1, 1970
                // Difference is 11,644,473,600 seconds
                let seconds = (ticks / TICKS_PER_SECOND).saturating_sub(EPOCH_DIFFERENCE);
                #[allow(clippy::cast_possible_truncation)]
                let nanos = ((ticks % TICKS_PER_SECOND) * 100) as u32;

                // Use timestamp_opt for compatibility
                #[allow(clippy::cast_possible_wrap)]
                return Utc.timestamp_opt(seconds as i64, nanos).single();
            }
        }
        None
    }

    /// Extract attachments from MSG file
    fn extract_attachments(
        comp: &mut CompoundFile<Cursor<&[u8]>>,
    ) -> Vec<prism_core::document::Attachment> {
        use std::io::Read;
        let mut attachments = Vec::new();

        // CFB crate doesn't easily list root entries matching a pattern.
        // We iterate indices 0..100 mostly.
        for i in 0..100 {
            let attach_storage_name = format!("__attach_version1.0_{i:08}");

            if comp.is_storage(&attach_storage_name) {
                let base = &attach_storage_name;

                // Filename: 0x3707 (Long) or 0x3704 (Short)
                let filename = Self::extract_string_property(comp, "3707")
                    .or_else(|| Self::extract_string_property(comp, "3704"))
                    .unwrap_or_else(|| format!("attachment_{i}"));

                // Mime Type: 0x370E
                let mime_type = Self::extract_string_property(comp, "370E");

                // Data: 0x3701 (Binary - 0102)
                let data_path = format!("{base}/__substg1.0_37010102");
                let data = if let Ok(mut stream) = comp.open_stream(&data_path) {
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
        // Check for CFB signature
        data.len() >= 8 && &data[0..8] == b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1"
    }

    /// # Errors
    /// Returns error if MSG data invalid
    #[allow(clippy::too_many_lines)]
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing MSG email, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let cursor = Cursor::new(&data[..]);
        let mut comp = CompoundFile::open(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open MSG as CFB: {e}")))?;

        // Common MSG properties: __substg1.0_XXXXYYYY
        let mut metadata = Metadata::default();
        let mut headers_html = String::from(
            r#"<div class="msg-headers" style="background: #f5f5f5; padding: 15px; border-bottom: 1px solid #ddd; border-radius: 4px 4px 0 0; font-family: system-ui, sans-serif; font-size: 14px; color: #333;">"#,
        );

        // From/Sender (0x0C1A, 0x0C1F, 0x0042)
        if let Some(sender_name) = Self::extract_string_property(&mut comp, "0C1A") {
            headers_html.push_str(&Self::format_header_html("From", &sender_name));
            metadata.author = Some(sender_name);
        } else if let Some(sender_email) = Self::extract_string_property(&mut comp, "0C1F") {
            headers_html.push_str(&Self::format_header_html("From", &sender_email));
            metadata.author = Some(sender_email);
        }

        // To (0x0E04)
        if let Some(to) = Self::extract_string_property(&mut comp, "0E04") {
            headers_html.push_str(&Self::format_header_html("To", &to));
        }

        // Cc (0x0E03)
        if let Some(cc) = Self::extract_string_property(&mut comp, "0E03") {
            headers_html.push_str(&Self::format_header_html("Cc", &cc));
        }

        // Subject (0x0037)
        if let Some(subject) = Self::extract_string_property(&mut comp, "0037") {
            headers_html.push_str(&Self::format_header_html("Subject", &subject));
            metadata.title = Some(subject);
        }

        // Date extraction with fallbacks
        let mut msg_date = None;
        let mut date_label = "Date";

        // 1. Client Submit Time (0x0039)
        if let Some(sent_time) = Self::extract_filetime_property(&mut comp, "__substg1.0_00390040")
        {
            msg_date = Some(sent_time);
        }
        // 2. Message Delivery Time (0x0E06)
        else if let Some(delivery_time) =
            Self::extract_filetime_property(&mut comp, "__substg1.0_0E060040")
        {
            msg_date = Some(delivery_time);
        }
        // 3. Creation Time (0x3007) - Common for Sticky Notes
        else if let Some(creation_time) =
            Self::extract_filetime_property(&mut comp, "__substg1.0_30070040")
        {
            msg_date = Some(creation_time);
            date_label = "Created";
        }

        if let Some(date) = msg_date {
            let formatted = date.format("%a %-m/%-d/%Y %-I:%M:%S %p").to_string();
            headers_html.push_str(&Self::format_header_html(date_label, &formatted));
            metadata.created = Some(date);
        }

        // Message Class (0x001A) - for identification
        if let Some(msg_class) = Self::extract_string_property(&mut comp, "001A") {
            if msg_class.to_lowercase().contains("stickynote") {
                headers_html.push_str(&Self::format_header_html("Type", "Sticky Note"));
            }
        }

        headers_html.push_str("</div>");

        // Body: Try HTML (0x1013) then Text (0x1000)
        let body_content = if let Some(html_body) =
            Self::extract_raw_string(&mut comp, "__substg1.0_10130102")
                .or_else(|| Self::extract_string_property(&mut comp, "1013"))
        {
            // Found HTML body. Clean it up.
            clean_html_body(&html_body)
        } else if let Some(text_body) = Self::extract_string_property(&mut comp, "1000") {
            // Text body: wrap in pre/div
            format!(
                r#"<div style="white-space: pre-wrap; font-family: system-ui, sans-serif; color: #333;">{}</div>"#,
                html_escape(&text_body)
            )
        } else {
            String::from(r#"<div style="font-style: italic; color: #777;">[No message body]</div>"#)
        };

        // Combine into full HTML
        let full_html = format!(
            r#"__HTML_RAW__:<div class="msg-container" style="background: white; border: 1px solid #ddd; border-radius: 4px; box-shadow: 0 2px 4px rgba(0,0,0,0.05); margin: 20px;">{headers_html}<div class="msg-body" style="padding: 20px; font-family: system-ui, sans-serif;">{body_content}</div></div>"#
        );

        let attachments = Self::extract_attachments(&mut comp);
        metadata.add_custom("format", "MSG");
        metadata.add_custom(
            "attachment_count",
            i64::try_from(attachments.len()).unwrap_or(0),
        );

        let text_run = TextRun {
            text: full_html,
            style: TextStyle::default(),
            bounds: None,
            char_positions: None,
        };

        let text_block = TextBlock {
            bounds: Rect::default(),
            runs: vec![text_run],
            paragraph_style: None,
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

/// Helper to sanitize HTML body
fn clean_html_body(html: &str) -> String {
    // Return inner body content if present, or strip doctype/html/head
    let lower = html.to_lowercase();

    if let Some(body_start) = lower.find("<body") {
        if let Some(content_start) = html[body_start..].find('>') {
            let start = body_start + content_start + 1;
            if let Some(body_end) = lower.find("</body>") {
                if body_end > start {
                    return html[start..body_end].to_string();
                }
            }
            return html[start..].to_string();
        }
    }

    // If no body tag, check for HTML tag
    if let Some(html_start) = lower.find("<html") {
        if let Some(content_start) = html[html_start..].find('>') {
            let start = html_start + content_start + 1;
            if let Some(html_end) = lower.rfind("</html>") {
                return html[start..html_end].to_string();
            }
        }
    }

    // Just return as is, maybe strip DOCTYPE
    if let Some(doctype_end) = lower.find('>') {
        if lower.trim_start().starts_with("<!doctype") {
            return html[doctype_end + 1..].to_string();
        }
    }

    html.to_string()
}

/// HTML escape function
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    #[test]
    fn test_can_parse_msg() {
        let parser = MsgParser::new();
        let msg_header = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
        assert!(parser.can_parse(msg_header));
    }

    #[tokio::test]
    async fn test_parse_msg_content() -> Result<()> {
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut comp =
                CompoundFile::create(&mut buffer).map_err(|e| Error::ParseError(e.to_string()))?;

            // 1. Sender (0C1A) - Unicode
            let sender = "Sender Name".encode_utf16().collect::<Vec<u16>>();
            let mut sender_bytes = Vec::new();
            for c in sender {
                sender_bytes.extend_from_slice(&c.to_le_bytes());
            }
            sender_bytes.push(0);
            sender_bytes.push(0);
            comp.create_stream("__substg1.0_0C1A001F")?
                .write_all(&sender_bytes)?;

            // 2. Subject (0037) - ASCII
            // Using 001E for ASCII test
            comp.create_stream("__substg1.0_0037001E")?
                .write_all(b"Test Subject ASCII\0")?;

            // 3. Body HTML (1013) - Binary (Using UTF-8 for test)
            let body_html = "<html><body><p>This is the HTML body.</p></body></html>";
            comp.create_stream("__substg1.0_10130102")?
                .write_all(body_html.as_bytes())?;

            // 4. Creation Time (3007)
            let time_bytes = 133_170_048_000_000_000u64.to_le_bytes();
            comp.create_stream("__substg1.0_30070040")?
                .write_all(&time_bytes)?;

            // 5. Message Class (001A) - ASCII
            comp.create_stream("__substg1.0_001A001E")?
                .write_all(b"IPM.StickyNote\0")?;

            // Attachments
            let attach_storage = "__attach_version1.0_00000000";
            comp.create_storage(attach_storage)?;
            // Filename - Unicode
            let filename = "test.txt".encode_utf16().collect::<Vec<u16>>();
            let mut filename_bytes = Vec::new();
            for c in filename {
                filename_bytes.extend_from_slice(&c.to_le_bytes());
            }
            filename_bytes.push(0);
            filename_bytes.push(0);
            comp.create_stream(format!("{attach_storage}/__substg1.0_3707001F"))?
                .write_all(&filename_bytes)?;
            comp.create_stream(format!("{attach_storage}/__substg1.0_37010102"))?
                .write_all(b"Hello Attachment")?;
        }

        let data = Bytes::from(buffer.into_inner());
        let parser = MsgParser::new();
        let context = ParseContext {
            format: parser.format(),
            filename: Some("test.msg".to_string()),
            size: data.len(),
            options: prism_core::parser::ParseOptions::default(),
        };

        let document = parser.parse(data, context).await?;

        // Verify Metadata
        assert_eq!(
            document.metadata.title.as_deref(),
            Some("Test Subject ASCII")
        );
        assert_eq!(document.metadata.author.as_deref(), Some("Sender Name"));

        // Verify Content
        let page = &document.pages[0];
        if let ContentBlock::Text(text_block) = &page.content[0] {
            assert_eq!(text_block.runs.len(), 1);
            let full_text = &text_block.runs[0].text;

            // Check prefix
            assert!(full_text.starts_with("__HTML_RAW__:"));

            // Check headers
            assert!(full_text.contains("Sender Name"));
            assert!(full_text.contains("Test Subject ASCII"));
            assert!(full_text.contains("Created")); // Fallback to 3007
            assert!(full_text.contains("Sticky Note")); // Message Class detection

            // Check Body
            assert!(full_text.contains("This is the HTML body."));
        } else {
            panic!("Expected text block");
        }

        Ok(())
    }
}
