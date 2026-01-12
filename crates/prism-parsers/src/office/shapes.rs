// SPDX-License-Identifier: AGPL-3.0-only
//! # Shapes Module
//!
//! Parsing logic for `DrawingML` shapes and text bodies.

use crate::office::utils;
use prism_core::document::{
    ContentBlock, Dimensions, ImageBlock, Rect, ShapeStyle, TextBlock, TextRun, TextStyle,
};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::BufRead;

/// Parse a shape element (`p:sp`) into a `ContentBlock`
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse_shape(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Option<ContentBlock> {
    let mut bounds = Rect::default();
    let mut style = ShapeStyle::default();
    let mut text_runs = Vec::new();
    let mut rotation = 0.0;
    // Auxiliary buffer for nested parsing to avoid borrow issues with `buf` which is borrowed by `e`
    let mut inner_buf = Vec::new();

    let mut in_ln = false;

    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:xfrm" | b"p:xfrm" | b"xfrm" => {
                    bounds = parse_transform_2d(reader, &mut inner_buf);
                    // Rotation? a:xfrm has rot attribute (60000ths of a degree)
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"rot" {
                            if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                rotation = val / 60_000.0;
                            }
                        }
                    }
                }
                b"a:solidFill" => {
                    // Try to find srgbClr
                    // Since solidFill is a container, we need to iterate its children or check next event
                    // Actually, let's just wait for srgbClr event to appear?
                    // But srgbClr might appear in other contexts (text runs).
                    // To be safe, we should really track context.
                    // For now, let's try a simple heuristic: if we see srgbClr and we haven't parsed text yet, it's likely shape fill.
                    if text_runs.is_empty() {
                        // We need to peek or read inside.
                        // Let's implement a quick helper or just use a flag?
                        // Simpler: iterate inside solidFill
                        // But we can't easily iterate inside without consuming.
                    }
                }
                b"a:srgbClr" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"val" {
                            let color = format!("#{}", utils::attr_value(&attr.value));
                            if in_ln {
                                style.stroke_color = Some(color);
                            } else {
                                style.fill_color = Some(color);
                            }
                        }
                    }
                }
                b"a:schemeClr" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"val" {
                            let color = resolve_scheme_color(&utils::attr_value(&attr.value));
                            if in_ln {
                                style.stroke_color = Some(color);
                            } else {
                                style.fill_color = Some(color);
                            }
                        }
                    }
                }
                b"a:ln" => {
                    in_ln = true;
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"w" {
                            if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                // EMUs to points
                                style.stroke_width = Some(val / 12700.0);
                            }
                        }
                    }
                }
                b"p:txBody" => {
                    text_runs = parse_text_body(reader, &mut inner_buf, b"p:txBody");
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"a:ln" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"w" {
                            if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                style.stroke_width = Some(val / 12700.0);
                            }
                        }
                    }
                }
                b"a:srgbClr" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"val" {
                            let color = format!("#{}", utils::attr_value(&attr.value));
                            if in_ln {
                                style.stroke_color = Some(color);
                            } else {
                                style.fill_color = Some(color);
                            }
                        }
                    }
                }
                b"a:schemeClr" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"val" {
                            let color = resolve_scheme_color(&utils::attr_value(&attr.value));
                            if in_ln {
                                style.stroke_color = Some(color);
                            } else {
                                style.fill_color = Some(color);
                            }
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => {
                let name = e.name();
                if name.as_ref() == b"p:sp" {
                    break;
                } else if name.as_ref() == b"a:ln" {
                    in_ln = false;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    // Always return the shape, even without text (for colored rectangles, boxes, etc.)
    let mut block = TextBlock::new(bounds);
    for run in text_runs {
        block.add_run(run);
    }
    block.style = style;
    block.rotation = rotation;
    Some(ContentBlock::Text(block))
}

use std::collections::HashMap;

/// Parse a picture element (`p:pic`) into a `ContentBlock`
#[must_use]
pub fn parse_picture<S: std::hash::BuildHasher>(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    rels: &HashMap<String, String, S>,
) -> Option<ContentBlock> {
    let mut bounds = Rect::default();
    let mut embed_id = String::new();
    let mut alt_text = None;
    let mut image_format = None;

    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:xfrm" | b"p:xfrm" | b"xfrm" => {
                    bounds = parse_transform_2d(reader, buf);
                }
                b"a:blip" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"r:embed" {
                            embed_id = utils::attr_value(&attr.value);
                        }
                    }
                }
                b"p:cNvPr" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"descr" {
                            alt_text = Some(utils::attr_value(&attr.value));
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"a:blip" {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"r:embed" {
                            embed_id = utils::attr_value(&attr.value);
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"p:pic" {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    if embed_id.is_empty() {
        return None;
    }

    let image_path = if let Some(target) = rels.get(&embed_id) {
        let path = target.clone();
        if let Some(ext) = std::path::Path::new(&path)
            .extension()
            .and_then(|s| s.to_str())
        {
            image_format = Some(match ext.to_lowercase().as_str() {
                "png" => "image/png".to_string(),
                "jpg" | "jpeg" => "image/jpeg".to_string(),
                "gif" => "image/gif".to_string(),
                "svg" => "image/svg+xml".to_string(),
                _ => format!("image/{ext}"),
            });
        }
        path
    } else {
        // If not found in rels, keep embed_id as resource_id or empty?
        // Fallback to embed_id if no path resolved, but usually this means broken link
        embed_id.clone()
    };

    Some(ContentBlock::Image(ImageBlock {
        bounds,
        resource_id: image_path,
        alt_text,
        format: image_format,
        original_size: None, // TODO: Get intrinsic size from headers?
        style: ShapeStyle::default(),
        rotation: 0.0,
    }))
}

/// Parse a graphic frame element (`p:graphicFrame`) into a `ContentBlock`
#[must_use]
pub fn parse_graphic_frame(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Option<ContentBlock> {
    let mut bounds = Rect::default();
    let mut table_block = None;

    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"p:xfrm" => {
                    bounds = parse_transform_2d(reader, buf);
                }
                b"a:tbl" => {
                    if let Ok(mut block) = crate::office::tables::parse_drawingml_table(reader) {
                        block.style = ShapeStyle::default();
                        block.rotation = 0.0;
                        table_block = Some(block);
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"p:graphicFrame" {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    if let Some(mut block) = table_block {
        block.bounds = bounds;
        Some(ContentBlock::Table(block))
    } else {
        None
    }
}

/// Parse a background element (`p:bg`) into a `ContentBlock`
/// Handles:
/// - `a:gradFill` - Gradient fills (converted to CSS linear-gradient)
/// - `a:solidFill` - Solid color fills
/// - `a:blipFill` / `a:blip` - Image fills
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse_background<S: std::hash::BuildHasher>(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    rels: &HashMap<String, String, S>,
    dimensions: Dimensions,
) -> Option<ContentBlock> {
    let mut embed_id = String::new();
    let mut solid_color: Option<String> = None;
    let mut gradient_stops: Vec<(u32, String)> = Vec::new();
    let mut gradient_angle: f64 = 90.0;

    // Parse p:bg to find fill type
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:gradFill" => {
                    // Parse gradient fill
                    parse_gradient_fill(reader, buf, &mut gradient_stops, &mut gradient_angle);
                }
                b"a:solidFill" => {
                    // Parse solid fill
                    solid_color = parse_solid_fill(reader, buf);
                }
                b"a:blip" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"r:embed" {
                            embed_id = utils::attr_value(&attr.value);
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"a:blip" {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"r:embed" {
                            embed_id = utils::attr_value(&attr.value);
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"p:bg" {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    // Priority: Image > Gradient > Solid
    if !embed_id.is_empty() {
        let image_path = rels.get(&embed_id).cloned().unwrap_or(embed_id);
        let image_format = std::path::Path::new(&image_path)
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| match ext.to_lowercase().as_str() {
                "png" => "image/png".to_string(),
                "jpg" | "jpeg" => "image/jpeg".to_string(),
                "gif" => "image/gif".to_string(),
                "svg" => "image/svg+xml".to_string(),
                _ => format!("image/{ext}"),
            });

        return Some(ContentBlock::Image(ImageBlock {
            bounds: Rect::new(0.0, 0.0, dimensions.width, dimensions.height),
            resource_id: image_path,
            alt_text: Some("Background Image".to_string()),
            format: image_format,
            original_size: None,
            style: ShapeStyle::default(),
            rotation: 0.0,
        }));
    }

    // Handle gradient fill - use a TextBlock with empty runs as background
    if !gradient_stops.is_empty() {
        let gradient_css = build_css_gradient(&gradient_stops, gradient_angle);
        let style = ShapeStyle {
            fill_color: Some(gradient_css),
            ..ShapeStyle::default()
        };

        return Some(ContentBlock::Text(TextBlock {
            bounds: Rect::new(0.0, 0.0, dimensions.width, dimensions.height),
            runs: Vec::new(),
            paragraph_style: None,
            style,
            rotation: 0.0,
        }));
    }

    // Handle solid fill
    if let Some(color) = solid_color {
        let style = ShapeStyle {
            fill_color: Some(color),
            ..ShapeStyle::default()
        };

        return Some(ContentBlock::Text(TextBlock {
            bounds: Rect::new(0.0, 0.0, dimensions.width, dimensions.height),
            runs: Vec::new(),
            paragraph_style: None,
            style,
            rotation: 0.0,
        }));
    }

    None
}

/// Parse a:gradFill element to extract gradient stops and angle
fn parse_gradient_fill(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    stops: &mut Vec<(u32, String)>,
    angle: &mut f64,
) {
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:lin" => {
                    // Linear gradient angle
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"ang" {
                            if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                // OOXML angle is in 1/60000 of a degree
                                *angle = val / 60000.0;
                            }
                        }
                    }
                }
                b"a:gs" => {
                    // Gradient stop
                    let mut pos: u32 = 0;
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"pos" {
                            if let Ok(val) = utils::attr_value(&attr.value).parse::<u32>() {
                                pos = val / 1000; // Convert from 1/100000 to percentage
                            }
                        }
                    }
                    // Parse color inside gs
                    if let Some(color) = parse_color_inside(reader, buf, b"a:gs") {
                        stops.push((pos, color));
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"a:gradFill" {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
}

/// Parse a:solidFill element to extract color
fn parse_solid_fill(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Option<String> {
    parse_color_inside(reader, buf, b"a:solidFill")
}

/// Parse color elements (a:srgbClr, a:schemeClr) inside a container
fn parse_color_inside(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    end_tag: &[u8],
) -> Option<String> {
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e) | Event::Empty(e)) => match e.name().as_ref() {
                b"a:srgbClr" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"val" {
                            return Some(format!("#{}", utils::attr_value(&attr.value)));
                        }
                    }
                }
                b"a:schemeClr" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"val" {
                            let scheme = utils::attr_value(&attr.value);
                            return Some(resolve_scheme_color(&scheme));
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => {
                if e.name().as_ref() == end_tag {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

/// Resolve OOXML scheme color to RGB hex
/// Falls back to default `PowerPoint` color palette
fn resolve_scheme_color(scheme: &str) -> String {
    match scheme {
        "dk1" | "tx1" => "#000000".to_string(), // Dark 1 (usually black)
        "lt1" | "bg1" => "#FFFFFF".to_string(), // Light 1 (usually white)
        "dk2" | "tx2" => "#44546A".to_string(), // Dark 2
        "lt2" | "bg2" => "#E7E6E6".to_string(), // Light 2
        "accent1" => "#4472C4".to_string(),     // Accent 1 (blue)
        "accent2" => "#ED7D31".to_string(),     // Accent 2 (orange)
        "accent3" => "#A5A5A5".to_string(),     // Accent 3 (gray)
        "accent4" => "#FFC000".to_string(),     // Accent 4 (gold)
        "accent5" => "#5B9BD5".to_string(),     // Accent 5 (light blue)
        "accent6" => "#70AD47".to_string(),     // Accent 6 (green)
        "hlink" => "#0563C1".to_string(),       // Hyperlink
        "folHlink" => "#954F72".to_string(),    // Followed hyperlink
        _ => format!("#{scheme}"),              // Fallback: try as hex
    }
}

/// Build CSS linear-gradient from gradient stops
fn build_css_gradient(stops: &[(u32, String)], angle: f64) -> String {
    if stops.is_empty() {
        return "#FFFFFF".to_string();
    }

    if stops.len() == 1 {
        return stops[0].1.clone();
    }

    let stops_css: Vec<String> = stops
        .iter()
        .map(|(pos, color)| format!("{color} {pos}%"))
        .collect();

    format!("linear-gradient({angle}deg, {})", stops_css.join(", "))
}

/// Parse a transform element (`a:xfrm` or `p:xfrm`) into a `Rect`
#[must_use]
pub fn parse_transform_2d(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Rect {
    let mut bounds = Rect::default();
    let mut depth = 0;

    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:off" | b"off" => {
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"x" => {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    bounds.x = val / 12700.0;
                                }
                            }
                            b"y" => {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    bounds.y = val / 12700.0;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"a:ext" | b"ext" => {
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"cx" => {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    bounds.width = val / 12700.0;
                                }
                            }
                            b"cy" => {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    bounds.height = val / 12700.0;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => depth += 1,
            },
            Ok(Event::End(_)) => {
                if depth > 0 {
                    depth -= 1;
                } else {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    bounds
}

/// Parse a text body element (`p:txBody` or `a:txBody`) into a list of `TextRun`s
#[allow(clippy::too_many_lines)]
pub fn parse_text_body<R: BufRead>(
    reader: &mut Reader<R>,
    buf: &mut Vec<u8>,
    end_tag: &[u8],
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut current_run_style = TextStyle::default();
    let mut current_run_text = String::new();
    let mut in_run = false;

    // Paragraph-level properties
    let mut current_para_alignment: Option<prism_core::document::TextAlignment> = None;
    let mut current_para_bullet: Option<String> = None;
    let mut current_para_indent: Option<f64> = None;
    let mut para_run_count = 0; // Track runs within paragraph for bullet prepend
    let mut auto_number_counter = 1;

    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:p" => {
                    // Reset paragraph properties
                    current_para_alignment = None;
                    current_para_bullet = None;
                    current_para_indent = None;
                    para_run_count = 0;
                }
                b"a:pPr" => {
                    // Parse paragraph properties
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"algn" => {
                                let val = utils::attr_value(&attr.value);
                                current_para_alignment = Some(match val.as_str() {
                                    "ctr" => prism_core::document::TextAlignment::Center,
                                    "r" => prism_core::document::TextAlignment::Right,
                                    "just" => prism_core::document::TextAlignment::Justify,
                                    _ => prism_core::document::TextAlignment::Left,
                                });
                            }
                            b"marL" => {
                                // Left margin in EMUs
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    current_para_indent = Some(val / 12700.0);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"a:buChar" => {
                    // Character bullet - extract the char attribute
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"char" {
                            current_para_bullet =
                                Some(format!("{} ", utils::attr_value(&attr.value)));
                        }
                    }
                }
                b"a:buAutoNum" => {
                    // Auto-numbered bullet
                    let mut num_type = String::from("arabicPeriod");
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"type" {
                            num_type = utils::attr_value(&attr.value);
                        }
                    }
                    // Generate bullet based on type
                    let bullet_str = match num_type.as_str() {
                        "arabicParenR" => format!("{auto_number_counter}) "),
                        "romanLcPeriod" => format!("{}. ", to_roman_lowercase(auto_number_counter)),
                        "romanUcPeriod" => format!("{}. ", to_roman_uppercase(auto_number_counter)),
                        "alphaLcPeriod" => format!("{}. ", to_alpha_lowercase(auto_number_counter)),
                        "alphaUcPeriod" => format!("{}. ", to_alpha_uppercase(auto_number_counter)),
                        // Default includes arabicPeriod and any unknown type
                        _ => format!("{auto_number_counter}. "),
                    };
                    current_para_bullet = Some(bullet_str);
                    auto_number_counter += 1;
                }
                b"a:buNone" => {
                    // Explicitly no bullet
                    current_para_bullet = None;
                }
                b"a:r" => {
                    in_run = true;
                    current_run_style = TextStyle::default();
                    // Apply paragraph-level properties to run
                    current_run_style.alignment = current_para_alignment;
                    current_run_style.left_indent = current_para_indent;
                    current_run_text.clear();
                }
                b"a:rPr" => {
                    if in_run {
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"sz" => {
                                    if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                        current_run_style.font_size = Some(val / 100.0);
                                    }
                                }
                                b"b" => {
                                    current_run_style.bold = utils::attr_value(&attr.value) == "1";
                                }
                                b"i" => {
                                    current_run_style.italic =
                                        utils::attr_value(&attr.value) == "1";
                                }
                                b"u" => {
                                    current_run_style.underline =
                                        utils::attr_value(&attr.value) == "sng";
                                }
                                _ => {}
                            }
                        }
                    }
                }
                b"a:latin" => {
                    if in_run {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"typeface" {
                                current_run_style.font_family =
                                    Some(utils::attr_value(&attr.value));
                            }
                        }
                    }
                }
                b"a:srgbClr" => {
                    if in_run {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"val" {
                                current_run_style.color =
                                    Some(format!("#{}", utils::attr_value(&attr.value)));
                            }
                        }
                    }
                }
                b"a:schemeClr" => {
                    if in_run {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"val" {
                                let scheme = utils::attr_value(&attr.value);
                                current_run_style.color = Some(resolve_scheme_color(&scheme));
                            }
                        }
                    }
                }
                b"a:highlight" => {
                    // Parse highlight color for text background
                    if in_run {
                        // Inline parse for colors within highlight (type safety)
                        loop {
                            match reader.read_event_into(buf) {
                                Ok(Event::Start(inner) | Event::Empty(inner)) => {
                                    match inner.name().as_ref() {
                                        b"a:srgbClr" => {
                                            for attr in inner.attributes().flatten() {
                                                if attr.key.as_ref() == b"val" {
                                                    current_run_style.background_color =
                                                        Some(format!(
                                                            "#{}",
                                                            utils::attr_value(&attr.value)
                                                        ));
                                                }
                                            }
                                        }
                                        b"a:schemeClr" => {
                                            for attr in inner.attributes().flatten() {
                                                if attr.key.as_ref() == b"val" {
                                                    current_run_style.background_color =
                                                        Some(resolve_scheme_color(
                                                            &utils::attr_value(&attr.value),
                                                        ));
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                Ok(Event::End(inner))
                                    if inner.name().as_ref() == b"a:highlight" =>
                                {
                                    break
                                }
                                Ok(Event::Eof) => break,
                                _ => {}
                            }
                            buf.clear();
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"a:buChar" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"char" {
                            current_para_bullet =
                                Some(format!("{} ", utils::attr_value(&attr.value)));
                        }
                    }
                }
                b"a:buAutoNum" => {
                    // Simplified: just generate arabic period numbering for Empty element
                    current_para_bullet = Some(format!("{auto_number_counter}. "));
                    auto_number_counter += 1;
                }
                b"a:buNone" => {
                    current_para_bullet = None;
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if in_run {
                    if let Ok(text) = e.unescape() {
                        current_run_text.push_str(&text);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"a:p" {
                    // End of paragraph, add newline
                    runs.push(TextRun {
                        text: "\n".to_string(),
                        style: TextStyle::default(),
                        bounds: None,
                        char_positions: None,
                    });
                    // Reset auto-number for each paragraph (optional, depends on slide behavior)
                    // Comment out if you want continuous numbering across paragraphs
                    // auto_number_counter = 1;
                } else if e.name().as_ref() == b"a:r" {
                    in_run = false;
                    if !current_run_text.is_empty() {
                        // Set bullet on first run of paragraph
                        if para_run_count == 0 {
                            current_run_style.bullet.clone_from(&current_para_bullet);
                        }

                        runs.push(TextRun {
                            text: std::mem::take(&mut current_run_text),
                            style: current_run_style.clone(),
                            bounds: None,
                            char_positions: None,
                        });
                        para_run_count += 1;
                    }
                } else if e.name().as_ref() == end_tag {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    runs
}

/// Convert number to lowercase Roman numerals
fn to_roman_lowercase(n: u32) -> String {
    to_roman(n).to_lowercase()
}

/// Convert number to uppercase Roman numerals  
fn to_roman_uppercase(n: u32) -> String {
    to_roman(n)
}

/// Convert number to Roman numerals
fn to_roman(mut n: u32) -> String {
    let numerals = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut result = String::new();
    for (value, symbol) in numerals {
        while n >= value {
            result.push_str(symbol);
            n -= value;
        }
    }
    result
}

/// Convert number to lowercase letter (a, b, c, ...)
fn to_alpha_lowercase(n: u32) -> String {
    if n == 0 {
        return String::new();
    }
    let c = ((n - 1) % 26) as u8 + b'a';
    (c as char).to_string()
}

/// Convert number to uppercase letter (A, B, C, ...)
fn to_alpha_uppercase(n: u32) -> String {
    if n == 0 {
        return String::new();
    }
    let c = ((n - 1) % 26) as u8 + b'A';
    (c as char).to_string()
}
