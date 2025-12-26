// SPDX-License-Identifier: AGPL-3.0-only
use crate::office::utils;
use prism_core::document::{
    ContentBlock, Dimensions, ImageBlock, Rect, ShapeStyle, TextBlock, TextRun, TextStyle,
};
use quick_xml::events::Event;
use quick_xml::Reader;

/// Parse a shape element (p:sp) into a ContentBlock
/// Parse a shape element (p:sp) into a ContentBlock
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
                                rotation = val / 60000.0;
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
                            let color = utils::attr_value(&attr.value);
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
                    // Handle self-closing color tags
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"val" {
                            let color = utils::attr_value(&attr.value);
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

    if !text_runs.is_empty() {
        let mut block = TextBlock::new(bounds);
        for run in text_runs {
            block.add_run(run);
        }
        block.style = style;
        block.rotation = rotation;
        return Some(ContentBlock::Text(block));
    }

    None
}

use std::collections::HashMap;

/// Parse a picture element (p:pic) into a ContentBlock
pub fn parse_picture(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    rels: &HashMap<String, String>,
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
        buf.clear();
    }

    let image_path = if let Some(target) = rels.get(&embed_id) {
        let mut path = target.clone();
        if let Some(ext) = std::path::Path::new(&path)
            .extension()
            .and_then(|s| s.to_str())
        {
            image_format = Some(match ext.to_lowercase().as_str() {
                "png" => "image/png".to_string(),
                "jpg" | "jpeg" => "image/jpeg".to_string(),
                "gif" => "image/gif".to_string(),
                "svg" => "image/svg+xml".to_string(),
                _ => format!("image/{}", ext),
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

/// Parse a graphic frame element (p:graphicFrame) into a ContentBlock
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

/// Parse a background element (p:bg) into a ContentBlock (Image)
pub fn parse_background(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    rels: &HashMap<String, String>,
    dimensions: Dimensions,
) -> Option<ContentBlock> {
    let mut embed_id = String::new();

    // Iterate through p:bg children to find p:bgPr -> a:blipFill -> a:blip
    // Since p:bg is a container, we just loop until we find a:blip or end of p:bg

    // Actually, we can just look for a:blip anywhere inside p:bg.
    // Assuming simple structure.

    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:blip" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"r:embed" {
                            embed_id = utils::attr_value(&attr.value);
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"a:blip" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"r:embed" {
                            embed_id = utils::attr_value(&attr.value);
                        }
                    }
                }
                _ => {}
            },
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

    if embed_id.is_empty() {
        return None;
    }

    let image_path = if let Some(target) = rels.get(&embed_id) {
        target.clone()
    } else {
        embed_id.clone()
    };

    // Determine format from path
    let image_format = if let Some(ext) = std::path::Path::new(&image_path)
        .extension()
        .and_then(|s| s.to_str())
    {
        Some(match ext.to_lowercase().as_str() {
            "png" => "image/png".to_string(),
            "jpg" | "jpeg" => "image/jpeg".to_string(),
            "gif" => "image/gif".to_string(),
            "svg" => "image/svg+xml".to_string(),
            _ => format!("image/{}", ext),
        })
    } else {
        None
    };

    Some(ContentBlock::Image(ImageBlock {
        bounds: Rect::new(0.0, 0.0, dimensions.width, dimensions.height),
        resource_id: image_path,
        alt_text: Some("Background Image".to_string()),
        format: image_format,
        original_size: None,
        style: ShapeStyle::default(),
        rotation: 0.0,
    }))
}

/// Parse a transform element (a:xfrm or p:xfrm) into a Rect
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

/// Parse a text body element (p:txBody) into a list of TextRuns
use std::io::BufRead;

/// Parse a text body element (p:txBody or a:txBody) into a list of TextRuns
pub fn parse_text_body<R: BufRead>(
    reader: &mut Reader<R>,
    buf: &mut Vec<u8>,
    end_tag: &[u8],
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut current_run_style = TextStyle::default();
    let mut current_run_text = String::new();
    let mut in_run = false;

    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:p" => {
                    // Paragraph start
                }
                b"a:r" => {
                    in_run = true;
                    current_run_style = TextStyle::default(); // Reset style for new run
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
                                current_run_style.color = Some(utils::attr_value(&attr.value));
                            }
                        }
                    }
                }
                b"a:t" => {
                    // Text content is usually in a text event inside a:t, but sometimes directly?
                    // actually a:t usually contains text.
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
                } else if e.name().as_ref() == b"a:r" {
                    in_run = false;
                    if !current_run_text.is_empty() {
                        runs.push(TextRun {
                            text: current_run_text.clone(),
                            style: current_run_style.clone(),
                            bounds: None,
                            char_positions: None,
                        });
                        current_run_text.clear();
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
