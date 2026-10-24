// SPDX-License-Identifier: AGPL-3.0-only
//! Pure-Rust legacy `PowerPoint` 97–2003 binary reader (`[MS-PPT]`) producing a [`Presentation`].

use crate::model::{
    DEFAULT_SLIDE_HEIGHT_PT, DEFAULT_SLIDE_WIDTH_PT, NotesSlide, PlaceholderRef, PptTextRun,
    Presentation, ShapeFill, ShapeKind, ShapeStroke, Slide, SlideMaster, SlideShape, TextBody,
    TextLevelStyle, TextParagraph, TextRunStyle,
};
use prism_core::document::ImageResource;
use prism_core::error::{Error, Result};
use prism_layout::Rgba;
use std::io::{Cursor, Read};

/// Default maximum stream size for legacy `.ppt` streams (`64 MiB`).
pub const DEFAULT_MAX_PPT_STREAM_BYTES: usize = 64 * 1024 * 1024;

/// Master units per typographic point (`576 master units per inch / 72 points per inch = 8.0`).
const MASTER_UNITS_PER_PT: f32 = 8.0;

// [MS-PPT] Record types
const RT_DOCUMENT: u16 = 0x03E8;
const RT_DOCUMENT_ATOM: u16 = 0x03E9;
const RT_SLIDE: u16 = 0x03EE;
const RT_SLIDE_ATOM: u16 = 0x03EF;
const RT_NOTES: u16 = 0x03F0;
const RT_MAIN_MASTER: u16 = 0x03F8;
const RT_SS_SLIDE_INFO_ATOM: u16 = 0x03F9;
const RT_PP_DRAWING: u16 = 0x040C;
const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
const RT_SLIDE_PERSIST_ATOM: u16 = 0x03F3;
const RT_TEXT_HEADER_ATOM: u16 = 0x0F9F;
const RT_TEXT_CHARS_ATOM: u16 = 0x0FA0;
const RT_TEXT_BYTES_ATOM: u16 = 0x0FA8;
const RT_CSTRING: u16 = 0x0FBA;
const RT_OE_PLACEHOLDER_ATOM: u16 = 0x0BC3;
const RT_CRYPT_SESSION_10_CONTAINER: u16 = 0x2F14;

// [MS-ODRAW] Escher record types
const OA_DG_CONTAINER: u16 = 0xF002;
const OA_SPGR_CONTAINER: u16 = 0xF003;
const OA_SP_CONTAINER: u16 = 0xF004;
const OA_SP: u16 = 0xF00A;
const OA_FOPT: u16 = 0xF00B;
const OA_CLIENT_TEXTBOX: u16 = 0xF00D;
const OA_CHILD_ANCHOR: u16 = 0xF00F;
const OA_CLIENT_ANCHOR: u16 = 0xF010;
const OA_CLIENT_DATA: u16 = 0xF011;

/// Encrypted `CurrentUserAtom` magic token (`[MS-PPT]` §2.4.2).
const ENCRYPTED_CURRENT_USER_TOKEN: u32 = 0xF3D1_C4DF;

/// Check whether `bytes` is an OLE2 Compound File containing a `"PowerPoint Document"` stream.
#[must_use]
pub fn is_ppt_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 8 || &bytes[0..8] != b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1" {
        return false;
    }
    let Ok(comp) = cfb::CompoundFile::open(Cursor::new(bytes)) else {
        return false;
    };
    comp.exists("/PowerPoint Document") || comp.exists("PowerPoint Document")
}

/// Parse a legacy binary `.ppt` file from `bytes` into a [`Presentation`].
///
/// # Errors
///
/// Returns [`Error::Encrypted`] if the presentation is encrypted,
/// [`Error::MemoryLimitExceeded`] if a stream exceeds `max_stream_bytes`, or
/// [`Error::ParseError`] if the OLE container or record stream is malformed.
pub fn parse_ppt(bytes: &[u8], max_stream_bytes: usize) -> Result<Presentation> {
    let mut comp = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|e| Error::ParseError(format!("Invalid PPT OLE container: {e}")))?;

    let mut pres = Presentation::default();

    // Check VBA storage
    if comp.exists("/VBA") || comp.exists("/_VBA_PROJECT_CUR") {
        pres.forensic.vba_project_present = true;
    }

    // Check Current User stream for encryption token
    if let Ok(cu_bytes) = read_cfb_stream(&mut comp, "/Current User", max_stream_bytes)
        && cu_bytes.len() >= 12
    {
        let token = u32::from_le_bytes([cu_bytes[8], cu_bytes[9], cu_bytes[10], cu_bytes[11]]);
        if token == ENCRYPTED_CURRENT_USER_TOKEN {
            return Err(Error::Encrypted(
                "Encrypted PowerPoint presentation".to_string(),
            ));
        }
    }

    // Read Pictures stream if present
    let pictures = if let Ok(pic_bytes) = read_cfb_stream(&mut comp, "/Pictures", max_stream_bytes)
    {
        parse_pictures_stream(&pic_bytes)
    } else {
        Vec::new()
    };
    for (idx, (mime, data)) in pictures.into_iter().enumerate() {
        let res_id = format!("ppt_blip_{}", idx + 1);
        let (width, height) = image::ImageReader::new(Cursor::new(&data))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok())
            .unwrap_or((100, 100));
        pres.images.insert(
            res_id.clone(),
            ImageResource {
                id: res_id,
                mime_type: mime,
                data: Some(data),
                url: None,
                width,
                height,
            },
        );
    }

    // Read PowerPoint Document stream
    let doc_stream = read_cfb_stream(&mut comp, "/PowerPoint Document", max_stream_bytes)?;
    parse_ppt_document_stream(&doc_stream, &mut pres)?;

    Ok(pres)
}

fn read_cfb_stream(
    comp: &mut cfb::CompoundFile<Cursor<&[u8]>>,
    path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut stream = comp
        .open_stream(path)
        .or_else(|_| comp.open_stream(path.trim_start_matches('/')))
        .map_err(|e| Error::ParseError(format!("Missing PPT stream {path}: {e}")))?;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|e| Error::ParseError(format!("Read error in {path}: {e}")))?;
        if n == 0 {
            break;
        }
        if out.len().saturating_add(n) > max_bytes {
            return Err(Error::MemoryLimitExceeded {
                used: out.len().saturating_add(n),
                limit: max_bytes,
            });
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
struct RecordHeader {
    rec_ver: u8,
    rec_instance: u16,
    rec_type: u16,
    rec_len: usize,
}

fn read_record_header(data: &[u8], pos: usize) -> Option<(RecordHeader, usize)> {
    let slice = data.get(pos..pos.checked_add(8)?)?;
    let ver_inst = u16::from_le_bytes([slice[0], slice[1]]);
    let rec_ver = (ver_inst & 0x000F) as u8;
    let rec_instance = ver_inst >> 4;
    let rec_type = u16::from_le_bytes([slice[2], slice[3]]);
    let rec_len = u32::from_le_bytes([slice[4], slice[5], slice[6], slice[7]]) as usize;
    let payload_end = (pos + 8).checked_add(rec_len)?;
    if payload_end > data.len() {
        return None;
    }
    Some((
        RecordHeader {
            rec_ver,
            rec_instance,
            rec_type,
            rec_len,
        },
        pos + 8,
    ))
}

fn parse_ppt_document_stream(data: &[u8], pres: &mut Presentation) -> Result<()> {
    let mut pos = 0usize;
    let mut slide_list_texts: Vec<Vec<String>> = Vec::new();

    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        match hdr.rec_type {
            RT_CRYPT_SESSION_10_CONTAINER => {
                return Err(Error::Encrypted(
                    "Encrypted PowerPoint presentation".to_string(),
                ));
            }
            RT_DOCUMENT => {
                parse_document_container(payload, pres, &mut slide_list_texts)?;
            }
            RT_MAIN_MASTER => {
                let master_idx = pres.masters.len() + 1;
                let master = parse_master_container(payload, &format!("master_{master_idx}"));
                pres.masters.insert(master.id.clone(), master);
            }
            RT_SLIDE => {
                #[allow(clippy::cast_possible_truncation)]
                let slide_num = (pres.slides.len() + 1) as u32;
                let mut slide = parse_slide_container(payload, slide_num, pres);
                if slide.hidden {
                    pres.forensic.hidden_slide_numbers.push(slide_num);
                }
                // If the slide had no text inside its Escher shapes, populate from SlideListWithText
                let has_text = slide.shapes.iter().any(shape_has_nonempty_text);
                if !has_text
                    && let Some(fallback_texts) = slide_list_texts.get((slide_num - 1) as usize)
                {
                    for (idx, txt) in fallback_texts.iter().enumerate() {
                        if !txt.trim().is_empty() {
                            #[allow(clippy::cast_precision_loss)]
                            let y_pt = 48.0 + (idx as f32) * 90.0;
                            slide.shapes.push(SlideShape {
                                id: format!("slwt_{idx}"),
                                name: format!("Text {idx}"),
                                x_pt: 48.0,
                                y_pt,
                                width_pt: (pres.slide_width_pt - 96.0).max(144.0),
                                height_pt: 72.0,
                                has_explicit_xfrm: true,
                                kind: ShapeKind::AutoShape {
                                    text_body: Some(text_string_to_body(
                                        txt,
                                        if idx == 0 { 28.0 } else { 18.0 },
                                    )),
                                },
                                ..SlideShape::default()
                            });
                        }
                    }
                }
                pres.slides.push(slide);
            }
            RT_NOTES => {
                let notes = parse_notes_container(payload);
                if !notes.plain_text.is_empty()
                    && let Some(last_slide) = pres.slides.last_mut()
                    && last_slide.notes.is_none()
                {
                    last_slide.notes = Some(notes);
                }
            }
            _ => {
                // Also scan inside any container for CryptSession10Container
                if hdr.rec_ver == 0x0F && contains_encryption_record(payload) {
                    return Err(Error::Encrypted(
                        "Encrypted PowerPoint presentation".to_string(),
                    ));
                }
            }
        }
        pos = payload_start + hdr.rec_len;
    }

    Ok(())
}

fn contains_encryption_record(data: &[u8]) -> bool {
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        if hdr.rec_type == RT_CRYPT_SESSION_10_CONTAINER {
            return true;
        }
        pos = payload_start + hdr.rec_len;
    }
    false
}

fn shape_has_nonempty_text(shape: &SlideShape) -> bool {
    match &shape.kind {
        ShapeKind::AutoShape {
            text_body: Some(tb),
        } => tb
            .paragraphs
            .iter()
            .any(|p| p.runs.iter().any(|r| !r.text.trim().is_empty())),
        ShapeKind::Group { children, .. } => children.iter().any(shape_has_nonempty_text),
        _ => false,
    }
}

fn parse_document_container(
    data: &[u8],
    pres: &mut Presentation,
    slide_list_texts: &mut Vec<Vec<String>>,
) -> Result<()> {
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        match hdr.rec_type {
            RT_CRYPT_SESSION_10_CONTAINER => {
                return Err(Error::Encrypted(
                    "Encrypted PowerPoint presentation".to_string(),
                ));
            }
            RT_DOCUMENT_ATOM => {
                if payload.len() >= 8 {
                    let sx = i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    let sy = i32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
                    #[allow(clippy::cast_precision_loss)]
                    if sx > 0 && sy > 0 {
                        pres.slide_width_pt = (sx as f32 / MASTER_UNITS_PER_PT).clamp(72.0, 5000.0);
                        pres.slide_height_pt =
                            (sy as f32 / MASTER_UNITS_PER_PT).clamp(72.0, 5000.0);
                    }
                }
                if payload.len() >= 16 {
                    let nx = i32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
                    let ny =
                        i32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]);
                    #[allow(clippy::cast_precision_loss)]
                    if nx > 0 && ny > 0 {
                        pres.notes_width_pt = (nx as f32 / MASTER_UNITS_PER_PT).clamp(72.0, 5000.0);
                        pres.notes_height_pt =
                            (ny as f32 / MASTER_UNITS_PER_PT).clamp(72.0, 5000.0);
                    }
                }
            }
            RT_SLIDE_LIST_WITH_TEXT if hdr.rec_instance == 0 => {
                // Instance 0 is the slide text list
                parse_slide_list_with_text(payload, slide_list_texts);
            }
            _ => {}
        }
        pos = payload_start + hdr.rec_len;
    }
    Ok(())
}

fn parse_slide_list_with_text(data: &[u8], out: &mut Vec<Vec<String>>) {
    let mut pos = 0usize;
    let mut current_slide_texts: Option<Vec<String>> = None;

    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        match hdr.rec_type {
            RT_SLIDE_PERSIST_ATOM => {
                if let Some(prev) = current_slide_texts.take() {
                    out.push(prev);
                }
                current_slide_texts = Some(Vec::new());
            }
            RT_TEXT_CHARS_ATOM => {
                let txt = decode_utf16le(payload);
                if let Some(ref mut cur) = current_slide_texts {
                    cur.push(txt);
                }
            }
            RT_TEXT_BYTES_ATOM => {
                let txt = decode_latin1(payload);
                if let Some(ref mut cur) = current_slide_texts {
                    cur.push(txt);
                }
            }
            _ => {}
        }
        pos = payload_start + hdr.rec_len;
    }
    if let Some(last) = current_slide_texts {
        out.push(last);
    }
}

fn parse_master_container(data: &[u8], id: &str) -> SlideMaster {
    let mut master = SlideMaster {
        id: id.to_string(),
        ..SlideMaster::default()
    };
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        if hdr.rec_type == RT_PP_DRAWING {
            let (bg, shapes) = parse_pp_drawing(payload);
            if bg != ShapeFill::Inherited {
                master.background = bg;
            }
            master.shapes.extend(shapes);
        }
        pos = payload_start + hdr.rec_len;
    }
    master
}

fn parse_slide_container(data: &[u8], slide_num: u32, pres: &Presentation) -> Slide {
    let mut slide = Slide {
        number: slide_num,
        id: format!("slide_{slide_num}"),
        master_id: pres.masters.keys().next().cloned(),
        ..Slide::default()
    };
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        match hdr.rec_type {
            RT_SLIDE_ATOM => {
                if payload.len() >= 14 {
                    let flags = u16::from_le_bytes([payload[12], payload[13]]);
                    slide.show_master_shapes = (flags & 0x0001) != 0;
                }
            }
            RT_SS_SLIDE_INFO_ATOM => {
                if payload.len() >= 14 {
                    let flags = payload[12];
                    if (flags & 0x04) != 0 || (flags & 0x10) != 0 {
                        slide.hidden = true;
                    }
                }
            }
            RT_PP_DRAWING => {
                let (bg, shapes) = parse_pp_drawing(payload);
                if bg != ShapeFill::Inherited {
                    slide.background = bg;
                }
                slide.shapes.extend(shapes);
            }
            _ => {}
        }
        pos = payload_start + hdr.rec_len;
    }
    slide
}

fn parse_notes_container(data: &[u8]) -> NotesSlide {
    let mut notes = NotesSlide::default();
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        if hdr.rec_type == RT_PP_DRAWING {
            let (_, shapes) = parse_pp_drawing(payload);
            for s in &shapes {
                if let ShapeKind::AutoShape {
                    text_body: Some(ref tb),
                } = s.kind
                {
                    for p in &tb.paragraphs {
                        for r in &p.runs {
                            notes.plain_text.push_str(&r.text);
                        }
                        notes.plain_text.push('\n');
                    }
                }
            }
            notes.shapes.extend(shapes);
        }
        pos = payload_start + hdr.rec_len;
    }
    notes.plain_text = notes.plain_text.trim().to_string();
    notes
}

fn parse_pp_drawing(data: &[u8]) -> (ShapeFill, Vec<SlideShape>) {
    let mut bg = ShapeFill::Inherited;
    let mut shapes = Vec::new();
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        if hdr.rec_type == OA_DG_CONTAINER {
            parse_dg_container(payload, &mut bg, &mut shapes);
        }
        pos = payload_start + hdr.rec_len;
    }
    (bg, shapes)
}

fn parse_dg_container(data: &[u8], bg: &mut ShapeFill, shapes: &mut Vec<SlideShape>) {
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        match hdr.rec_type {
            OA_SPGR_CONTAINER => {
                parse_spgr_container(payload, shapes, true);
            }
            OA_SP_CONTAINER => {
                // Background shape in OfficeArtDgContainer
                if let Some(bg_shape) = parse_sp_container(payload)
                    && bg_shape.fill != ShapeFill::Inherited
                {
                    *bg = bg_shape.fill;
                }
            }
            _ => {}
        }
        pos = payload_start + hdr.rec_len;
    }
}

fn parse_spgr_container(data: &[u8], shapes: &mut Vec<SlideShape>, is_patriarch_group: bool) {
    let mut pos = 0usize;
    let mut first_sp = true;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        match hdr.rec_type {
            OA_SP_CONTAINER => {
                if is_patriarch_group && first_sp {
                    // The first SpContainer inside the root SpgrContainer is the patriarch shape (group frame)
                    first_sp = false;
                } else if let Some(s) = parse_sp_container(payload) {
                    shapes.push(s);
                }
            }
            OA_SPGR_CONTAINER => {
                let mut children = Vec::new();
                parse_spgr_container(payload, &mut children, false);
                if !children.is_empty() {
                    shapes.push(SlideShape {
                        id: format!("grp_{}", shapes.len()),
                        name: "Group".to_string(),
                        width_pt: DEFAULT_SLIDE_WIDTH_PT,
                        height_pt: DEFAULT_SLIDE_HEIGHT_PT,
                        kind: ShapeKind::Group {
                            ch_off_x_pt: 0.0,
                            ch_off_y_pt: 0.0,
                            ch_ext_w_pt: DEFAULT_SLIDE_WIDTH_PT,
                            ch_ext_h_pt: DEFAULT_SLIDE_HEIGHT_PT,
                            children,
                        },
                        ..SlideShape::default()
                    });
                }
            }
            _ => {}
        }
        pos = payload_start + hdr.rec_len;
    }
}

fn parse_sp_container(data: &[u8]) -> Option<SlideShape> {
    let mut shape = SlideShape {
        preset_geom: Some("rect".to_string()),
        ..SlideShape::default()
    };
    let mut is_patriarch = false;
    let mut blip_id: Option<u32> = None;
    let mut text_body: Option<TextBody> = None;

    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        match hdr.rec_type {
            OA_SP => {
                if payload.len() >= 8 {
                    let shape_id =
                        u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    let flags =
                        u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
                    shape.id = shape_id.to_string();
                    is_patriarch = (flags & 0x0004) != 0;
                    shape.flip_h = (flags & 0x0040) != 0;
                    shape.flip_v = (flags & 0x0080) != 0;
                    shape.preset_geom =
                        Some(escher_shape_type_to_preset(hdr.rec_instance).to_string());
                }
            }
            OA_FOPT => {
                parse_fopt(payload, hdr.rec_instance, &mut shape, &mut blip_id);
            }
            OA_CLIENT_ANCHOR | OA_CHILD_ANCHOR => {
                if let Some((x, y, w, h)) = parse_escher_anchor(payload) {
                    shape.x_pt = x;
                    shape.y_pt = y;
                    shape.width_pt = w;
                    shape.height_pt = h;
                    shape.has_explicit_xfrm = true;
                }
            }
            OA_CLIENT_DATA => {
                if let Some(ph) = parse_client_data_placeholder(payload) {
                    shape.placeholder = Some(ph);
                }
            }
            OA_CLIENT_TEXTBOX => {
                let tb = parse_client_textbox(payload);
                if !tb.paragraphs.is_empty() {
                    text_body = Some(tb);
                }
            }
            _ => {}
        }
        pos = payload_start + hdr.rec_len;
    }

    if is_patriarch {
        return None;
    }

    if let Some(bid) = blip_id {
        shape.kind = ShapeKind::Picture {
            resource_id: format!("ppt_blip_{bid}"),
            crop: None,
            opacity: 1.0,
        };
    } else {
        shape.kind = ShapeKind::AutoShape { text_body };
    }

    Some(shape)
}

fn escher_shape_type_to_preset(spt: u16) -> &'static str {
    match spt {
        2 => "roundRect",
        3 => "ellipse",
        4 => "diamond",
        5 => "triangle",
        6 => "rtTriangle",
        7 => "parallelogram",
        8 => "trapezoid",
        9 => "hexagon",
        10 => "octagon",
        13 | 55 => "rightArrow",
        20 => "line",
        _ => "rect",
    }
}

fn parse_fopt(payload: &[u8], prop_count: u16, shape: &mut SlideShape, blip_id: &mut Option<u32>) {
    let mut fill_color: Option<Rgba> = None;
    let mut no_fill = false;
    let mut line_color: Option<Rgba> = None;
    let mut line_width_pt = 0.75_f32;
    let mut no_line = false;

    for i in 0..usize::from(prop_count) {
        let Some(entry) = payload.get(i * 6..(i + 1) * 6) else {
            break;
        };
        let prop_raw = u16::from_le_bytes([entry[0], entry[1]]);
        let prop_id = prop_raw & 0x3FFF;
        let op = u32::from_le_bytes([entry[2], entry[3], entry[4], entry[5]]);
        match prop_id {
            0x0004 => {
                // rotation in 16.16 fixed-point degrees
                #[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss)]
                {
                    shape.rotation_deg = (op as i32) as f32 / 65_536.0;
                }
            }
            0x0104 => {
                if op > 0 {
                    *blip_id = Some(op);
                }
            }
            0x0181 => {
                fill_color = decode_officeart_color(op);
            }
            0x01BF => {
                // fNoFillHitTest: bit 4 (0x10) is fFilled, bit 20 (0x10_0000) is fUsefFilled
                if (op & 0x0010_0000) != 0 && (op & 0x0000_0010) == 0 {
                    no_fill = true;
                }
            }
            0x01C0 => {
                line_color = decode_officeart_color(op);
            }
            0x01CB => {
                #[allow(clippy::cast_precision_loss)]
                {
                    line_width_pt = (op as f32 / 12_700.0).clamp(0.25, 24.0);
                }
            }
            // fNoLineDrawDash: bit 3 (0x08) is fLine, bit 19 (0x08_0000) is fUsefLine
            0x01FF if (op & 0x0008_0000) != 0 && (op & 0x0000_0008) == 0 => {
                no_line = true;
            }
            _ => {}
        }
    }

    if no_fill {
        shape.fill = ShapeFill::NoFill;
    } else if let Some(c) = fill_color {
        shape.fill = ShapeFill::Solid(c);
    }

    if !no_line && let Some(lc) = line_color {
        shape.stroke = Some(ShapeStroke {
            color: lc,
            width_pt: line_width_pt,
            dash: None,
        });
    }
}

fn decode_officeart_color(op: u32) -> Option<Rgba> {
    let flag = (op >> 24) & 0xFF;
    // Flag 0x08 represents a scheme-indexed color; direct RGB has flag 0x00 or 0x02
    if (flag & 0x08) != 0 {
        return None;
    }
    let r = (op & 0xFF) as u8;
    let g = ((op >> 8) & 0xFF) as u8;
    let b = ((op >> 16) & 0xFF) as u8;
    Some(Rgba::rgb(r, g, b))
}

fn parse_escher_anchor(payload: &[u8]) -> Option<(f32, f32, f32, f32)> {
    #[allow(clippy::cast_precision_loss)]
    if payload.len() == 8 {
        // SmallRectStruct: top, left, right, bottom (i16 in master units)
        let top = f32::from(i16::from_le_bytes([payload[0], payload[1]])) / MASTER_UNITS_PER_PT;
        let left = f32::from(i16::from_le_bytes([payload[2], payload[3]])) / MASTER_UNITS_PER_PT;
        let right = f32::from(i16::from_le_bytes([payload[4], payload[5]])) / MASTER_UNITS_PER_PT;
        let bottom = f32::from(i16::from_le_bytes([payload[6], payload[7]])) / MASTER_UNITS_PER_PT;
        Some((left, top, (right - left).max(1.0), (bottom - top).max(1.0)))
    } else if payload.len() >= 16 {
        // RectStruct: either (top, left, right, bottom) or (left, top, right, bottom)
        let a0 = i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as f32;
        let a1 = i32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]) as f32;
        let a2 = i32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]) as f32;
        let a3 = i32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]) as f32;
        // In [MS-PPT] PointStruct/RectStruct: top, left, right, bottom in master units
        let top = a0 / MASTER_UNITS_PER_PT;
        let left = a1 / MASTER_UNITS_PER_PT;
        let right = a2 / MASTER_UNITS_PER_PT;
        let bottom = a3 / MASTER_UNITS_PER_PT;
        Some((left, top, (right - left).max(1.0), (bottom - top).max(1.0)))
    } else {
        None
    }
}

fn parse_client_data_placeholder(data: &[u8]) -> Option<PlaceholderRef> {
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        if hdr.rec_type == RT_OE_PLACEHOLDER_ATOM && payload.len() >= 6 {
            let placement_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let ph_id = payload[4];
            let ph_type = match ph_id {
                1 | 13 => Some("title".to_string()),
                2 | 14 => Some("body".to_string()),
                3 | 15 => Some("ctrTitle".to_string()),
                4 | 16 => Some("subTitle".to_string()),
                7 => Some("dt".to_string()),
                8 => Some("sldNum".to_string()),
                9 => Some("ftr".to_string()),
                _ => None,
            };
            return Some(PlaceholderRef {
                ph_type,
                idx: if placement_id < 64 {
                    Some(placement_id)
                } else {
                    None
                },
            });
        }
        pos = payload_start + hdr.rec_len;
    }
    None
}

fn parse_client_textbox(data: &[u8]) -> TextBody {
    let mut tb = TextBody::default();
    let mut pos = 0usize;
    let mut current_header_type: u32 = 1; // 0 = Title, 1 = Body

    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        match hdr.rec_type {
            RT_TEXT_HEADER_ATOM => {
                if payload.len() >= 4 {
                    current_header_type =
                        u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                }
            }
            RT_TEXT_CHARS_ATOM => {
                let txt = decode_utf16le(payload);
                let sz = if current_header_type == 0 || current_header_type == 6 {
                    28.0
                } else {
                    18.0
                };
                append_text_to_body(&mut tb, &txt, sz);
            }
            RT_TEXT_BYTES_ATOM => {
                let txt = decode_latin1(payload);
                let sz = if current_header_type == 0 || current_header_type == 6 {
                    28.0
                } else {
                    18.0
                };
                append_text_to_body(&mut tb, &txt, sz);
            }
            RT_CSTRING => {
                let txt = decode_utf16le(payload);
                append_text_to_body(&mut tb, &txt, 18.0);
            }
            _ => {}
        }
        pos = payload_start + hdr.rec_len;
    }

    tb
}

fn text_string_to_body(txt: &str, font_size_pt: f32) -> TextBody {
    let mut tb = TextBody::default();
    append_text_to_body(&mut tb, txt, font_size_pt);
    tb
}

fn append_text_to_body(tb: &mut TextBody, txt: &str, font_size_pt: f32) {
    for line in txt.split(['\r', '\n']) {
        tb.paragraphs.push(TextParagraph {
            props: TextLevelStyle::default(),
            runs: vec![PptTextRun {
                text: line.to_string(),
                style: TextRunStyle {
                    font_size_pt: Some(font_size_pt),
                    ..TextRunStyle::default()
                },
                field_type: None,
            }],
        });
    }
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn decode_utf16le(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    String::from_utf16_lossy(&units)
}

fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

fn parse_pictures_stream(data: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some((hdr, payload_start)) = read_record_header(data, pos) {
        let payload = &data[payload_start..payload_start + hdr.rec_len];
        // BLIP records: 0xF01A..=0xF029
        // Standard header inside BLIP is 17 bytes (16-byte UID + 1-byte tag), or 33 bytes if two UIDs
        let skip = if (hdr.rec_instance & 0x0001) != 0 {
            33
        } else {
            17
        };
        if payload.len() > skip {
            let img_bytes = payload[skip..].to_vec();
            let mime = match hdr.rec_type {
                0xF01A => Some("image/x-emf"),
                0xF01B => Some("image/x-wmf"),
                0xF01D | 0xF02A => Some("image/jpeg"),
                0xF01E => Some("image/png"),
                0xF01F => Some("image/bmp"),
                _ => None,
            };
            if let Some(m) = mime {
                out.push((m.to_string(), img_bytes));
            }
        }
        pos = payload_start + hdr.rec_len;
    }
    out
}
