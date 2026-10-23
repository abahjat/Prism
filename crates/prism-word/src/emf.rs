// SPDX-License-Identifier: AGPL-3.0-only
//! Pure-Rust Windows Metafile (`[MS-WMF]`) and Enhanced Metafile (`[MS-EMF]`) interpreter.
//!
//! Converts embedded `.wmf` and `.emf` vector graphics into positioned [`prism_layout::Item`]
//! primitives (`PathItem` and `GlyphRun`) scaled into a destination [`Rect`], with strict bounds
//! on record count, GDI object table slots, and polygon vertex counts.

use prism_layout::{
    Affine, FillPaint, FillRule, FontSystem, Item, LineCap, LineJoin, ParagraphSpec, PathCommand,
    PathItem, Point, Rect, Rgba, SpanSpec, StrokeStyle, TextAlign, TextLayoutEngine,
    normalize_zero,
};
use std::collections::BTreeMap;

/// Maximum number of metafile records processed before stopping.
pub const MAX_METAFILE_RECORDS: usize = 50_000;
/// Maximum number of GDI object table entries.
const MAX_GDI_OBJECTS: u32 = 4096;
/// Maximum number of vertices in a single polygon/polyline record.
const MAX_POLY_POINTS: usize = 16_384;

/// Stock GDI object base index (`0x8000_0000`).
const STOCK_OBJECT_BASE: u32 = 0x8000_0000;

#[derive(Debug, Clone)]
enum GdiObject {
    Pen {
        style: u32,
        width: f32,
        color: Rgba,
    },
    Brush {
        style: u32,
        color: Rgba,
    },
    Font {
        family: String,
        size_logical: f32,
        bold: bool,
        italic: bool,
    },
}

#[derive(Debug, Clone)]
struct DcState {
    window_org: (f32, f32),
    window_ext: (f32, f32),
    cur_pos: (f32, f32),
    pen_style: u32,
    pen_width: f32,
    pen_color: Rgba,
    brush_style: u32,
    brush_color: Rgba,
    font_family: String,
    font_size: f32,
    font_bold: bool,
    font_italic: bool,
    text_color: Rgba,
    fill_rule: FillRule,
}

impl Default for DcState {
    fn default() -> Self {
        Self {
            window_org: (0.0, 0.0),
            window_ext: (1000.0, 1000.0),
            cur_pos: (0.0, 0.0),
            pen_style: 0, // PS_SOLID
            pen_width: 1.0,
            pen_color: Rgba::BLACK,
            brush_style: 0, // BS_SOLID
            brush_color: Rgba::WHITE,
            font_family: "Arial".to_string(),
            font_size: 12.0,
            font_bold: false,
            font_italic: false,
            text_color: Rgba::BLACK,
            fill_rule: FillRule::EvenOdd,
        }
    }
}

/// Check whether `data` starts with a WMF (placeable or standard) or EMF signature.
#[must_use]
pub fn is_metafile(data: &[u8]) -> bool {
    is_emf(data) || is_wmf(data)
}

/// Check whether `data` is an Enhanced Metafile (`EMR_HEADER` + `" EMF"` signature).
#[must_use]
pub fn is_emf(data: &[u8]) -> bool {
    if data.len() < 44 {
        return false;
    }
    let rec_type = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let sig = u32::from_le_bytes([data[40], data[41], data[42], data[43]]);
    rec_type == 1 && sig == 0x464D_4520
}

/// Check whether `data` is a Windows Metafile (Aldus Placeable header `0x9AC6CDD7` or standard header).
#[must_use]
pub fn is_wmf(data: &[u8]) -> bool {
    if data.len() >= 22 {
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if magic == 0x9AC6_CDD7 {
            return true;
        }
    }
    if data.len() >= 18 {
        let mt_type = u16::from_le_bytes([data[0], data[1]]);
        let mt_header_size = u16::from_le_bytes([data[2], data[3]]);
        let mt_version = u16::from_le_bytes([data[4], data[5]]);
        if (mt_type == 1 || mt_type == 2)
            && mt_header_size == 9
            && (mt_version == 0x0100 || mt_version == 0x0300)
        {
            return true;
        }
    }
    false
}

/// Render a WMF or EMF byte stream into positioned [`Item`]s inside `dest`.
pub fn render_metafile(
    data: &[u8],
    dest: Rect,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
) -> Vec<Item> {
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return Vec::new();
    }
    if is_emf(data) {
        render_emf(data, dest, font_sys, text_engine)
    } else if is_wmf(data) {
        render_wmf(data, dest, font_sys, text_engine)
    } else {
        Vec::new()
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::match_same_arms,
    clippy::manual_range_patterns
)]
fn render_emf(
    data: &[u8],
    dest: Rect,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
) -> Vec<Item> {
    if data.len() < 88 {
        return Vec::new();
    }
    let bounds_left = read_i32(data, 8) as f32;
    let bounds_top = read_i32(data, 12) as f32;
    let bounds_right = read_i32(data, 16) as f32;
    let bounds_bottom = read_i32(data, 20) as f32;

    let mut dc = DcState::default();
    let bw = (bounds_right - bounds_left).abs();
    let bh = (bounds_bottom - bounds_top).abs();
    if bw > 0.0 && bh > 0.0 {
        dc.window_org = (bounds_left, bounds_top);
        dc.window_ext = (bw, bh);
    }

    let mut dc_stack: Vec<DcState> = Vec::new();
    let mut objects: BTreeMap<u32, GdiObject> = BTreeMap::new();
    let mut items: Vec<Item> = Vec::new();
    let mut active_path: Option<Vec<PathCommand>> = None;

    let mut offset = 0usize;
    let mut record_count = 0usize;

    while offset + 8 <= data.len() && record_count < MAX_METAFILE_RECORDS {
        record_count += 1;
        let rec_type = read_u32(data, offset);
        let rec_size = read_u32(data, offset + 4) as usize;
        if rec_size < 8 || offset + rec_size > data.len() {
            break;
        }
        let rec = &data[offset..offset + rec_size];
        offset += rec_size;

        match rec_type {
            14 => break, // EMR_EOF
            9 if rec.len() >= 16 => {
                let cx = read_i32(rec, 8) as f32;
                let cy = read_i32(rec, 12) as f32;
                if cx.abs() > f32::EPSILON && cy.abs() > f32::EPSILON {
                    dc.window_ext = (cx, cy);
                }
            }
            10 if rec.len() >= 16 => {
                let x = read_i32(rec, 8) as f32;
                let y = read_i32(rec, 12) as f32;
                dc.window_org = (x, y);
            }
            19 if rec.len() >= 12 => {
                let mode = read_u32(rec, 8);
                dc.fill_rule = if mode == 2 {
                    FillRule::NonZero
                } else {
                    FillRule::EvenOdd
                };
            }
            24 if rec.len() >= 12 => {
                dc.text_color = read_colorref(rec, 8);
            }
            27 if rec.len() >= 16 => {
                let (x, y) = map_pt(&dc, dest, read_i32(rec, 8) as f32, read_i32(rec, 12) as f32);
                dc.cur_pos = (x, y);
                if let Some(ref mut path) = active_path {
                    path.push(PathCommand::MoveTo(Point::new(x, y)));
                }
            }
            54 if rec.len() >= 16 => {
                let (x, y) = map_pt(&dc, dest, read_i32(rec, 8) as f32, read_i32(rec, 12) as f32);
                if let Some(ref mut path) = active_path {
                    path.push(PathCommand::LineTo(Point::new(x, y)));
                } else if let Some(stroke) = dc_stroke(&dc, dest) {
                    items.push(Item::Path(PathItem {
                        commands: vec![
                            PathCommand::MoveTo(Point::new(dc.cur_pos.0, dc.cur_pos.1)),
                            PathCommand::LineTo(Point::new(x, y)),
                        ],
                        fill: None,
                        stroke: Some(stroke),
                        fill_rule: dc.fill_rule,
                        transform: Affine::IDENTITY,
                    }));
                }
                dc.cur_pos = (x, y);
            }
            33 => {
                if dc_stack.len() < 32 {
                    dc_stack.push(dc.clone());
                }
            }
            34 => {
                if let Some(prev) = dc_stack.pop() {
                    dc = prev;
                }
            }
            37 if rec.len() >= 12 => {
                let handle = read_u32(rec, 8);
                apply_object(&mut dc, &objects, handle);
            }
            40 if rec.len() >= 12 => {
                let handle = read_u32(rec, 8);
                objects.remove(&handle);
            }
            38 if rec.len() >= 28 => {
                let handle = read_u32(rec, 8);
                let style = read_u32(rec, 12) & 0x0F;
                let width = (read_i32(rec, 16) as f32).max(1.0);
                let color = read_colorref(rec, 24);
                if handle < MAX_GDI_OBJECTS {
                    objects.insert(
                        handle,
                        GdiObject::Pen {
                            style,
                            width,
                            color,
                        },
                    );
                }
            }
            95 if rec.len() >= 52 => {
                let handle = read_u32(rec, 8);
                let style = read_u32(rec, 28) & 0x0F;
                let width = (read_i32(rec, 32) as f32).max(1.0);
                let color = read_colorref(rec, 40);
                if handle < MAX_GDI_OBJECTS {
                    objects.insert(
                        handle,
                        GdiObject::Pen {
                            style,
                            width,
                            color,
                        },
                    );
                }
            }
            39 if rec.len() >= 24 => {
                let handle = read_u32(rec, 8);
                let style = read_u32(rec, 12);
                let color = read_colorref(rec, 16);
                if handle < MAX_GDI_OBJECTS {
                    objects.insert(handle, GdiObject::Brush { style, color });
                }
            }
            82 if rec.len() >= 104 => {
                let handle = read_u32(rec, 8);
                let lf_height = (read_i32(rec, 12) as f32).abs();
                let lf_weight = read_i32(rec, 28);
                let lf_italic = rec[32] != 0;
                let family = read_utf16le_str(&rec[40..104]);
                if handle < MAX_GDI_OBJECTS {
                    objects.insert(
                        handle,
                        GdiObject::Font {
                            family: if family.is_empty() {
                                "Arial".to_string()
                            } else {
                                family
                            },
                            size_logical: lf_height.max(6.0),
                            bold: lf_weight >= 700,
                            italic: lf_italic,
                        },
                    );
                }
            }
            42 | 43 if rec.len() >= 24 => {
                let (x1, y1) = map_pt(&dc, dest, read_i32(rec, 8) as f32, read_i32(rec, 12) as f32);
                let (x2, y2) = map_pt(
                    &dc,
                    dest,
                    read_i32(rec, 16) as f32,
                    read_i32(rec, 20) as f32,
                );
                let cmds = if rec_type == 43 {
                    rect_commands(x1, y1, x2, y2)
                } else {
                    ellipse_commands(x1, y1, x2, y2)
                };
                emit_or_append_path(&dc, dest, cmds, &mut active_path, &mut items);
            }
            3 | 4 if rec.len() >= 28 => {
                let count = (read_u32(rec, 24) as usize).min(MAX_POLY_POINTS);
                if 28 + count * 8 <= rec.len() && count >= 2 {
                    let mut cmds = Vec::with_capacity(count + 1);
                    for i in 0..count {
                        let px = read_i32(rec, 28 + i * 8) as f32;
                        let py = read_i32(rec, 32 + i * 8) as f32;
                        let (x, y) = map_pt(&dc, dest, px, py);
                        if i == 0 {
                            cmds.push(PathCommand::MoveTo(Point::new(x, y)));
                        } else {
                            cmds.push(PathCommand::LineTo(Point::new(x, y)));
                        }
                    }
                    if rec_type == 3 {
                        cmds.push(PathCommand::Close);
                        emit_or_append_path(&dc, dest, cmds, &mut active_path, &mut items);
                    } else if let Some(stroke) = dc_stroke(&dc, dest) {
                        items.push(Item::Path(PathItem {
                            commands: cmds,
                            fill: None,
                            stroke: Some(stroke),
                            fill_rule: dc.fill_rule,
                            transform: Affine::IDENTITY,
                        }));
                    }
                }
            }
            86 | 87 if rec.len() >= 28 => {
                let count = (read_u32(rec, 24) as usize).min(MAX_POLY_POINTS);
                if 28 + count * 4 <= rec.len() && count >= 2 {
                    let mut cmds = Vec::with_capacity(count + 1);
                    for i in 0..count {
                        let px = f32::from(read_i16(rec, 28 + i * 4));
                        let py = f32::from(read_i16(rec, 30 + i * 4));
                        let (x, y) = map_pt(&dc, dest, px, py);
                        if i == 0 {
                            cmds.push(PathCommand::MoveTo(Point::new(x, y)));
                        } else {
                            cmds.push(PathCommand::LineTo(Point::new(x, y)));
                        }
                    }
                    if rec_type == 86 {
                        cmds.push(PathCommand::Close);
                        emit_or_append_path(&dc, dest, cmds, &mut active_path, &mut items);
                    } else if let Some(stroke) = dc_stroke(&dc, dest) {
                        items.push(Item::Path(PathItem {
                            commands: cmds,
                            fill: None,
                            stroke: Some(stroke),
                            fill_rule: dc.fill_rule,
                            transform: Affine::IDENTITY,
                        }));
                    }
                }
            }
            59 => {
                active_path = Some(Vec::new());
            }
            60 => {}
            61 => {
                if let Some(ref mut path) = active_path {
                    path.push(PathCommand::Close);
                }
            }
            62 | 63 | 64 => {
                if let Some(cmds) = active_path.take()
                    && !cmds.is_empty()
                {
                    let fill = (rec_type == 62 || rec_type == 63)
                        .then(|| dc_fill(&dc))
                        .flatten();
                    let stroke = (rec_type == 63 || rec_type == 64)
                        .then(|| dc_stroke(&dc, dest))
                        .flatten();
                    if fill.is_some() || stroke.is_some() {
                        items.push(Item::Path(PathItem {
                            commands: cmds,
                            fill,
                            stroke,
                            fill_rule: dc.fill_rule,
                            transform: Affine::IDENTITY,
                        }));
                    }
                }
            }
            83 | 84 if rec.len() >= 76 => {
                let ref_x = read_i32(rec, 36) as f32;
                let ref_y = read_i32(rec, 40) as f32;
                let n_chars = (read_u32(rec, 44) as usize).min(4096);
                let off_string = read_u32(rec, 48) as usize;
                let text = if rec_type == 84 {
                    if off_string + n_chars * 2 <= rec.len() {
                        read_utf16le_exact(&rec[off_string..off_string + n_chars * 2])
                    } else {
                        String::new()
                    }
                } else if off_string + n_chars <= rec.len() {
                    rec[off_string..off_string + n_chars]
                        .iter()
                        .map(|&b| char::from(b))
                        .collect()
                } else {
                    String::new()
                };
                if !text.trim().is_empty() {
                    let (x, y) = map_pt(&dc, dest, ref_x, ref_y);
                    let scale_y = (dest.height / dc.window_ext.1.abs().max(1.0)).clamp(0.05, 10.0);
                    let pt_size = (dc.font_size * scale_y).clamp(4.0, 72.0);
                    let spec = ParagraphSpec {
                        spans: vec![SpanSpec {
                            text,
                            family: dc.font_family.clone(),
                            size: pt_size,
                            bold: dc.font_bold,
                            italic: dc.font_italic,
                            color: dc.text_color,
                            ..SpanSpec::default()
                        }],
                        align: TextAlign::Left,
                        origin: Point::new(x, (y - pt_size * 0.85).max(dest.y)),
                        ..ParagraphSpec::default()
                    };
                    let shaped = text_engine.layout_paragraph(&spec, font_sys);
                    for run in shaped.runs {
                        items.push(Item::Glyphs(run));
                    }
                }
            }
            _ => {}
        }
    }

    items
}

#[allow(clippy::too_many_lines)]
fn render_wmf(
    data: &[u8],
    dest: Rect,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
) -> Vec<Item> {
    let mut dc = DcState::default();
    let mut pos = 0usize;

    if data.len() >= 22 && read_u32(data, 0) == 0x9AC6_CDD7 {
        let left = f32::from(read_i16(data, 6));
        let top = f32::from(read_i16(data, 8));
        let right = f32::from(read_i16(data, 10));
        let bottom = f32::from(read_i16(data, 12));
        let bw = (right - left).abs();
        let bh = (bottom - top).abs();
        if bw > 0.0 && bh > 0.0 {
            dc.window_org = (left, top);
            dc.window_ext = (bw, bh);
        }
        pos = 22;
    }

    if pos + 18 > data.len() {
        return Vec::new();
    }
    let header_words = usize::from(read_u16(data, pos + 2));
    pos += header_words.max(9) * 2;

    let mut objects: Vec<Option<GdiObject>> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    let mut record_count = 0usize;

    while pos + 6 <= data.len() && record_count < MAX_METAFILE_RECORDS {
        record_count += 1;
        let rd_size_words = read_u32(data, pos) as usize;
        let rd_func = read_u16(data, pos + 4);
        let rec_bytes = rd_size_words.saturating_mul(2);
        if rd_func == 0 || rec_bytes < 6 || pos + rec_bytes > data.len() {
            break;
        }
        let params = &data[pos + 6..pos + rec_bytes];
        pos += rec_bytes;

        match rd_func {
            0x020B if params.len() >= 4 => {
                let y = f32::from(read_i16(params, 0));
                let x = f32::from(read_i16(params, 2));
                dc.window_org = (x, y);
            }
            0x020C if params.len() >= 4 => {
                let y = f32::from(read_i16(params, 0));
                let x = f32::from(read_i16(params, 2));
                if x.abs() > f32::EPSILON && y.abs() > f32::EPSILON {
                    dc.window_ext = (x, y);
                }
            }
            0x0209 if params.len() >= 4 => {
                dc.text_color = read_colorref(params, 0);
            }
            0x02FA if params.len() >= 10 => {
                let style = u32::from(read_u16(params, 0)) & 0x0F;
                let width = f32::from(read_i16(params, 2)).max(1.0);
                let color = read_colorref(params, 6);
                insert_wmf_object(
                    &mut objects,
                    GdiObject::Pen {
                        style,
                        width,
                        color,
                    },
                );
            }
            0x02FC if params.len() >= 8 => {
                let style = u32::from(read_u16(params, 0));
                let color = read_colorref(params, 2);
                insert_wmf_object(&mut objects, GdiObject::Brush { style, color });
            }
            0x02FB if params.len() >= 18 => {
                let height = f32::from(read_i16(params, 0)).abs().max(6.0);
                let weight = read_i16(params, 8);
                let italic = params[10] != 0;
                let face_bytes = &params[18..params.len().min(50)];
                let face: String = face_bytes
                    .iter()
                    .take_while(|&&b| b != 0)
                    .map(|&b| char::from(b))
                    .collect();
                insert_wmf_object(
                    &mut objects,
                    GdiObject::Font {
                        family: if face.is_empty() {
                            "Arial".to_string()
                        } else {
                            face
                        },
                        size_logical: height,
                        bold: weight >= 700,
                        italic,
                    },
                );
            }
            0x012D if params.len() >= 2 => {
                let idx = usize::from(read_u16(params, 0));
                if let Some(Some(obj)) = objects.get(idx) {
                    apply_gdi_obj(&mut dc, obj);
                }
            }
            0x01F0 if params.len() >= 2 => {
                let idx = usize::from(read_u16(params, 0));
                if let Some(slot) = objects.get_mut(idx) {
                    *slot = None;
                }
            }
            0x0214 if params.len() >= 4 => {
                let y = f32::from(read_i16(params, 0));
                let x = f32::from(read_i16(params, 2));
                dc.cur_pos = map_pt(&dc, dest, x, y);
            }
            0x0213 if params.len() >= 4 => {
                let y = f32::from(read_i16(params, 0));
                let x = f32::from(read_i16(params, 2));
                let (nx, ny) = map_pt(&dc, dest, x, y);
                if let Some(stroke) = dc_stroke(&dc, dest) {
                    items.push(Item::Path(PathItem {
                        commands: vec![
                            PathCommand::MoveTo(Point::new(dc.cur_pos.0, dc.cur_pos.1)),
                            PathCommand::LineTo(Point::new(nx, ny)),
                        ],
                        fill: None,
                        stroke: Some(stroke),
                        fill_rule: dc.fill_rule,
                        transform: Affine::IDENTITY,
                    }));
                }
                dc.cur_pos = (nx, ny);
            }
            0x041B | 0x0418 if params.len() >= 8 => {
                let bottom = f32::from(read_i16(params, 0));
                let right = f32::from(read_i16(params, 2));
                let top = f32::from(read_i16(params, 4));
                let left = f32::from(read_i16(params, 6));
                let (x1, y1) = map_pt(&dc, dest, left, top);
                let (x2, y2) = map_pt(&dc, dest, right, bottom);
                let cmds = if rd_func == 0x041B {
                    rect_commands(x1, y1, x2, y2)
                } else {
                    ellipse_commands(x1, y1, x2, y2)
                };
                let mut none_path = None;
                emit_or_append_path(&dc, dest, cmds, &mut none_path, &mut items);
            }
            0x0324 | 0x0325 if params.len() >= 6 => {
                let count = usize::from(read_u16(params, 0)).min(MAX_POLY_POINTS);
                if 2 + count * 4 <= params.len() && count >= 2 {
                    let mut cmds = Vec::with_capacity(count + 1);
                    for i in 0..count {
                        let px = f32::from(read_i16(params, 2 + i * 4));
                        let py = f32::from(read_i16(params, 4 + i * 4));
                        let (x, y) = map_pt(&dc, dest, px, py);
                        if i == 0 {
                            cmds.push(PathCommand::MoveTo(Point::new(x, y)));
                        } else {
                            cmds.push(PathCommand::LineTo(Point::new(x, y)));
                        }
                    }
                    if rd_func == 0x0324 {
                        cmds.push(PathCommand::Close);
                        let mut none_path = None;
                        emit_or_append_path(&dc, dest, cmds, &mut none_path, &mut items);
                    } else if let Some(stroke) = dc_stroke(&dc, dest) {
                        items.push(Item::Path(PathItem {
                            commands: cmds,
                            fill: None,
                            stroke: Some(stroke),
                            fill_rule: dc.fill_rule,
                            transform: Affine::IDENTITY,
                        }));
                    }
                }
            }
            0x0521 if params.len() >= 6 => {
                let n_chars = usize::from(read_u16(params, 0)).min(4096);
                let str_padded = n_chars + (n_chars % 2);
                if 2 + str_padded + 4 <= params.len() {
                    let text: String = params[2..2 + n_chars]
                        .iter()
                        .map(|&b| char::from(b))
                        .collect();
                    let y = f32::from(read_i16(params, 2 + str_padded));
                    let x = f32::from(read_i16(params, 4 + str_padded));
                    if !text.trim().is_empty() {
                        let (px, py) = map_pt(&dc, dest, x, y);
                        let scale_y =
                            (dest.height / dc.window_ext.1.abs().max(1.0)).clamp(0.05, 10.0);
                        let pt_size = (dc.font_size * scale_y).clamp(4.0, 72.0);
                        let spec = ParagraphSpec {
                            spans: vec![SpanSpec {
                                text,
                                family: dc.font_family.clone(),
                                size: pt_size,
                                bold: dc.font_bold,
                                italic: dc.font_italic,
                                color: dc.text_color,
                                ..SpanSpec::default()
                            }],
                            align: TextAlign::Left,
                            origin: Point::new(px, (py - pt_size * 0.85).max(dest.y)),
                            ..ParagraphSpec::default()
                        };
                        let shaped = text_engine.layout_paragraph(&spec, font_sys);
                        for run in shaped.runs {
                            items.push(Item::Glyphs(run));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    items
}

fn insert_wmf_object(objects: &mut Vec<Option<GdiObject>>, obj: GdiObject) {
    for slot in objects.iter_mut() {
        if slot.is_none() {
            *slot = Some(obj);
            return;
        }
    }
    if objects.len() < MAX_GDI_OBJECTS as usize {
        objects.push(Some(obj));
    }
}

fn apply_object(dc: &mut DcState, objects: &BTreeMap<u32, GdiObject>, handle: u32) {
    if handle >= STOCK_OBJECT_BASE {
        match handle - STOCK_OBJECT_BASE {
            0 => {
                dc.brush_style = 0;
                dc.brush_color = Rgba::WHITE;
            }
            4 => {
                dc.brush_style = 0;
                dc.brush_color = Rgba::BLACK;
            }
            5 => {
                dc.brush_style = 1;
            }
            6 => {
                dc.pen_style = 0;
                dc.pen_color = Rgba::WHITE;
            }
            7 => {
                dc.pen_style = 0;
                dc.pen_color = Rgba::BLACK;
            }
            8 => {
                dc.pen_style = 5;
            }
            _ => {}
        }
        return;
    }
    if let Some(obj) = objects.get(&handle) {
        apply_gdi_obj(dc, obj);
    }
}

fn apply_gdi_obj(dc: &mut DcState, obj: &GdiObject) {
    match obj {
        GdiObject::Pen {
            style,
            width,
            color,
        } => {
            dc.pen_style = *style;
            dc.pen_width = *width;
            dc.pen_color = *color;
        }
        GdiObject::Brush { style, color } => {
            dc.brush_style = *style;
            dc.brush_color = *color;
        }
        GdiObject::Font {
            family,
            size_logical,
            bold,
            italic,
        } => {
            dc.font_family.clone_from(family);
            dc.font_size = *size_logical;
            dc.font_bold = *bold;
            dc.font_italic = *italic;
        }
    }
}

fn map_pt(dc: &DcState, dest: Rect, lx: f32, ly: f32) -> (f32, f32) {
    let wx = dc.window_ext.0.abs().max(1.0);
    let wy = dc.window_ext.1.abs().max(1.0);
    let nx = ((lx - dc.window_org.0) / wx).clamp(-2.0, 3.0);
    let ny = ((ly - dc.window_org.1) / wy).clamp(-2.0, 3.0);
    let x = normalize_zero(dest.x + nx * dest.width);
    let y = normalize_zero(dest.y + ny * dest.height);
    (x, y)
}

fn dc_fill(dc: &DcState) -> Option<FillPaint> {
    if dc.brush_style == 1 {
        None
    } else {
        Some(FillPaint::Solid(dc.brush_color))
    }
}

fn dc_stroke(dc: &DcState, dest: Rect) -> Option<StrokeStyle> {
    if dc.pen_style == 5 {
        return None;
    }
    let scale = (dest.width / dc.window_ext.0.abs().max(1.0)).clamp(0.1, 4.0);
    let width = (dc.pen_width * scale).clamp(0.5, 12.0);
    let dash_array = match dc.pen_style {
        1 => vec![width * 4.0, width * 2.0],
        2 => vec![width, width * 2.0],
        3 => vec![width * 4.0, width * 2.0, width, width * 2.0],
        4 => vec![width * 4.0, width * 2.0, width, width * 2.0, width],
        _ => Vec::new(),
    };
    Some(StrokeStyle {
        color: dc.pen_color,
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        dash_array,
        dash_offset: 0.0,
    })
}

fn emit_or_append_path(
    dc: &DcState,
    dest: Rect,
    cmds: Vec<PathCommand>,
    active_path: &mut Option<Vec<PathCommand>>,
    items: &mut Vec<Item>,
) {
    if let Some(path) = active_path {
        path.extend(cmds);
    } else {
        let fill = dc_fill(dc);
        let stroke = dc_stroke(dc, dest);
        if fill.is_some() || stroke.is_some() {
            items.push(Item::Path(PathItem {
                commands: cmds,
                fill,
                stroke,
                fill_rule: dc.fill_rule,
                transform: Affine::IDENTITY,
            }));
        }
    }
}

fn rect_commands(x1: f32, y1: f32, x2: f32, y2: f32) -> Vec<PathCommand> {
    let l = x1.min(x2);
    let r = x1.max(x2);
    let t = y1.min(y2);
    let b = y1.max(y2);
    vec![
        PathCommand::MoveTo(Point::new(l, t)),
        PathCommand::LineTo(Point::new(r, t)),
        PathCommand::LineTo(Point::new(r, b)),
        PathCommand::LineTo(Point::new(l, b)),
        PathCommand::Close,
    ]
}

fn ellipse_commands(x1: f32, y1: f32, x2: f32, y2: f32) -> Vec<PathCommand> {
    let cx = f32::midpoint(x1, x2);
    let cy = f32::midpoint(y1, y2);
    let rx = (x2 - x1).abs() * 0.5;
    let ry = (y2 - y1).abs() * 0.5;
    let k = 0.552_284_8;
    let kx = rx * k;
    let ky = ry * k;
    vec![
        PathCommand::MoveTo(Point::new(cx + rx, cy)),
        PathCommand::CurveTo {
            ctrl1: Point::new(cx + rx, cy + ky),
            ctrl2: Point::new(cx + kx, cy + ry),
            to: Point::new(cx, cy + ry),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(cx - kx, cy + ry),
            ctrl2: Point::new(cx - rx, cy + ky),
            to: Point::new(cx - rx, cy),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(cx - rx, cy - ky),
            ctrl2: Point::new(cx - kx, cy - ry),
            to: Point::new(cx, cy - ry),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(cx + kx, cy - ry),
            ctrl2: Point::new(cx + rx, cy - ky),
            to: Point::new(cx + rx, cy),
        },
        PathCommand::Close,
    ]
}

fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_i16(b: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn read_i32(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn read_colorref(b: &[u8], off: usize) -> Rgba {
    Rgba::rgb(b[off], b[off + 1], b[off + 2])
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn read_utf16le_str(b: &[u8]) -> String {
    let mut u16s = Vec::with_capacity(b.len() / 2);
    for chunk in b.chunks_exact(2) {
        let code = u16::from_le_bytes([chunk[0], chunk[1]]);
        if code == 0 {
            break;
        }
        u16s.push(code);
    }
    String::from_utf16_lossy(&u16s)
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn read_utf16le_exact(b: &[u8]) -> String {
    let u16s: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&u16s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_layout::LayoutOptions;

    #[test]
    fn renders_emf_rectangle_and_eof() {
        let mut emf = vec![0u8; 88];
        emf[0..4].copy_from_slice(&1u32.to_le_bytes());
        emf[4..8].copy_from_slice(&88u32.to_le_bytes());
        emf[16..20].copy_from_slice(&200i32.to_le_bytes());
        emf[20..24].copy_from_slice(&100i32.to_le_bytes());
        emf[40..44].copy_from_slice(&0x464D_4520u32.to_le_bytes());

        emf.extend_from_slice(&43u32.to_le_bytes());
        emf.extend_from_slice(&24u32.to_le_bytes());
        emf.extend_from_slice(&10i32.to_le_bytes());
        emf.extend_from_slice(&10i32.to_le_bytes());
        emf.extend_from_slice(&190i32.to_le_bytes());
        emf.extend_from_slice(&90i32.to_le_bytes());

        emf.extend_from_slice(&14u32.to_le_bytes());
        emf.extend_from_slice(&20u32.to_le_bytes());
        emf.extend_from_slice(&[0u8; 12]);

        assert!(is_emf(&emf));
        assert!(is_metafile(&emf));

        let mut font_sys = FontSystem::new(&LayoutOptions::default());
        let mut text_engine = TextLayoutEngine::new();
        let items = render_metafile(
            &emf,
            Rect::new(50.0, 50.0, 200.0, 100.0),
            &mut font_sys,
            &mut text_engine,
        );
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], Item::Path(_)));
    }

    #[test]
    fn renders_placeable_wmf_rectangle() {
        let mut wmf = Vec::new();
        wmf.extend_from_slice(&0x9AC6_CDD7u32.to_le_bytes());
        wmf.extend_from_slice(&0u16.to_le_bytes());
        wmf.extend_from_slice(&0i16.to_le_bytes());
        wmf.extend_from_slice(&0i16.to_le_bytes());
        wmf.extend_from_slice(&100i16.to_le_bytes());
        wmf.extend_from_slice(&100i16.to_le_bytes());
        wmf.extend_from_slice(&96u16.to_le_bytes());
        wmf.extend_from_slice(&0u32.to_le_bytes());
        wmf.extend_from_slice(&0u16.to_le_bytes());

        wmf.extend_from_slice(&1u16.to_le_bytes());
        wmf.extend_from_slice(&9u16.to_le_bytes());
        wmf.extend_from_slice(&0x0300u16.to_le_bytes());
        wmf.extend_from_slice(&20u32.to_le_bytes());
        wmf.extend_from_slice(&2u16.to_le_bytes());
        wmf.extend_from_slice(&7u32.to_le_bytes());
        wmf.extend_from_slice(&0u16.to_le_bytes());

        wmf.extend_from_slice(&7u32.to_le_bytes());
        wmf.extend_from_slice(&0x041Bu16.to_le_bytes());
        wmf.extend_from_slice(&80i16.to_le_bytes());
        wmf.extend_from_slice(&80i16.to_le_bytes());
        wmf.extend_from_slice(&20i16.to_le_bytes());
        wmf.extend_from_slice(&20i16.to_le_bytes());

        wmf.extend_from_slice(&3u32.to_le_bytes());
        wmf.extend_from_slice(&0u16.to_le_bytes());

        assert!(is_wmf(&wmf));
        assert!(is_metafile(&wmf));

        let mut font_sys = FontSystem::new(&LayoutOptions::default());
        let mut text_engine = TextLayoutEngine::new();
        let items = render_metafile(
            &wmf,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            &mut font_sys,
            &mut text_engine,
        );
        assert_eq!(items.len(), 1);
    }
}
