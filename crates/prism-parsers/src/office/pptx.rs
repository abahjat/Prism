// SPDX-License-Identifier: AGPL-3.0-only
//! PPTX (Microsoft PowerPoint) parser
//!
//! Parses PPTX files into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{Dimensions, Document},
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
use image::ImageReader;
use prism_core::document::ImageResource;
use std::collections::HashSet;

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

    /// Parse presentation.xml to get slide IDs and dimensions
    fn parse_presentation_xml(xml: &str) -> Result<(Vec<String>, Dimensions)> {
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut slide_rids = Vec::new();
        let mut dimensions = Dimensions::new(960.0, 540.0); // Default 16:9

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    match e.name().as_ref() {
                        b"p:sldId" => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"r:id" {
                                    slide_rids.push(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        b"p:sldSz" => {
                            let mut width = 12192000.0;
                            let mut height = 6858000.0;

                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    b"cx" => {
                                        if let Ok(val) =
                                            utils::attr_value(&attr.value).parse::<f64>()
                                        {
                                            width = val;
                                        }
                                    }
                                    b"cy" => {
                                        if let Ok(val) =
                                            utils::attr_value(&attr.value).parse::<f64>()
                                        {
                                            height = val;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            // Convert EMUs to points (1 pt = 12700 EMUs)
                            dimensions = Dimensions::new(width / 12700.0, height / 12700.0);
                        }
                        _ => {}
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

        Ok((slide_rids, dimensions))
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
                for rid in rels.map.values() {
                    if rid.rel_type == "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" {
                        rels_map.insert(rid.id.clone(), rid.target.clone());
                    }
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
        let (slide_rids, dimensions) =
            if let Ok(mut presentation_file) = archive.by_name("ppt/presentation.xml") {
                let mut xml = String::new();
                use std::io::Read;
                presentation_file.read_to_string(&mut xml).map_err(|e| {
                    Error::ParseError(format!("Failed to read presentation.xml: {}", e))
                })?;
                Self::parse_presentation_xml(&xml)?
            } else {
                return Err(Error::ParseError(
                    "Missing ppt/presentation.xml".to_string(),
                ));
            };

        // 3. Resolve rIds to filenames
        // We need to read rels file if we haven't already popluated a map.
        // Let's do it properly now.
        // 4. Parse theme (if available)
        // Find relationship of type theme
        let mut theme_name = None;
        let mut major_font = None;
        let mut minor_font = None;

        // Let's refactor the previous block to keep `rels` available.
        let mut rid_to_target = HashMap::new();
        let mut theme_target = None;

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

            // Find theme
            for rel in rels.map.values() {
                if rel.rel_type
                    == "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme"
                {
                    theme_target = Some(rel.target.clone());
                    break;
                }
            }
        }

        if let Some(target) = theme_target {
            let entry_name = format!("ppt/{}", target);
            let clean_name = entry_name.replace('\\', "/");
            if let Ok(mut theme_file) = archive.by_name(&clean_name) {
                let mut theme_xml = Vec::new();
                use std::io::Read;
                if theme_file.read_to_end(&mut theme_xml).is_ok() {
                    if let Ok(theme) = crate::office::theme::parse_theme(&theme_xml) {
                        debug!("Parsed theme: {}", theme.name);
                        theme_name = Some(theme.name);
                        major_font = theme.major_font;
                        minor_font = theme.minor_font;
                    }
                }
            }
        }

        // 5. Parse slides in order
        let mut pages = Vec::new();
        let mut images = Vec::new();
        let mut loaded_images: HashSet<String> = HashSet::new();

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
                    // Load slide relationships to resolve images
                    // Path format: ppt/slides/slide1.xml -> ppt/slides/_rels/slide1.xml.rels
                    let mut slide_rels = HashMap::new();
                    if let Some((dir, filename)) = clean_name.rsplit_once('/') {
                        let rels_path = format!("{}/_rels/{}.rels", dir, filename);
                        use std::io::Read; // Ensure Read is imported for ZipFile

                        // ... existing code ...

                        if let Ok(mut rels_file) = archive.by_name(&rels_path) {
                            let mut xml = String::new();
                            if rels_file.read_to_string(&mut xml).is_ok() {
                                if let Ok(rels) = Relationships::from_xml(&xml) {
                                    for rel in rels.map.values() {
                                        slide_rels.insert(rel.id.clone(), rel.target.clone());
                                    }
                                }
                            }
                        }

                        // Extract images referenced by this slide
                        for target in slide_rels.values() {
                            // Target is usually relative like "../media/image1.png"
                            // or "media/image2.jpeg"
                            // We need to resolve it relative to the slide directory (dir)
                            // dir is "ppt/slides" usually.

                            // Simple path resolution:
                            // Split base dir and target by '/'
                            let base_parts: Vec<&str> = dir.split('/').collect();
                            let target_parts: Vec<&str> = target.split('/').collect();

                            let mut resolved_parts = base_parts.clone();

                            for part in target_parts {
                                if part == ".." {
                                    resolved_parts.pop();
                                } else if part != "." {
                                    resolved_parts.push(part);
                                }
                            }

                            let resolved_path = resolved_parts.join("/");

                            // Check if already loaded to avoid duplicates
                            // Use the raw target as the ID, because proper parsing uses the target string from relationships
                            let image_id = target.clone();

                            // We use a composite key for loaded_images to ensure we don't load the same ZIP entry multiple times
                            // But we might need to duplicate resources if they have different IDs (targets) but point to same file?
                            // No, renderer looks up by ID.
                            // If two slides refer to "../media/img1.png", they have same ID.
                            // If one refers to "../media/img1.png" and another "media/img1.png" (same file), they have different IDs.
                            // We should store both, pointing to same data.

                            if !loaded_images.contains(&image_id) {
                                let clean_path = resolved_path.replace('\\', "/");
                                if let Ok(mut img_file) = archive.by_name(&clean_path) {
                                    let mut img_data = Vec::new();
                                    if img_file.read_to_end(&mut img_data).is_ok() {
                                        // Determine mime type
                                        let mime_type = if clean_path.ends_with(".png") {
                                            "image/png"
                                        } else if clean_path.ends_with(".jpg")
                                            || clean_path.ends_with(".jpeg")
                                        {
                                            "image/jpeg"
                                        } else if clean_path.ends_with(".gif") {
                                            "image/gif"
                                        } else if clean_path.ends_with(".svg") {
                                            "image/svg+xml"
                                        } else {
                                            "application/octet-stream"
                                        };

                                        let (width, height) = if mime_type == "image/svg+xml" {
                                            (0, 0)
                                        } else {
                                            match ImageReader::new(std::io::Cursor::new(&img_data))
                                                .with_guessed_format()
                                            {
                                                Ok(reader) => {
                                                    reader.into_dimensions().unwrap_or((0, 0))
                                                }
                                                Err(_) => (0, 0),
                                            }
                                        };

                                        images.push(ImageResource {
                                            id: image_id.clone(),
                                            data: Some(img_data),
                                            mime_type: mime_type.to_string(),
                                            url: None,
                                            width,
                                            height,
                                        });
                                        loaded_images.insert(image_id);
                                    }
                                }
                            }
                        }
                    }

                    let page =
                        SlideParser::parse(&slide_xml, (i + 1) as u32, &slide_rels, dimensions);
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
        if let Some(name) = theme_name {
            metadata.add_custom("theme_name", name);
        }
        if let Some(font) = major_font {
            metadata.add_custom("theme_font_major", font);
        }
        if let Some(font) = minor_font {
            metadata.add_custom("theme_font_minor", font);
        }

        // Build document
        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;
        document.resources.images = images;

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
