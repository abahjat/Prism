//! PPTX (Microsoft PowerPoint) parser
//!
//! Parses PPTX files into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::Document,
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Cursor;
use tracing::{debug, info};
use zip::ZipArchive;

use crate::office::relationships::Relationships;
use crate::office::slides::SlideParser;
use crate::office::utils;

/// PPTX parser
///
/// Parses Microsoft PowerPoint PPTX files into the Unified Document Model.
/// Each slide becomes a separate page in the document.
#[derive(Debug, Clone)]
pub struct PptxParser;

impl PptxParser {
    /// Create a new PPTX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data is a valid PPTX file (ZIP with ppt/ directory)
    fn is_pptx_zip(data: &[u8]) -> bool {
        // Check ZIP signature: PK (0x504B)
        if data.len() < 4 {
            return false;
        }

        if &data[0..2] != b"PK" {
            return false;
        }

        // Try to open as ZIP and check for ppt/ directory
        let cursor = std::io::Cursor::new(data);
        if let Ok(mut archive) = ZipArchive::new(cursor) {
            // Check for ppt/presentation.xml or [Content_Types].xml which covers valid Office files
            if archive.by_name("ppt/presentation.xml").is_ok() {
                return true;
            }
        }

        false
    }

    /// Parse presentation.xml to get slide IDs and order
    fn parse_presentation_xml(xml: &str) -> Result<Vec<String>> {
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut slide_rids = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    if e.name().as_ref() == b"p:sldId" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"r:id" {
                                slide_rids.push(utils::attr_value(&attr.value));
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(Error::ParseError(format!(
                        "XML error in presentation.xml: {}",
                        e
                    )))
                }
                _ => {}
            }
            buf.clear();
        }

        Ok(slide_rids)
    }
}

impl Default for PptxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PptxParser {
    fn format(&self) -> Format {
        Format::pptx()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_pptx_zip(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing PPTX file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Open PPTX as ZIP archive
        let cursor = Cursor::new(data.as_ref());
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open PPTX as ZIP: {}", e)))?;

        // 1. Read relationships to find slide filenames
        let mut rels_map: HashMap<String, String> = HashMap::new();
        if let Ok(mut rels_file) = archive.by_name("ppt/_rels/presentation.xml.rels") {
            let mut xml = String::new();
            use std::io::Read;
            rels_file.read_to_string(&mut xml).map_err(|e| {
                Error::ParseError(format!("Failed to read relationship XML: {}", e))
            })?;

            if let Ok(rels) = Relationships::from_xml(&xml) {
                // Determine target using rId
                // Relationships map ID -> Target (e.g., "rId2" -> "slides/slide1.xml")
                // We need to iterate over all parsing slideIds later
                for rid in rels.find_by_type(
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide",
                ) {
                    rels_map.insert(rid.id.clone(), rid.target.clone());
                }
                // Try generic relationship type if strict type not found (sometimes variations exist)
                // Or just map all valid targets if we filter by rId from presentation.xml

                // Just dumping all into a map for lookup is easier
                // We'll re-read to populate fully distinct from the typed find above if needed, but for now let's trust the logic below.
                // Actually, simpler:
                // Re-parse completely or just expose map values? Relationships struct hides internal map.
                // Let's assume we look up by ID one by one.
            }
        }

        // Reload relationships to keep object alive if needed, or better yet, read again since borrow checker with zip archive is tricky
        // Let's just do it in one pass: read presentation.xml, get rIds, then open relationship file again to resolve.

        // 2. Read presentation.xml to get slide order (rIds)
        let mut slide_rids = Vec::new();
        if let Ok(mut presentation_file) = archive.by_name("ppt/presentation.xml") {
            let mut xml = String::new();
            use std::io::Read;
            presentation_file.read_to_string(&mut xml).map_err(|e| {
                Error::ParseError(format!("Failed to read presentation.xml: {}", e))
            })?;
            slide_rids = Self::parse_presentation_xml(&xml)?;
        } else {
            return Err(Error::ParseError(
                "Missing ppt/presentation.xml".to_string(),
            ));
        }

        // 3. Resolve rIds to filenames
        // We need to read rels file if we haven't already popluated a map.
        // Let's do it properly now.
        let mut rid_to_target = HashMap::new();
        if let Ok(mut rels_file) = archive.by_name("ppt/_rels/presentation.xml.rels") {
            let mut xml = String::new();
            use std::io::Read;
            rels_file.read_to_string(&mut xml).map_err(|e| {
                Error::ParseError(format!("Failed to read relationship XML: {}", e))
            })?;
            let rels = Relationships::from_xml(&xml)?;

            for rid in &slide_rids {
                if let Some(rel) = rels.get(rid) {
                    rid_to_target.insert(rid.clone(), rel.target.clone());
                }
            }
        }

        // 4. Parse slides in order
        let mut pages = Vec::new();
        for (i, rid) in slide_rids.iter().enumerate() {
            if let Some(target) = rid_to_target.get(rid) {
                // Target is relative to ppt/, usually "slides/slide1.xml"
                // Zip entry name should be "ppt/" + target
                let entry_name = format!("ppt/{}", target);
                // Handle cases where target might already start with / or be relative
                // Usually it acts as "ppt/slides/slide1.xml" if target is "slides/slide1.xml"

                let mut slide_xml = String::new();
                // Try searching for the file in the archive
                // Standardize path separators
                let clean_name = entry_name.replace('\\', "/");

                if let Ok(mut file) = archive.by_name(&clean_name) {
                    use std::io::Read;
                    file.read_to_string(&mut slide_xml).map_err(|e| {
                        Error::ParseError(format!("Failed to read slide XML {}: {}", clean_name, e))
                    })?;
                } else {
                    debug!("Could not find slide file: {}", clean_name);
                    continue;
                }

                if !slide_xml.is_empty() {
                    let page = SlideParser::parse(&slide_xml, (i + 1) as u32);
                    pages.push(page);
                }
            }
        }

        // Create document metadata
        let mut metadata = Metadata::new();
        if let Some(filename) = context.filename {
            metadata.title = Some(filename);
        }
        metadata.add_custom("format", "PPTX");
        metadata.add_custom("slide_count", pages.len() as i64);

        // Build document
        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;

        info!(
            "Successfully parsed PPTX with {} slides",
            document.page_count()
        );

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PPTX Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}
