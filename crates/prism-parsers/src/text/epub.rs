// SPDX-License-Identifier: AGPL-3.0-only
//! EPUB (Electronic Publication) parser
//!
//! Parses .EPUB files (ZIP + XML + HTML) into the Unified Document Model.
//! Extracts content in reading order (spine).

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, EmbeddedBlock, EmbeddedKind, Page, PageMetadata,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::HashMap;
use std::io::Cursor;
use tracing::{debug, info, warn};
use zip::ZipArchive;

/// EPUB file parser
#[derive(Debug, Clone)]
pub struct EpubParser;

impl EpubParser {
    /// Create a new EPUB parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    // Helper to find attribute in XML event
    fn get_attr(e: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
        crate::office::utils::attr_value_opt(e, name)
    }
}

impl Default for EpubParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for EpubParser {
    fn format(&self) -> Format {
        Format::epub()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // EPUB is a ZIP. First 4 bytes: PK\x03\x04
        if data.len() < 4 || &data[0..4] != b"PK\x03\x04" {
            return false;
        }

        // EPUB must have "mimetype" file at root with "application/epub+zip"
        // We can't easily check internal files without unzip, but we can check if "mimetype" string is near text
        // or just rely on ZIP + extension.
        // For strict check, we could try to read local file header for "mimetype".
        // "mimetype" is usually the first file.
        // PK\x03\x04 (30 bytes header) "mimetype" ... "application/epub+zip"
        // 30 bytes + 8 bytes filename = 38 bytes. Content follows.

        // Let's iterate data to find "mimetype" and "application/epub+zip"
        let s = String::from_utf8_lossy(&data[..data.len().min(256)]);
        s.contains("mimetype") && s.contains("application/epub+zip")
    }

    #[allow(clippy::too_many_lines)]
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing EPUB, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let limit = crate::limits::max_decompressed_bytes(&context);
        let cursor = Cursor::new(data);
        let mut zip = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open EPUB zip: {e}")))?;

        // 1. Parse container.xml to find OPF path
        let container_xml = {
            let mut file = zip
                .by_name("META-INF/container.xml")
                .map_err(|_| Error::ParseError("Missing META-INF/container.xml".to_string()))?;
            let mut buf = String::new();
            crate::limits::read_to_string_with_limit(&mut file, &mut buf, limit)?;
            buf
        };

        let mut opf_path = String::new();
        let mut reader = Reader::from_str(&container_xml);
        reader.config_mut().trim_text(true);

        loop {
            match reader.read_event() {
                Ok(Event::Start(e) | Event::Empty(e)) => {
                    if e.name().as_ref().as_bytes() == b"rootfile"
                        && let Some(mt) = Self::get_attr(&e, b"media-type")
                        && mt == "application/oebps-package+xml"
                        && let Some(path) = Self::get_attr(&e, b"full-path")
                    {
                        opf_path = path;
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(Error::ParseError(format!(
                        "XML error in container.xml: {e}"
                    )));
                }
                _ => (),
            }
        }

        if opf_path.is_empty() {
            return Err(Error::ParseError(
                "Could not find OPF path in container.xml".to_string(),
            ));
        }

        // 2. Parse OPF to get metadata and spine
        let opf_content = {
            let mut file = zip
                .by_name(&opf_path)
                .map_err(|_| Error::ParseError(format!("Missing OPF file: {opf_path}")))?;
            let mut buf = String::new();
            crate::limits::read_to_string_with_limit(&mut file, &mut buf, limit)?;
            buf
        };

        let opf_dir = std::path::Path::new(&opf_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let mut reader = Reader::from_str(&opf_content);
        let mut manifest: HashMap<String, String> = HashMap::new(); // id -> href
        let mut spine: Vec<String> = Vec::new(); // idref list
        let mut title = None;
        let mut creator = None;

        loop {
            match reader.read_event() {
                Ok(Event::Start(e))
                    if matches!(e.name().as_ref().as_bytes(), b"dc:title" | b"dc:creator") =>
                {
                    // Read the whole element: entity references (&amp; etc.)
                    // arrive as separate events, so a single Text event may
                    // hold only part of the value.
                    let value = reader
                        .read_text(e.name())
                        .ok()
                        .and_then(|raw| {
                            quick_xml::escape::unescape(&raw)
                                .ok()
                                .map(|s| s.trim().to_string())
                        })
                        .filter(|s| !s.is_empty());
                    if e.name().as_ref().as_bytes() == b"dc:title" {
                        title = title.or(value);
                    } else {
                        creator = creator.or(value);
                    }
                }
                Ok(Event::Start(e) | Event::Empty(e)) => match e.name().as_ref().as_bytes() {
                    b"item" => {
                        if let (Some(id), Some(href)) =
                            (Self::get_attr(&e, b"id"), Self::get_attr(&e, b"href"))
                        {
                            manifest.insert(id, href);
                        }
                    }
                    b"itemref" => {
                        if let Some(idref) = Self::get_attr(&e, b"idref") {
                            spine.push(idref);
                        }
                    }
                    _ => (),
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(Error::ParseError(format!("XML error in OPF: {e}"))),
                _ => (),
            }
        }

        // 3. Extract content in spine order
        let mut pages = Vec::new();
        let mut page_num = 1;

        for idref in spine {
            if let Some(href) = manifest.get(&idref) {
                // Resolve path relative to OPF
                let full_href = if opf_dir.is_empty() {
                    href.clone()
                } else {
                    format!("{opf_dir}/{href}")
                };

                // Read content
                let content_str = if let Ok(mut file) = zip.by_name(&full_href) {
                    let mut buf = String::new();
                    match crate::limits::read_to_string_with_limit(&mut file, &mut buf, limit) {
                        Ok(_) => buf,
                        Err(e @ Error::MemoryLimitExceeded { .. }) => return Err(e),
                        Err(_) => {
                            warn!("Failed to read text from {}", full_href);
                            continue;
                        }
                    }
                } else {
                    warn!("Missing content file: {}", full_href);
                    continue;
                };

                // Embed images
                // Find all src="..." and replace with data:image/...;base64,...
                // We do a simple string replacement for now.
                // A better approach would be to parse HTML, but that's heavy.
                // We'll scan for src=" and match until ".

                let mut processed_content = content_str;
                let current_dir = std::path::Path::new(&full_href)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Find all image references
                // We look for src="([^"]+)"
                // This is a naive regex-like search
                let mut search_idx = 0;
                while let Some(start) = processed_content[search_idx..].find("src=\"") {
                    let absolute_start = search_idx + start + 5; // skip src="
                    if let Some(end) = processed_content[absolute_start..].find('"') {
                        let absolute_end = absolute_start + end;
                        let img_path = &processed_content[absolute_start..absolute_end];

                        // Check if it's an external link or data URI already
                        if !img_path.starts_with("http") && !img_path.starts_with("data:") {
                            // Resolve path relative to current HTML file
                            let full_img_path = if current_dir.is_empty() {
                                img_path.to_string()
                            } else {
                                // Handle ../ resolution if needed, but for now simple join
                                // EPUB paths are usually clean
                                format!("{current_dir}/{img_path}").replace("//", "/")
                                // basic cleanup
                            };

                            // Try to read image from zip
                            let mut img_data = Vec::new();
                            let mut found = false;

                            // Try exact path
                            if let Ok(mut file) = zip.by_name(&full_img_path) {
                                crate::limits::read_to_end_with_limit(
                                    &mut file,
                                    &mut img_data,
                                    limit,
                                )?;
                                found = true;
                            }

                            // If not found, try canonicalizing .. components
                            // (Naive implementation)
                            if !found {
                                // Try finding in OPF directory content if path fails
                                let opf_rel = format!("{opf_dir}/{img_path}");
                                if let Ok(mut file) = zip.by_name(&opf_rel) {
                                    crate::limits::read_to_end_with_limit(
                                        &mut file,
                                        &mut img_data,
                                        limit,
                                    )?;
                                    found = true;
                                }
                            }

                            if found {
                                use base64::{Engine as _, engine::general_purpose};
                                let b64 = general_purpose::STANDARD.encode(&img_data);
                                let path = std::path::Path::new(img_path);
                                let ext = path
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .unwrap_or("")
                                    .to_lowercase();

                                let mime = match ext.as_str() {
                                    "png" => "image/png",
                                    "jpg" | "jpeg" => "image/jpeg",
                                    "gif" => "image/gif",
                                    "svg" => "image/svg+xml",
                                    "webp" => "image/webp",
                                    _ => "application/octet-stream",
                                };

                                let new_src = format!("data:{mime};base64,{b64}");

                                // Replace in string (inefficient but works for MVP)
                                // We replace ONLY this occurrence
                                let prefix = &processed_content[..absolute_start];
                                let suffix = &processed_content[absolute_end..];
                                processed_content = format!("{prefix}{new_src}{suffix}");

                                // Update search index to skip the data URI we just inserted
                                search_idx = absolute_start + new_src.len();
                                continue;
                            }
                        }
                        search_idx = absolute_end;
                    } else {
                        break;
                    }
                }

                // Treat as HTML page
                let embedded = EmbeddedBlock::markup(EmbeddedKind::Html, processed_content);

                pages.push(Page {
                    number: page_num,
                    dimensions: Dimensions {
                        width: 600.0, // Standard ebook width
                        height: 800.0,
                    },
                    content: vec![ContentBlock::Embedded(embedded)],
                    metadata: PageMetadata::default(),
                    annotations: Vec::new(),
                });
                page_num += 1;
            }
        }

        if pages.is_empty() {
            return Err(Error::ParseError(
                "No content found in EPUB spine".to_string(),
            ));
        }

        // 4. Metadata
        let mut document_metadata = Metadata {
            title: title.or_else(|| context.filename.clone()),
            author: creator,
            ..Metadata::default()
        };
        document_metadata.add_custom("format", "EPUB");

        let mut document = Document::new();
        document.pages = pages;
        document.metadata = document_metadata;

        info!("Successfully parsed EPUB");
        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "EPUB Parser".to_string(),
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
    fn test_can_parse_epub_signature() {
        let parser = EpubParser::new();
        // Mock ZIP header with mimetype
        let mut data = vec![0u8; 100];
        data[0] = 0x50;
        data[1] = 0x4B;
        data[2] = 0x03;
        data[3] = 0x04; // PK..
        let s = "mimetypeapplication/epub+zip";
        for (i, b) in s.bytes().enumerate() {
            data[30 + i] = b;
        }
        assert!(parser.can_parse(&data));
    }

    #[tokio::test]
    async fn test_metadata_with_entities() {
        use std::io::Write;

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::FileOptions::default();
        let files: [(&str, &str); 4] = [
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                r#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            ),
            (
                "OEBPS/content.opf",
                r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>AT&amp;T &lt;Annual&gt; Report</dc:title><dc:creator>Smith &amp; Wesson</dc:creator></metadata><manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/></spine></package>"#,
            ),
            (
                "OEBPS/c1.xhtml",
                "<html><body><p>Chapter one</p></body></html>",
            ),
        ];
        for (name, content) in files {
            zip.start_file(name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        let data = Bytes::from(zip.finish().unwrap().into_inner());

        let parser = EpubParser::new();
        let context = ParseContext {
            format: parser.format(),
            filename: Some("book.epub".to_string()),
            size: data.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        let document = parser.parse(data, context).await.unwrap();
        assert_eq!(
            document.metadata.title.as_deref(),
            Some("AT&T <Annual> Report")
        );
        assert_eq!(document.metadata.author.as_deref(), Some("Smith & Wesson"));
    }
}
