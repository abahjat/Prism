// SPDX-License-Identifier: AGPL-3.0-only
//! `DXF` (`AutoCAD` Drawing Exchange Format) parser
//!
//! Parses DXF files and extracts text entities and drawing metadata.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, ShapeStyle, TextBlock,
        TextRun, TextStyle,
    },
    error::{Error, Result},
    format::{Format, FormatFamily},
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::{debug, info};

/// DXF format parser
#[derive(Debug, Clone)]
pub struct DxfParser;

impl DxfParser {
    /// Create a new DXF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data has DXF signature (text or binary format)
    #[must_use]
    fn is_dxf_file(data: &[u8]) -> bool {
        // Binary DXF: "AutoCAD Binary DXF\r\n\x1a\0"
        if data.starts_with(b"AutoCAD Binary DXF") {
            return true;
        }

        // Text DXF: starts with "0\n" or "0\r\n" or " 0\n" followed by SECTION
        // Check first 200 bytes for "SECTION" keyword
        let header = std::str::from_utf8(&data[..data.len().min(200)]).unwrap_or("");
        let header_upper = header.to_uppercase();
        header_upper.contains("SECTION") || header_upper.starts_with("999")
    }

    /// Extract text content and entities from DXF file
    fn extract_dxf_content(data: &[u8]) -> Result<(Vec<String>, DxfInfo)> {
        let cursor = Cursor::new(data);
        let drawing = dxf::Drawing::load(&mut cursor.clone())
            .map_err(|e| Error::ParseError(format!("Failed to parse DXF: {e}")))?;

        let mut texts = Vec::new();
        // Extract header version using the header's version property
        let mut info = DxfInfo {
            version: format!("{:?}", drawing.header.version),
            ..DxfInfo::default()
        };

        // Count and extract entities using entities() method
        for entity in drawing.entities() {
            info.entity_count += 1;

            match &entity.specific {
                dxf::entities::EntityType::Text(text) => {
                    if !text.value.trim().is_empty() {
                        texts.push(text.value.clone());
                    }
                    info.text_count += 1;
                }
                dxf::entities::EntityType::MText(mtext) => {
                    if !mtext.text.trim().is_empty() {
                        // MText can have formatting codes, try to clean them
                        let clean_text = clean_mtext(&mtext.text);
                        if !clean_text.is_empty() {
                            texts.push(clean_text);
                        }
                    }
                    info.text_count += 1;
                }
                dxf::entities::EntityType::Line(_) => info.line_count += 1,
                dxf::entities::EntityType::Circle(_) => info.circle_count += 1,
                dxf::entities::EntityType::Arc(_) => info.arc_count += 1,
                dxf::entities::EntityType::Polyline(_)
                | dxf::entities::EntityType::LwPolyline(_) => {
                    info.polyline_count += 1;
                }
                dxf::entities::EntityType::Insert(_) => info.block_count += 1,
                _ => {}
            }
        }

        // Extract layer names using layers() method
        for layer in drawing.layers() {
            info.layers.push(layer.name.clone());
        }

        Ok((texts, info))
    }
}

/// DXF drawing information
#[derive(Debug, Default)]
struct DxfInfo {
    version: String,
    entity_count: usize,
    text_count: usize,
    line_count: usize,
    circle_count: usize,
    arc_count: usize,
    polyline_count: usize,
    block_count: usize,
    dimension_count: usize,
    layers: Vec<String>,
}

/// Clean `MText` formatting codes
fn clean_mtext(text: &str) -> String {
    // `MText` can contain formatting like {\fArial|b1|i0;text}
    // This is a simplified cleaner
    let mut result = String::new();
    let mut in_format = false;
    let mut brace_depth = 0;

    for ch in text.chars() {
        match ch {
            '{' => {
                brace_depth += 1;
                in_format = true;
            }
            '}' => {
                brace_depth -= 1;
                if brace_depth == 0 {
                    in_format = false;
                }
            }
            ';' if in_format => {
                in_format = false;
            }
            '\\' => {
                // Skip escape sequences like \P (paragraph), \f (font)
            }
            _ if !in_format => {
                result.push(ch);
            }
            _ => {}
        }
    }

    result.trim().to_string()
}

impl Default for DxfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for DxfParser {
    fn format(&self) -> Format {
        Format {
            mime_type: "image/vnd.dxf".to_string(),
            extension: "dxf".to_string(),
            family: FormatFamily::Cad,
            name: "AutoCAD DXF".to_string(),
            is_container: false,
        }
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_dxf_file(data)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "DXF Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing DXF file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let (texts, info) = Self::extract_dxf_content(&data)?;

        info!(
            "DXF parsed: {} entities, {} texts, {} layers",
            info.entity_count,
            info.text_count,
            info.layers.len()
        );

        // Build document content as HTML
        let html = format!(
            r#"__HTML_RAW__:<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<style>
body {{ font-family: system-ui, sans-serif; padding: 20px; background: #1a1a2e; color: #eee; }}
.header {{ background: linear-gradient(135deg, #16213e, #0f3460); padding: 20px; border-radius: 8px; margin-bottom: 20px; }}
.stats {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 15px; margin-bottom: 20px; }}
.stat {{ background: #16213e; padding: 15px; border-radius: 8px; text-align: center; }}
.stat-value {{ font-size: 24px; font-weight: bold; color: #e94560; }}
.stat-label {{ color: #94a3b8; font-size: 12px; text-transform: uppercase; }}
.section {{ background: #16213e; padding: 15px; border-radius: 8px; margin-bottom: 15px; }}
.section h3 {{ color: #e94560; margin-top: 0; }}
.layers {{ display: flex; flex-wrap: wrap; gap: 8px; }}
.layer {{ background: #0f3460; padding: 4px 10px; border-radius: 4px; font-size: 12px; }}
.texts {{ max-height: 400px; overflow-y: auto; }}
.text-item {{ background: #0f3460; padding: 8px 12px; margin-bottom: 8px; border-radius: 4px; border-left: 3px solid #e94560; }}
</style>
</head>
<body>
<div class="header">
<h1>📐 AutoCAD DXF Drawing</h1>
<p>Version: {version}</p>
</div>
<div class="stats">
<div class="stat"><div class="stat-value">{entities}</div><div class="stat-label">Total Entities</div></div>
<div class="stat"><div class="stat-value">{lines}</div><div class="stat-label">Lines</div></div>
<div class="stat"><div class="stat-value">{circles}</div><div class="stat-label">Circles/Arcs</div></div>
<div class="stat"><div class="stat-value">{polylines}</div><div class="stat-label">Polylines</div></div>
<div class="stat"><div class="stat-value">{blocks}</div><div class="stat-label">Blocks</div></div>
<div class="stat"><div class="stat-value">{dimensions}</div><div class="stat-label">Dimensions</div></div>
<div class="stat"><div class="stat-value">{text_count}</div><div class="stat-label">Text Labels</div></div>
</div>
{layers_section}
{texts_section}
</body></html>"#,
            version = if info.version.is_empty() {
                "Unknown".to_string()
            } else {
                info.version.clone()
            },
            entities = info.entity_count,
            lines = info.line_count,
            circles = info.circle_count + info.arc_count,
            polylines = info.polyline_count,
            blocks = info.block_count,
            dimensions = info.dimension_count,
            text_count = info.text_count,
            layers_section = if info.layers.is_empty() {
                String::new()
            } else {
                use std::fmt::Write;
                let mut s =
                    String::from(r#"<div class="section"><h3>📁 Layers</h3><div class="layers">"#);
                for layer in &info.layers {
                    let _ = write!(s, r#"<span class="layer">{}</span>"#, html_escape(layer));
                }
                s.push_str("</div></div>");
                s
            },
            texts_section = if texts.is_empty() {
                String::new()
            } else {
                use std::fmt::Write;
                let mut s = String::from(
                    r#"<div class="section"><h3>📝 Text Content</h3><div class="texts">"#,
                );
                for text in &texts {
                    let _ = write!(s, r#"<div class="text-item">{}</div>"#, html_escape(text));
                }
                s.push_str("</div></div>");
                s
            },
        );

        let text_run = TextRun {
            text: html,
            style: TextStyle::default(),
            bounds: None,
            char_positions: None,
        };

        let text_block = TextBlock {
            runs: vec![text_run],
            paragraph_style: None,
            bounds: Rect::default(),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: vec![ContentBlock::Text(text_block)],
            annotations: Vec::new(),
            metadata: PageMetadata::default(),
        };

        let mut document = Document::new();
        document.pages.push(page);

        info!("Successfully parsed DXF");
        Ok(document)
    }
}

/// HTML escape function
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
