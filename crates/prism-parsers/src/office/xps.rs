// SPDX-License-Identifier: AGPL-3.0-only
//! XPS (XML Paper Specification) parser
//!
//! Parses .XPS and .OXPS files (ZIP + XML) into the Unified Document Model.
//! Extracts content from `FixedPage` parts.

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
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::io::Cursor;
use tracing::{debug, info, warn};
use zip::ZipArchive;

/// XPS file parser
#[derive(Debug, Clone)]
pub struct XpsParser;

impl XpsParser {
    /// Create a new XPS parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    // Helper to find attribute in XML event
    fn get_attr(e: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
        e.attributes()
            .flatten()
            .find(|a| a.key.as_ref() == name)
            .map(|a| String::from_utf8_lossy(&a.value).into_owned())
    }

    // Helper to read file from zip
    fn read_zip_file(zip: &mut ZipArchive<Cursor<Bytes>>, name: &str) -> Result<String> {
        let mut file = zip
            .by_name(name)
            .map_err(|_| Error::ParseError(format!("Missing file: {name}")))?;
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut file, &mut buf)
            .map_err(|e| Error::ParseError(format!("Failed to read {name}: {e}")))?;
        Ok(buf)
    }
}

impl Default for XpsParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for XpsParser {
    fn format(&self) -> Format {
        Format::xps()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // ZIP check
        if data.len() < 4 || &data[0..4] != b"PK\x03\x04" {
            return false;
        }

        // Check for FixedDocumentSequence usually
        // Or [Content_Types].xml which mentions application/vnd.ms-package.xps-fixeddocumentsequence+xml
        // Simple scan for now
        let s = String::from_utf8_lossy(&data[..data.len().min(4096)]);
        s.contains("FixedDocumentSequence.fdseq") || s.contains("FixedDocument.fdoc")
    }

    #[allow(clippy::too_many_lines)]
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing XPS file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let cursor = Cursor::new(data.clone());
        let mut zip = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open XPS zip: {e}")))?;

        // 1. Find FixedDocumentSequence
        // Typically Documents/1/FixedDocumentSequence.fdseq or similar.
        // We'll search for file ending in .fdseq
        let fdseq_path = {
            let mut found = None;
            for i in 0..zip.len() {
                if let Ok(file) = zip.by_index(i) {
                    if std::path::Path::new(file.name())
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("fdseq"))
                    {
                        found = Some(file.name().to_string());
                        break;
                    }
                }
            }
            found.ok_or_else(|| Error::ParseError("No .fdseq file found".to_string()))?
        };

        let fdseq_content = Self::read_zip_file(&mut zip, &fdseq_path)?;

        // 2. Parse fdseq to find FixedDocument refs
        let mut doc_refs = Vec::new();
        let mut reader = Reader::from_str(&fdseq_content);
        reader.trim_text(true);

        loop {
            match reader.read_event() {
                Ok(Event::Start(e) | Event::Empty(e)) => {
                    if e.name().as_ref() == b"DocumentReference" {
                        if let Some(source) = Self::get_attr(&e, b"Source") {
                            doc_refs.push(source);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(Error::ParseError(format!("XML error in fdseq: {e}"))),
                _ => (),
            }
        }

        // 3. Parse FixedDocuments to find PageContent refs
        let mut page_refs = Vec::new();
        let base_dir = std::path::Path::new(&fdseq_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        for doc_ref in doc_refs {
            // Resolve path
            // doc_ref might be absolute in package (/Documents/1/FixedDocument.fdoc) or relative
            let path = if doc_ref.starts_with('/') {
                doc_ref.trim_start_matches('/').to_string()
            } else if base_dir.is_empty() {
                doc_ref
            } else {
                format!("{base_dir}/{doc_ref}")
            };

            let doc_content = match Self::read_zip_file(&mut zip, &path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to read FixedDocument {path}: {e}");
                    continue;
                }
            };

            let mut reader = Reader::from_str(&doc_content);
            reader.trim_text(true);

            // Keep track of doc base for pages
            let doc_base = std::path::Path::new(&path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            loop {
                match reader.read_event() {
                    Ok(Event::Start(e) | Event::Empty(e)) => {
                        if e.name().as_ref() == b"PageContent" {
                            if let Some(source) = Self::get_attr(&e, b"Source") {
                                // Resolve page path
                                let page_path = if source.starts_with('/') {
                                    source.trim_start_matches('/').to_string()
                                } else if doc_base.is_empty() {
                                    source
                                } else {
                                    format!("{doc_base}/{source}")
                                };
                                page_refs.push(page_path);
                            }
                        }
                    }
                    Ok(Event::Eof) | Err(_) => break, // Ignore errors in doc parsing to continue
                    _ => (),
                }
            }
        }

        // 4. Parse Pages (extract Glyphs)
        let mut pages = Vec::new();
        let mut page_num = 1;

        for page_path in page_refs {
            let page_content = match Self::read_zip_file(&mut zip, &page_path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to read FixedPage {page_path}: {e}");
                    continue;
                }
            };

            let mut reader = Reader::from_str(&page_content);
            reader.trim_text(true);

            let mut runs = Vec::new();
            let mut width = 816.0; // Default Letter 8.5 * 96
            let mut height = 1056.0; // Default Letter 11 * 96

            loop {
                match reader.read_event() {
                    Ok(Event::Start(e) | Event::Empty(e)) => {
                        match e.name().as_ref() {
                            b"FixedPage" => {
                                if let Some(w) = Self::get_attr(&e, b"Width") {
                                    if let Ok(val) = w.parse::<f64>() {
                                        width = val;
                                    }
                                }
                                if let Some(h) = Self::get_attr(&e, b"Height") {
                                    if let Ok(val) = h.parse::<f64>() {
                                        height = val;
                                    }
                                }
                            }
                            b"Glyphs" => {
                                if let Some(text) = Self::get_attr(&e, b"UnicodeString") {
                                    let origin_x = Self::get_attr(&e, b"OriginX")
                                        .and_then(|v| v.parse::<f64>().ok())
                                        .unwrap_or(0.0);
                                    let origin_y = Self::get_attr(&e, b"OriginY")
                                        .and_then(|v| v.parse::<f64>().ok())
                                        .unwrap_or(0.0);
                                    let font_size = Self::get_attr(&e, b"FontRenderingEmSize")
                                        .and_then(|v| v.parse::<f64>().ok())
                                        .unwrap_or(10.0);

                                    // BidiLevel, Indices, etc. ignored for now.

                                    runs.push(TextRun {
                                        text,
                                        style: TextStyle {
                                            font_size: Some(font_size),
                                            ..TextStyle::default()
                                        },
                                        bounds: Some(Rect::new(origin_x, origin_y, 0.0, font_size)), // Crude bounds
                                        char_positions: Some(Vec::new()),
                                    });
                                }
                            }
                            _ => (),
                        }
                    }
                    Ok(Event::Eof) | Err(_) => break,
                    _ => (),
                }
            }

            if !runs.is_empty() {
                let text_block = TextBlock {
                    runs,
                    bounds: Rect::new(0.0, 0.0, width, height),
                    paragraph_style: None,
                    style: prism_core::document::ShapeStyle::default(),
                    rotation: 0.0,
                };

                pages.push(Page {
                    number: page_num,
                    dimensions: Dimensions { width, height },
                    content: vec![ContentBlock::Text(text_block)],
                    metadata: PageMetadata::default(),
                    annotations: Vec::new(),
                });
                page_num += 1;
            }
        }

        if pages.is_empty() {
            return Err(Error::ParseError(
                "No content found in XPS documents".to_string(),
            ));
        }

        let mut document = Document::new();
        document.metadata = Metadata {
            title: context.filename.clone(),
            ..Metadata::default()
        };
        document.metadata.add_custom("format", "XPS");
        document.pages = pages;

        info!("Successfully parsed XPS");
        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "XPS Parser".to_string(),
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
    fn test_can_parse_xps() {
        let parser = XpsParser::new();
        let mut data = vec![0u8; 100];
        data[0] = 0x50;
        data[1] = 0x4B;
        data[2] = 0x03;
        data[3] = 0x04; // PK..
        let s = "FixedDocumentSequence.fdseq";
        for (i, b) in s.bytes().enumerate() {
            data[30 + i] = b;
        }
        assert!(parser.can_parse(&data));
    }
}
