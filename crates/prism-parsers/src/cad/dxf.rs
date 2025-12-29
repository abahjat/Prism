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

    /// Extract text content and entities from DXF file, and render SVG
    fn extract_dxf_content(data: &[u8]) -> Result<(Vec<String>, DxfInfo, String)> {
        let cursor = Cursor::new(data);
        let drawing = dxf::Drawing::load(&mut cursor.clone())
            .map_err(|e| Error::ParseError(format!("Failed to parse DXF: {e}")))?;

        let mut texts = Vec::new();
        // Extract header version using the header's version property
        let mut info = DxfInfo {
            version: format!("{:?}", drawing.header.version),
            ..DxfInfo::default()
        };

        // Count entities for stats
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

        // Extract layer names
        for layer in drawing.layers() {
            info.layers.push(layer.name.clone());
        }

        // Render SVG
        let svg = render_to_svg(&drawing);

        Ok((texts, info, svg))
    }
}

/// Calculate bounds and render SVG
#[allow(clippy::too_many_lines)]
fn render_to_svg(drawing: &dxf::Drawing) -> String {
    use std::fmt::Write;
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    let mut has_ents = false;

    // Helper to update bounds
    let mut update_bounds = |x: f64, y: f64| {
        if x < min_x {
            min_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if x > max_x {
            max_x = x;
        }
        if y > max_y {
            max_y = y;
        }
        has_ents = true;
    };

    // 1. Calculate Bounds
    for entity in drawing.entities() {
        match &entity.specific {
            dxf::entities::EntityType::Line(line) => {
                update_bounds(line.p1.x, line.p1.y);
                update_bounds(line.p2.x, line.p2.y);
            }
            dxf::entities::EntityType::Circle(circle) => {
                let r = circle.radius;
                update_bounds(circle.center.x - r, circle.center.y - r);
                update_bounds(circle.center.x + r, circle.center.y + r);
            }
            dxf::entities::EntityType::Arc(arc) => {
                let r = arc.radius;
                update_bounds(arc.center.x - r, arc.center.y - r);
                update_bounds(arc.center.x + r, arc.center.y + r);
            }
            dxf::entities::EntityType::LwPolyline(poly) => {
                for v in &poly.vertices {
                    update_bounds(v.x, v.y);
                }
            }
            _ => {}
        }
    }

    if !has_ents {
        return String::from("<svg viewBox='0 0 100 100' xmlns='http://www.w3.org/2000/svg'><text x='10' y='50' fill='#94a3b8' font-family='system-ui'>Empty Drawing</text></svg>");
    }

    // Add padding (5%)
    let width = max_x - min_x;
    let height = max_y - min_y;
    let margin_x = if width > 0.0 { width * 0.05 } else { 10.0 };
    let margin_y = if height > 0.0 { height * 0.05 } else { 10.0 };

    let view_min_x = min_x - margin_x;
    let view_max_x = max_x + margin_x;
    let view_min_y = min_y - margin_y;
    let view_max_y = max_y + margin_y;

    let svg_width = view_max_x - view_min_x;
    let svg_height = view_max_y - view_min_y;

    let mut svg = String::with_capacity(4096);
    let _ = write!(
        svg,
        r#"<svg viewBox="0 0 {svg_width} {svg_height}" xmlns="http://www.w3.org/2000/svg" style="background:#222; width:100%; height:auto; border-radius:4px; aspect-ratio: {svg_width} / {svg_height};">"#
    );

    // Add a group to apply styles
    // Use scaling for stroke-width to ensure lines are visible regardless of zoom
    let stroke_width = (svg_width.max(svg_height)) * 0.002; // 0.2% of max dimension
    let _ = write!(
        svg,
        r##"<g stroke="#e94560" stroke-width="{stroke_width}" fill="none" stroke-linecap="round" stroke-linejoin="round">"##
    );

    for entity in drawing.entities() {
        match &entity.specific {
            dxf::entities::EntityType::Line(line) => {
                let x1 = line.p1.x - view_min_x;
                let y1 = view_max_y - line.p1.y;
                let x2 = line.p2.x - view_min_x;
                let y2 = view_max_y - line.p2.y;
                let _ = write!(svg, r#"<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" />"#);
            }
            dxf::entities::EntityType::Circle(circle) => {
                let cx = circle.center.x - view_min_x;
                let cy = view_max_y - circle.center.y;
                let r = circle.radius;
                let _ = write!(svg, r#"<circle cx="{cx}" cy="{cy}" r="{r}" />"#);
            }
            dxf::entities::EntityType::Arc(arc) => {
                let cx = arc.center.x - view_min_x;
                let cy = view_max_y - arc.center.y;
                let r = arc.radius;
                let start_angle = arc.start_angle;
                let end_angle = arc.end_angle;

                let start_rad = start_angle.to_radians();
                let end_rad = end_angle.to_radians();

                let start_x = cx + r * start_rad.cos();
                let start_y = cy - r * start_rad.sin();
                let end_x = cx + r * end_rad.cos();
                let end_y = cy - r * end_rad.sin();

                let mut span = end_angle - start_angle;
                if span < 0.0 {
                    span += 360.0;
                }
                let large_arc = i32::from(span > 180.0);

                let _ = write!(
                    svg,
                    r#"<path d="M {start_x} {start_y} A {r} {r} 0 {large_arc} 0 {end_x} {end_y}" />"#
                );
            }
            dxf::entities::EntityType::LwPolyline(poly) => {
                if !poly.vertices.is_empty() {
                    let _ = write!(svg, "<polyline points=\"");
                    for (i, v) in poly.vertices.iter().enumerate() {
                        let x = v.x - view_min_x;
                        let y = view_max_y - v.y;
                        if i > 0 {
                            let _ = write!(svg, " ");
                        }
                        let _ = write!(svg, "{x},{y}");
                    }
                    if poly.is_closed() {
                        let v = &poly.vertices[0];
                        let x = v.x - view_min_x;
                        let y = view_max_y - v.y;
                        let _ = write!(svg, " {x},{y}");
                    }
                    let _ = write!(svg, "\" />");
                }
            }
            _ => {}
        }
    }

    svg.push_str("</g></svg>");
    svg
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

        let (texts, info, svg_content) = Self::extract_dxf_content(&data)?;

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
.drawing-container {{ background: #000; border-radius: 8px; overflow: hidden; margin-bottom: 20px; box-shadow: 0 4px 6px rgba(0,0,0,0.3); }}
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

{svg_section}

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
            svg_section = if svg_content.is_empty() {
                String::new()
            } else {
                format!(r#"<div class="drawing-container">{svg_content}</div>"#)
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
