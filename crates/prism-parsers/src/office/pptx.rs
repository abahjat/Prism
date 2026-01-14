// SPDX-License-Identifier: AGPL-3.0-only
//! `PPTX` (`Microsoft PowerPoint`) parser
//!
//! Parses `PPTX` files into the Unified Document Model.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{ContentBlock, Dimensions, Document},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use tracing::{debug, info};
use zip::ZipArchive;

use crate::office::relationships::Relationships;

use crate::office::slides::SlideParser;
use crate::office::theme::Theme;
use crate::office::utils;
use image::ImageReader;
use prism_core::document::ImageResource;
use std::collections::HashSet;

/// `PPTX` parser
///
/// Parses `Microsoft PowerPoint` `PPTX` files into the Unified Document Model.
/// Each slide becomes a separate page in the document.
#[derive(Debug, Clone)]
pub struct PptxParser {
    format: Format,
}

impl PptxParser {
    /// Create a new PPTX parser
    #[must_use]
    /// Create a new PPTX parser with default format
    pub fn new() -> Self {
        Self {
            format: Format::pptx(),
        }
    }

    /// Create a new PPTX parser with a specific format (e.g. POTX, PPSX)
    #[must_use]
    pub fn new_with_format(format: Format) -> Self {
        Self { format }
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
                Ok(Event::Start(e) | Event::Empty(e)) => {
                    match e.name().as_ref() {
                        b"p:sldId" => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"r:id" {
                                    slide_rids.push(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        b"p:sldSz" => {
                            let mut width = 12_192_000.0;
                            let mut height = 6_858_000.0;

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
                        "XML error in presentation.xml: {e}"
                    )))
                }
                _ => {}
            }
            buf.clear();
        }

        Ok((slide_rids, dimensions))
    }

    /// Helper to get relationships for a specific file in the archive
    fn get_relationships_for_file(
        archive: &mut ZipArchive<Cursor<&[u8]>>,
        entry_name: &str,
    ) -> HashMap<String, String> {
        let mut rels = HashMap::new();
        if let Some((dir, filename)) = entry_name.rsplit_once('/') {
            let rels_path = format!("{dir}/_rels/{filename}.rels");
            // Standardize path separators just in case
            let clean_path = rels_path.replace('\\', "/");
            if let Ok(mut file) = archive.by_name(&clean_path) {
                let mut xml = String::new();
                if file.read_to_string(&mut xml).is_ok() {
                    if let Ok(parsed) = Relationships::from_xml(&xml) {
                        for rel in parsed.map.values() {
                            rels.insert(rel.id.clone(), rel.target.clone());
                        }
                    }
                }
            }
        }
        rels
    }

    /// Helper to parse just the background from an XML file (layout or master)
    fn extract_background_from_xml(
        archive: &mut ZipArchive<Cursor<&[u8]>>,
        entry_name: &str,
        rels: &HashMap<String, String>,
        theme: Option<&Theme>,
    ) -> Option<ContentBlock> {
        // Read file
        let mut xml = String::new();
        let clean_name = entry_name.replace('\\', "/");
        if let Ok(mut file) = archive.by_name(&clean_name) {
            if file.read_to_string(&mut xml).is_err() {
                return None;
            }
        } else {
            return None;
        }

        let mut reader = Reader::from_str(&xml);
        reader.trim_text(true);
        let mut buf = Vec::new();

        // Search for p:bg
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    if e.name().as_ref() == b"p:bg" {
                        // Found background, parse it
                        // Note: parse_background expects valid relationships for THIS file
                        // The `rels` passed in must be for `entry_name`.
                        return crate::office::shapes::parse_background(
                            &mut reader,
                            &mut Vec::new(),
                            rels,
                            Dimensions::new(0.0, 0.0), // Dims don't matter for bg extraction usually
                            theme,
                        );
                    } else if e.name().as_ref() == b"p:spTree" {
                        // Background must be before spTree in valid PPTX?
                        // Actually p:bg is usually first child of p:cSld
                    }
                }
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }
        None
    }

    /// Helper to parse content (placeholders and static shapes) from an XML file (Master or Layout)
    fn extract_master_layout_content<S: std::hash::BuildHasher>(
        archive: &mut ZipArchive<Cursor<&[u8]>>,
        entry_name: &str,
        rels: &HashMap<String, String, S>,
        theme: Option<&Theme>,
    ) -> (HashMap<u32, ContentBlock>, Vec<ContentBlock>) {
        let mut placeholders = HashMap::new();
        let mut static_content = Vec::new();

        // Read file
        let mut xml = String::new();
        let clean_name = entry_name.replace('\\', "/");
        if let Ok(mut file) = archive.by_name(&clean_name) {
            if file.read_to_string(&mut xml).is_err() {
                return (placeholders, static_content);
            }
        } else {
            return (placeholders, static_content);
        }

        let mut reader = Reader::from_str(&xml);
        reader.trim_text(true);
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => match e.name().as_ref() {
                    b"p:sp" => {
                        if let Some((block, idx_opt)) = crate::office::shapes::parse_shape(
                            &mut reader,
                            &mut Vec::new(),
                            rels,
                            theme,
                            None,
                        ) {
                            if let Some(idx) = idx_opt {
                                placeholders.insert(idx, block);
                            } else {
                                static_content.push(block);
                            }
                        }
                    }
                    b"p:pic" => {
                        if let Some(block) =
                            crate::office::shapes::parse_picture(&mut reader, &mut Vec::new(), rels)
                        {
                            static_content.push(block);
                        }
                    }
                    b"p:grpSp" => {
                        if let Some(blocks) = crate::office::shapes::parse_group_shape(
                            &mut reader,
                            &mut Vec::new(),
                            rels,
                            theme,
                        ) {
                            static_content.extend(blocks);
                        }
                    }
                    b"p:graphicFrame" => {
                        if let Some(block) = crate::office::shapes::parse_graphic_frame(
                            &mut reader,
                            &mut Vec::new(),
                            theme,
                        ) {
                            static_content.push(block);
                        }
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }
        (placeholders, static_content)
    }

    /// Text styles extracted from slide master (p:txStyles)
    fn extract_text_styles_from_master(
        archive: &mut ZipArchive<Cursor<&[u8]>>,
        master_path: &str,
    ) -> MasterTextStyles {
        let mut styles = MasterTextStyles::default();

        let mut xml = String::new();
        let clean_path = master_path.replace('\\', "/");
        if let Ok(mut file) = archive.by_name(&clean_path) {
            if file.read_to_string(&mut xml).is_err() {
                return styles;
            }
        } else {
            return styles;
        }

        let mut reader = Reader::from_str(&xml);
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut in_title_style = false;
        let mut in_body_style = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = e.name();
                    match name.as_ref() {
                        b"p:titleStyle" => in_title_style = true,
                        b"p:bodyStyle" => in_body_style = true,
                        b"a:defRPr" => {
                            // Extract font size from sz attribute (in hundredths of a point)
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"sz" {
                                    if let Ok(sz) = utils::attr_value(&attr.value).parse::<f64>() {
                                        let font_size = sz / 100.0;
                                        if in_title_style && styles.title_font_size.is_none() {
                                            styles.title_font_size = Some(font_size);
                                        }
                                        if in_body_style && styles.body_font_size.is_none() {
                                            styles.body_font_size = Some(font_size);
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let name = e.name();
                    let name_ref = name.as_ref();
                    // Look for a:srgbClr or a:schemeClr inside a:solidFill within defRPr
                    if name_ref == b"a:srgbClr" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"val" {
                                let color = format!("#{}", utils::attr_value(&attr.value));
                                if in_title_style && styles.title_color.is_none() {
                                    styles.title_color = Some(color.clone());
                                }
                                if in_body_style && styles.body_color.is_none() {
                                    styles.body_color = Some(color);
                                }
                            }
                        }
                    } else if name_ref == b"a:defRPr" {
                        // Extract font size from sz attribute (in hundredths of a point)
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"sz" {
                                if let Ok(sz) = utils::attr_value(&attr.value).parse::<f64>() {
                                    let font_size = sz / 100.0; // Convert from hundredths to points
                                    if in_title_style && styles.title_font_size.is_none() {
                                        styles.title_font_size = Some(font_size);
                                    }
                                    if in_body_style && styles.body_font_size.is_none() {
                                        styles.body_font_size = Some(font_size);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    match name.as_ref() {
                        b"p:titleStyle" => in_title_style = false,
                        b"p:bodyStyle" => in_body_style = false,
                        b"p:txStyles" => break, // Done with text styles
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }
        styles
    }
}

/// Default text styles from slide master
#[derive(Debug, Clone, Default)]
pub struct MasterTextStyles {
    /// Default title text color (from p:titleStyle)
    pub title_color: Option<String>,
    /// Default body text color (from p:bodyStyle)
    pub body_color: Option<String>,
    /// Default title font size in points (from p:titleStyle)
    pub title_font_size: Option<f64>,
    /// Default body font size in points (from p:bodyStyle)
    pub body_font_size: Option<f64>,
}

impl Default for PptxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PptxParser {
    fn format(&self) -> Format {
        self.format.clone()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        PptxParser::is_pptx_zip(data)
    }

    /// Parse a `PPTX` document
    ///
    /// # Errors
    ///
    /// Returns an error if the ZIP archive cannot be opened or if required XML files are missing or malformed.
    #[allow(clippy::too_many_lines)]
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing PPTX file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Open PPTX as ZIP archive
        let cursor = Cursor::new(data.as_ref());
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open PPTX as ZIP: {e}")))?;

        // 1. Read relationships to find slide filenames
        let mut rels_map: HashMap<String, String> = HashMap::new();
        if let Ok(mut rels_file) = archive.by_name("ppt/_rels/presentation.xml.rels") {
            let mut xml = String::new();
            rels_file
                .read_to_string(&mut xml)
                .map_err(|e| Error::ParseError(format!("Failed to read relationship XML: {e}")))?;

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
                presentation_file.read_to_string(&mut xml).map_err(|e| {
                    Error::ParseError(format!("Failed to read presentation.xml: {e}"))
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
        // Find relationship of type theme
        let mut theme_name = None;
        let mut major_font = None;
        let mut minor_font = None;
        let mut theme: Option<Theme> = None;

        // Let's refactor the previous block to keep `rels` available.
        let mut rid_to_target: HashMap<String, String> = HashMap::new();
        let mut theme_target = None;

        if let Ok(mut rels_file) = archive.by_name("ppt/_rels/presentation.xml.rels") {
            let mut xml = String::new();
            rels_file
                .read_to_string(&mut xml)
                .map_err(|e| Error::ParseError(format!("Failed to read relationship XML: {e}")))?;
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
            let entry_name = format!("ppt/{target}");
            let clean_name = entry_name.replace('\\', "/");
            if let Ok(mut theme_file) = archive.by_name(&clean_name) {
                let mut theme_xml = Vec::new();
                if theme_file.read_to_end(&mut theme_xml).is_ok() {
                    if let Ok(parsed_theme) = crate::office::theme::parse_theme(&theme_xml) {
                        debug!("Parsed theme: {}", parsed_theme.name);
                        theme_name = Some(parsed_theme.name.clone());
                        major_font.clone_from(&parsed_theme.major_font);
                        minor_font.clone_from(&parsed_theme.minor_font);
                        theme = Some(parsed_theme);
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
                let entry_name = format!("ppt/{target}");
                // Handle cases where target might already start with / or be relative
                // Usually it acts as "ppt/slides/slide1.xml" if target is "slides/slide1.xml"

                let mut slide_xml = String::new();
                // Try searching for the file in the archive
                // Standardize path separators
                let clean_name = entry_name.replace('\\', "/");

                if let Ok(mut file) = archive.by_name(&clean_name) {
                    file.read_to_string(&mut slide_xml).map_err(|e| {
                        Error::ParseError(format!("Failed to read slide XML {clean_name}: {e}"))
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
                        let rels_path = format!("{dir}/_rels/{filename}.rels");

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
                                        let path_for_ext = std::path::Path::new(&clean_path);
                                        let mime_type = if path_for_ext
                                            .extension()
                                            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
                                        {
                                            "image/png"
                                        } else if path_for_ext.extension().is_some_and(|ext| {
                                            ext.eq_ignore_ascii_case("jpg")
                                                || ext.eq_ignore_ascii_case("jpeg")
                                        }) {
                                            "image/jpeg"
                                        } else if path_for_ext
                                            .extension()
                                            .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"))
                                        {
                                            "image/gif"
                                        } else if path_for_ext
                                            .extension()
                                            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
                                        {
                                            "image/svg+xml"
                                        } else if path_for_ext
                                            .extension()
                                            .is_some_and(|ext| ext.eq_ignore_ascii_case("emf"))
                                        {
                                            "image/emf"
                                        } else if path_for_ext
                                            .extension()
                                            .is_some_and(|ext| ext.eq_ignore_ascii_case("wmf"))
                                        {
                                            "image/wmf"
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

                    // Resolve Layout and Master early for Placeholders
                    let mut placeholder_map = HashMap::new();
                    let mut slide_static_content = Vec::new();
                    // We'll also store these for background usage to avoid re-resolving
                    let mut layout_info: Option<(String, HashMap<String, String>)> = None;
                    let mut master_info: Option<(String, HashMap<String, String>)> = None;
                    let mut master_text_styles_for_slide: Option<MasterTextStyles> = None;

                    if let Some(layout_target) =
                        slide_rels.values().find(|t| t.contains("slideLayout"))
                    {
                        let layout_entry = if let Some(stripped) = layout_target.strip_prefix("../")
                        {
                            format!("ppt/{stripped}")
                        } else {
                            format!("ppt/slides/{layout_target}")
                        };

                        let layout_rels =
                            Self::get_relationships_for_file(&mut archive, &layout_entry);

                        // Master first (base)
                        if let Some(master_target) =
                            layout_rels.values().find(|t| t.contains("slideMaster"))
                        {
                            let master_entry =
                                if let Some(stripped) = master_target.strip_prefix("../") {
                                    format!("ppt/{stripped}")
                                } else {
                                    format!("ppt/slideLayouts/{master_target}")
                                };

                            let master_rels =
                                Self::get_relationships_for_file(&mut archive, &master_entry);

                            // Extract master content
                            let (master_phs, master_static) = Self::extract_master_layout_content(
                                &mut archive,
                                &master_entry,
                                &master_rels,
                                theme.as_ref(),
                            );
                            placeholder_map.extend(master_phs);
                            slide_static_content.extend(master_static);

                            master_info = Some((master_entry.clone(), master_rels));

                            // Extract text styles (title/body default colors) from master
                            // Store for application AFTER layout merge
                            master_text_styles_for_slide = Some(
                                Self::extract_text_styles_from_master(&mut archive, &master_entry),
                            );
                        }

                        // Layout second (override with merge)
                        let (layout_phs, layout_static) = Self::extract_master_layout_content(
                            &mut archive,
                            &layout_entry,
                            &layout_rels,
                            theme.as_ref(),
                        );
                        slide_static_content.extend(layout_static);

                        for (idx, mut layout_ph) in layout_phs {
                            if let Some(master_ph) = placeholder_map.get(&idx) {
                                // Merge properties: Layout inherits from Master if Layout lacks props
                                if let ContentBlock::Text(ref mut l_text) = layout_ph {
                                    if let ContentBlock::Text(ref m_text) = master_ph {
                                        // Inherit styling if missing in layout
                                        if l_text.style.fill_color.is_none() {
                                            l_text
                                                .style
                                                .fill_color
                                                .clone_from(&m_text.style.fill_color);
                                        }
                                        if l_text.style.stroke_color.is_none() {
                                            l_text
                                                .style
                                                .stroke_color
                                                .clone_from(&m_text.style.stroke_color);
                                        }
                                        if l_text.style.stroke_width.is_none() {
                                            l_text.style.stroke_width = m_text.style.stroke_width;
                                        }
                                        // Inherit missing bounds?
                                        // Usually layout defines position. But if layout has 0 bounds (placeholder)?
                                        // Pptx bounds check is specific.
                                        // Let's rely on style merging primarily for now.

                                        // Also inherit default text styling if we tracked it?
                                        // Text runs style inheritance happens in parse_text_body / parse_shape
                                        // But here we are merging the *containers*.
                                    }
                                }
                            }
                            placeholder_map.insert(idx, layout_ph);
                        }

                        layout_info = Some((layout_entry, layout_rels));
                    }

                    // Apply master text styles to placeholders AFTER layout merge
                    // This ensures layout placeholders also get the font_color
                    if let Some(ref mts) = master_text_styles_for_slide {
                        if let Some(ref title_color) = mts.title_color {
                            if let Some(ContentBlock::Text(ref mut t)) = placeholder_map.get_mut(&0)
                            {
                                if t.style.font_color.is_none() {
                                    t.style.font_color = Some(title_color.clone());
                                }
                            }
                        }
                        if let Some(title_size) = mts.title_font_size {
                            if let Some(ContentBlock::Text(ref mut t)) = placeholder_map.get_mut(&0)
                            {
                                if t.style.font_size.is_none() {
                                    t.style.font_size = Some(title_size);
                                }
                            }
                        }
                        if let Some(ref body_color) = mts.body_color {
                            if let Some(ContentBlock::Text(ref mut t)) = placeholder_map.get_mut(&1)
                            {
                                if t.style.font_color.is_none() {
                                    t.style.font_color = Some(body_color.clone());
                                }
                            }
                        }
                        if let Some(body_size) = mts.body_font_size {
                            if let Some(ContentBlock::Text(ref mut t)) = placeholder_map.get_mut(&1)
                            {
                                if t.style.font_size.is_none() {
                                    t.style.font_size = Some(body_size);
                                }
                            }
                        }
                    }

                    #[allow(clippy::cast_possible_truncation)]
                    let mut page = SlideParser::parse(
                        &slide_xml,
                        (i + 1) as u32,
                        &slide_rels,
                        dimensions,
                        theme.as_ref(),
                        Some(&placeholder_map),
                    );

                    // Check if page has background (index 0, z-index -1)
                    let has_bg = if let Some(first) = page.content.first() {
                        match first {
                            ContentBlock::Text(t) => t.style.z_index == Some(-1),
                            ContentBlock::Image(i) => i.style.z_index == Some(-1),
                            // Container/Table/Vector usually don't have bg z-index unless specific
                            _ => false,
                        }
                    } else {
                        false
                    };

                    // Insert static content from Master/Layout (background layers)
                    // If slide has its own background, insert after it.
                    let bg_offset = usize::from(has_bg);
                    page.content
                        .splice(bg_offset..bg_offset, slide_static_content);

                    if !has_bg {
                        // Use pre-resolved layout info
                        if let Some((layout_entry, layout_rels)) = layout_info {
                            if let Some(bg) = Self::extract_background_from_xml(
                                &mut archive,
                                &layout_entry,
                                &layout_rels,
                                theme.as_ref(),
                            ) {
                                page.content.insert(0, bg);
                            } else if let Some((master_entry, master_rels)) = master_info {
                                if let Some(bg) = Self::extract_background_from_xml(
                                    &mut archive,
                                    &master_entry,
                                    &master_rels,
                                    theme.as_ref(),
                                ) {
                                    page.content.insert(0, bg);
                                }
                            }
                        }
                    }
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
        #[allow(clippy::cast_possible_wrap)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::FileOptions;

    #[tokio::test]
    async fn test_pptx_master_background() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = FileOptions::default().compression_method(zip::CompressionMethod::Stored);

            // 1. presentation.xml
            zip.start_file("ppt/presentation.xml", options).unwrap();
            zip.write_all(br#"
                <p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
                    <p:sldIdLst>
                        <p:sldId r:id="rId2"/>
                    </p:sldIdLst>
                    <p:sldSz cx="9144000" cy="6858000"/>
                </p:presentation>
            "#).unwrap();

            // 2. presentation.xml.rels
            zip.start_file("ppt/_rels/presentation.xml.rels", options)
                .unwrap();
            zip.write_all(br#"
                <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                    <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
                </Relationships>
            "#).unwrap();

            // 3. slide1.xml (No bg)
            zip.start_file("ppt/slides/slide1.xml", options).unwrap();
            zip.write_all(br#"
                <p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
                    <p:cSld>
                        <p:spTree>
                             <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
                             <p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>
                        </p:spTree>
                    </p:cSld>
                </p:sld>
            "#).unwrap();

            // 4. slide1.xml.rels (points to layout)
            zip.start_file("ppt/slides/_rels/slide1.xml.rels", options)
                .unwrap();
            zip.write_all(br#"
                <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                    <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>
                </Relationships>
            "#).unwrap();

            // 5. slideLayout1.xml (No bg)
            zip.start_file("ppt/slideLayouts/slideLayout1.xml", options)
                .unwrap();
            zip.write_all(
                br#"
                <p:sldLayout xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
                     <p:cSld/>
                </p:sldLayout>
            "#,
            )
            .unwrap();

            // 6. slideLayout1.xml.rels (points to master)
            zip.start_file("ppt/slideLayouts/_rels/slideLayout1.xml.rels", options)
                .unwrap();
            zip.write_all(br#"
                <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                    <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/>
                </Relationships>
            "#).unwrap();

            // 7. slideMaster1.xml (HAS RED BG)
            zip.start_file("ppt/slideMasters/slideMaster1.xml", options)
                .unwrap();
            zip.write_all(br#"
                <p:sldMaster xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
                    <p:cSld>
                        <p:bg>
                            <p:bgPr>
                                <a:solidFill>
                                    <a:srgbClr val="FF0000"/>
                                </a:solidFill>
                            </p:bgPr>
                        </p:bg>
                    </p:cSld>
                </p:sldMaster>
            "#).unwrap();

            zip.finish().unwrap();
        }

        // Test parser
        let parser = PptxParser::new();
        let context = ParseContext {
            filename: Some("test.pptx".to_string()),
            format: Format::pptx(),
            size: buf.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        let doc = parser.parse(Bytes::from(buf), context).await.unwrap();

        assert_eq!(doc.pages.len(), 1);
        let page = &doc.pages[0];

        // Output content types for debugging if it fails
        for block in &page.content {
            println!("Block: {block:?}");
        }

        // Check content for background
        assert!(
            !page.content.is_empty(),
            "Page content is empty, background not inserted"
        );

        // Background should be first
        if let ContentBlock::Text(block) = &page.content[0] {
            assert_eq!(block.style.fill_color, Some("#FF0000".to_string()));
            assert_eq!(block.style.z_index, Some(-1));
        } else {
            panic!("Expected background block, got {:?}", page.content[0]);
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_pptx_placeholders() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = FileOptions::default().compression_method(zip::CompressionMethod::Stored);

            // 1. presentation.xml
            zip.start_file("ppt/presentation.xml", options).unwrap();
            zip.write_all(br#"
                <p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
                    <p:sldIdLst>
                        <p:sldId r:id="rId2"/>
                    </p:sldIdLst>
                    <p:sldSz cx="9144000" cy="6858000"/>
                </p:presentation>
            "#).unwrap();

            // 2. presentation.xml.rels
            zip.start_file("ppt/_rels/presentation.xml.rels", options)
                .unwrap();
            zip.write_all(br#"
                <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                    <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
                </Relationships>
            "#).unwrap();

            // 3. slide1.xml (References Layout placeholder idx=1)
            // Shape here has NO geometry/style, only idx="1"
            zip.start_file("ppt/slides/slide1.xml", options).unwrap();
            zip.write_all(br#"
                <p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
                    <p:cSld>
                        <p:spTree>
                             <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
                             <p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>
                             
                             <p:sp>
                                <p:nvSpPr>
                                    <p:cNvPr id="2" name="Placeholder"/>
                                    <p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>
                                    <p:nvPr>
                                        <p:ph type="body" idx="1"/>
                                    </p:nvPr>
                                </p:nvSpPr>
                                <p:spPr/>
                             </p:sp>
                        </p:spTree>
                    </p:cSld>
                </p:sld>
            "#).unwrap();

            // 4. slide1.xml.rels (points to layout)
            zip.start_file("ppt/slides/_rels/slide1.xml.rels", options)
                .unwrap();
            zip.write_all(br#"
                <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                    <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>
                </Relationships>
            "#).unwrap();

            // 5. slideLayout1.xml (Defines placeholder idx=1 with RED fill and specific bounds)
            zip.start_file("ppt/slideLayouts/slideLayout1.xml", options)
                .unwrap();
            zip.write_all(br#"
                <p:sldLayout xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
                     <p:cSld>
                        <p:spTree>
                            <p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>
                            <p:sp>
                                <p:nvSpPr>
                                    <p:cNvPr id="2" name="Layout Placeholder"/>
                                    <p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>
                                    <p:nvPr>
                                        <p:ph type="body" idx="1"/>
                                    </p:nvPr>
                                </p:nvSpPr>
                                <p:spPr>
                                    <a:xfrm>
                                        <a:off x="1000000" y="1000000"/>
                                        <a:ext cx="2000000" cy="2000000"/>
                                    </a:xfrm>
                                    <a:solidFill>
                                        <a:srgbClr val="FF0000"/>
                                    </a:solidFill>
                                </p:spPr>
                                <p:txBody>
                                    <a:bodyPr/>
                                    <a:lstStyle/>
                                    <a:p><a:r><a:t>Placeholder Text</a:t></a:r></a:p>
                                </p:txBody>
                            </p:sp>
                        </p:spTree>
                     </p:cSld>
                </p:sldLayout>
            "#).unwrap();

            // 6. slideLayout1.xml.rels (points to master, effectively empty here as we test layout inheritance)
            zip.start_file("ppt/slideLayouts/_rels/slideLayout1.xml.rels", options)
                .unwrap();
            zip.write_all(br#"
                <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                    <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/>
                </Relationships>
            "#).unwrap();

            // 7. slideMaster1.xml (Empty logic for this test)
            zip.start_file("ppt/slideMasters/slideMaster1.xml", options)
                .unwrap();
            zip.write_all(
                br#"
                <p:sldMaster xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
                    <p:cSld/>
                </p:sldMaster>
            "#,
            )
            .unwrap();

            zip.finish().unwrap();
        }

        // Test parser
        let parser = PptxParser::new();
        let context = ParseContext {
            filename: Some("test_ph.pptx".to_string()),
            format: Format::pptx(),
            size: buf.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        let doc = parser.parse(Bytes::from(buf), context).await.unwrap();

        assert_eq!(doc.pages.len(), 1);
        let page = &doc.pages[0];

        // Find the shape. Should be the second block (after empty group shape if any, or first if group shape ignored correctly)
        // Wait, parse_shape adds to content.

        let mut found_ph = false;
        for block in &page.content {
            if let ContentBlock::Text(text) = block {
                // Check if it inherited RED color
                if text.style.fill_color == Some("#FF0000".to_string()) {
                    found_ph = true;
                    // Check bounds - transformed from EMUs (1M/2M) to points (div 12700)
                    // 1,000,000 / 12700 = 78.74
                    // 2,000,000 / 12700 = 157.48
                    assert!(text.bounds.x > 78.0 && text.bounds.x < 79.0);
                    assert!(text.bounds.width > 157.0 && text.bounds.width < 158.0);
                }
            }
        }
        assert!(
            found_ph,
            "Did not find shape with inherited placeholder properties"
        );
    }
}
