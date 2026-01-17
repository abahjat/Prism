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

        // 4. Parse Pages (extract Glyphs and Images, generate HTML)
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

            let page_base = std::path::Path::new(&page_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            let mut reader = Reader::from_str(&page_content);
            reader.trim_text(true);

            // Collect positioned elements for sorting
            #[allow(clippy::items_after_statements)]
            struct TextElement {
                x: f64,
                y: f64,
                text: String,
            }
            #[allow(clippy::items_after_statements)]
            struct ImageElement {
                y: f64,
                width: f64,
                data_uri: String,
            }

            let mut text_elements: Vec<TextElement> = Vec::new();
            let mut image_elements: Vec<ImageElement> = Vec::new();
            let mut width = 816.0;
            let mut height = 1056.0;

            // Track current path context for ImageBrush
            let mut current_path_bounds: Option<(f64, f64, f64, f64)> = None; // x, y, w, h

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
                            b"Path" => {
                                // Parse path bounds from RenderTransform or Data
                                // For simplicity, try to extract from Clip or Canvas.RenderTransform
                                // Often paths with images have explicit bounds
                                let render_transform = Self::get_attr(&e, b"RenderTransform");
                                if let Some(rt) = render_transform {
                                    // Parse matrix: "M11,M12,M21,M22,OffsetX,OffsetY"
                                    let parts: Vec<&str> = rt.split(',').collect();
                                    if parts.len() >= 6 {
                                        let offset_x =
                                            parts[4].trim().parse::<f64>().unwrap_or(0.0);
                                        let offset_y =
                                            parts[5].trim().parse::<f64>().unwrap_or(0.0);
                                        current_path_bounds =
                                            Some((offset_x, offset_y, 100.0, 100.0));
                                    }
                                }
                            }
                            b"ImageBrush" => {
                                if let Some(img_src) = Self::get_attr(&e, b"ImageSource") {
                                    debug!("Found ImageBrush with ImageSource: {}", img_src);

                                    // Try multiple path resolutions
                                    let paths_to_try = vec![
                                        // Try as-is (absolute in package)
                                        img_src.trim_start_matches('/').to_string(),
                                        // Try relative to page
                                        if page_base.is_empty() {
                                            img_src.clone()
                                        } else {
                                            format!(
                                                "{page_base}/{}",
                                                img_src.trim_start_matches('/')
                                            )
                                        },
                                        // Try in Resources folder relative to page
                                        format!(
                                            "{page_base}/Resources/{}",
                                            std::path::Path::new(&img_src)
                                                .file_name()
                                                .map(|s| s.to_string_lossy().to_string())
                                                .unwrap_or(img_src.clone())
                                        ),
                                        // Try direct path
                                        img_src.clone(),
                                    ];

                                    let mut img_data = Vec::new();
                                    let mut found_path = String::new();

                                    for try_path in &paths_to_try {
                                        debug!("Trying image path: {}", try_path);
                                        if let Ok(mut file) = zip.by_name(try_path) {
                                            if std::io::Read::read_to_end(&mut file, &mut img_data)
                                                .is_ok()
                                                && !img_data.is_empty()
                                            {
                                                try_path.clone_into(&mut found_path);
                                                debug!(
                                                    "Successfully read image from: {}",
                                                    try_path
                                                );
                                                break;
                                            }
                                        }
                                    }

                                    if img_data.is_empty() {
                                        warn!(
                                            "Could not find image in ZIP for any path variant of: {}",
                                            img_src
                                        );
                                    } else {
                                        let path = std::path::Path::new(&found_path);
                                        let ext = path
                                            .extension()
                                            .and_then(|e| e.to_str())
                                            .unwrap_or("")
                                            .to_lowercase();

                                        // Convert non-web-compatible formats to PNG
                                        let (final_data, mime) = if ext == "tif"
                                            || ext == "tiff"
                                            || ext == "bmp"
                                        {
                                            // Convert to PNG using image crate
                                            match image::load_from_memory(&img_data) {
                                                Ok(img) => {
                                                    let mut png_data = Vec::new();
                                                    let mut cursor =
                                                        std::io::Cursor::new(&mut png_data);
                                                    if img
                                                        .write_to(
                                                            &mut cursor,
                                                            image::ImageFormat::Png,
                                                        )
                                                        .is_ok()
                                                    {
                                                        debug!(
                                                            "Converted {} to PNG ({} -> {} bytes)",
                                                            ext,
                                                            img_data.len(),
                                                            png_data.len()
                                                        );
                                                        (png_data, "image/png")
                                                    } else {
                                                        warn!(
                                                            "Failed to encode {} as PNG, skipping",
                                                            ext
                                                        );
                                                        continue;
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        "Failed to load {} image: {}, skipping",
                                                        ext, e
                                                    );
                                                    continue;
                                                }
                                            }
                                        } else {
                                            // Use original data for web-compatible formats
                                            let m = match ext.as_str() {
                                                "jpg" | "jpeg" => "image/jpeg",
                                                "gif" => "image/gif",
                                                "webp" => "image/webp",
                                                "svg" => "image/svg+xml",
                                                _ => "image/png",
                                            };
                                            (img_data.clone(), m)
                                        };

                                        let b64 = <base64::engine::general_purpose::GeneralPurpose as base64::Engine>::encode(&base64::engine::general_purpose::STANDARD, &final_data);

                                        let data_uri = format!("data:{mime};base64,{b64}");
                                        let (_x, y, w, _h) =
                                            current_path_bounds.unwrap_or((0.0, 0.0, 100.0, 100.0));

                                        // Parse Viewbox for dimensions if available
                                        let mut img_w = w;
                                        if let Some(vb) = Self::get_attr(&e, b"Viewbox") {
                                            let parts: Vec<&str> = vb.split(',').collect();
                                            if parts.len() >= 4 {
                                                img_w = parts[2].trim().parse::<f64>().unwrap_or(w);
                                            }
                                        }

                                        image_elements.push(ImageElement {
                                            y,
                                            width: img_w,
                                            data_uri,
                                        });
                                        debug!(
                                            "Added image element with {} bytes",
                                            final_data.len()
                                        );
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

                                    let bidi_level = Self::get_attr(&e, b"BidiLevel")
                                        .and_then(|v| v.parse::<u8>().ok())
                                        .unwrap_or(0);

                                    let final_text = if bidi_level % 2 != 0 {
                                        text.chars().rev().collect()
                                    } else {
                                        text
                                    };

                                    text_elements.push(TextElement {
                                        x: origin_x,
                                        y: origin_y,
                                        text: final_text,
                                    });
                                }
                            }
                            _ => (),
                        }
                    }
                    Ok(Event::End(e)) => {
                        if e.name().as_ref() == b"Path" {
                            current_path_bounds = None;
                        }
                    }
                    Ok(Event::Eof) | Err(_) => break,
                    _ => (),
                }
            }

            // Sort text elements by Y position (top to bottom), then X (left to right)
            text_elements.sort_by(|a, b| {
                let y_cmp = a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal);
                if y_cmp == std::cmp::Ordering::Equal {
                    a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    y_cmp
                }
            });

            // Sort images by Y position
            image_elements
                .sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));

            // Generate HTML
            let mut html =
                String::from("<!DOCTYPE html><html><head><meta charset=\"UTF-8\"><style>");
            html.push_str("body { font-family: sans-serif; padding: 20px; max-width: 800px; margin: 0 auto; }");
            html.push_str("img { max-width: 100%; height: auto; display: block; margin: 10px 0; }");
            html.push_str(".text-line { margin: 4px 0; }");
            html.push_str("</style></head><body>");

            // Add images first (usually at top)
            for img in &image_elements {
                use std::fmt::Write;
                let _ = write!(
                    html,
                    "<img src=\"{}\" alt=\"XPS Image\" style=\"max-width:{}px;\">",
                    img.data_uri,
                    img.width.min(600.0)
                );
            }

            // Group text elements by approximate Y position (within 5 units = same line)
            let mut current_line_y: Option<f64> = None;
            let mut current_line = String::new();
            let line_threshold = 5.0;

            for elem in &text_elements {
                if let Some(last_y) = current_line_y {
                    if (elem.y - last_y).abs() > line_threshold {
                        // New line
                        if !current_line.is_empty() {
                            use std::fmt::Write;
                            let _ = write!(
                                html,
                                "<div class=\"text-line\">{}</div>",
                                html_escape(&current_line)
                            );
                            current_line.clear();
                        }
                    } else {
                        // Same line, add space
                        current_line.push(' ');
                    }
                }
                current_line.push_str(&elem.text);
                current_line_y = Some(elem.y);
            }

            // Flush last line
            if !current_line.is_empty() {
                use std::fmt::Write;
                let _ = write!(
                    html,
                    "<div class=\"text-line\">{}</div>",
                    html_escape(&current_line)
                );
            }

            html.push_str("</body></html>");

            if !text_elements.is_empty() || !image_elements.is_empty() {
                let text_run = TextRun {
                    text: format!("__HTML_RAW__:{html}"),
                    style: TextStyle::default(),
                    bounds: Some(Rect::default()),
                    char_positions: Some(Vec::new()),
                };

                let text_block = TextBlock {
                    runs: vec![text_run],
                    bounds: Rect::new(0.0, 0.0, width, height),
                    paragraph_style: None,
                    vertical_alignment: None,
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

        // Helper function for HTML escaping
        #[allow(clippy::items_after_statements)]
        fn html_escape(s: &str) -> String {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
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
