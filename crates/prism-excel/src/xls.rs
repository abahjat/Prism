// SPDX-License-Identifier: AGPL-3.0-only
//! Pure-Rust legacy `Excel` 97–2003 binary BIFF8 reader (`[MS-XLS]`) producing a [`Workbook`].

use crate::model::{
    CellHAlign, CellRange, CellVAlign, CellValue, CellXf, ColumnSpan, DefinedName, ExcelBorder,
    ExcelFill, ExcelFont, FormulaInfo, PageOrientation, PaneInfo, RowProps, Sheet, SheetComment,
    SheetVisibility, Workbook,
};
use crate::xlsx::{biff_indexed_color, excel_chars_to_points, finalize_cell};
use cfb::CompoundFile;
use prism_core::error::{Error, Result};
use prism_numfmt::{DateSystem, FormatOptions};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

/// Default maximum stream size for legacy `.xls` files (`64 MiB`).
pub const DEFAULT_MAX_XLS_STREAM_BYTES: usize = 64 * 1024 * 1024;

// BIFF8 Record Types ([MS-XLS])
const REC_FORMULA: u16 = 0x0006;
const REC_EOF: u16 = 0x000A;
const REC_HEADER: u16 = 0x0014;
const REC_FOOTER: u16 = 0x0015;
const REC_LBL: u16 = 0x0018;
const REC_PROTECT: u16 = 0x0012;
const REC_DATEMODE: u16 = 0x0022;
const REC_LEFT_MARGIN: u16 = 0x0026;
const REC_RIGHT_MARGIN: u16 = 0x0027;
const REC_TOP_MARGIN: u16 = 0x0028;
const REC_BOTTOM_MARGIN: u16 = 0x0029;
const REC_FILEPASS: u16 = 0x002F;
const REC_FONT: u16 = 0x0031;
const REC_NOTE: u16 = 0x001C;
const REC_CONTINUE: u16 = 0x003C;
const REC_OBJ: u16 = 0x005D;
const REC_TXO: u16 = 0x01B6;
const REC_PANE: u16 = 0x0041;
const REC_COLINFO: u16 = 0x007D;
const REC_BOUNDSHEET8: u16 = 0x0085;
const REC_SETUP: u16 = 0x00A1;
const REC_MULRK: u16 = 0x00BD;
const REC_MULBLANK: u16 = 0x00BE;
const REC_XF: u16 = 0x00E0;
const REC_MERGEDCELLS: u16 = 0x00E5;
const REC_SST: u16 = 0x00FC;
const REC_LABELSST: u16 = 0x00FD;
const REC_BLANK: u16 = 0x0201;
const REC_NUMBER: u16 = 0x0203;
const REC_LABEL: u16 = 0x0204;
const REC_BOOLERR: u16 = 0x0205;
const REC_STRING: u16 = 0x0207;
const REC_ROW: u16 = 0x0208;
const REC_RK: u16 = 0x027E;
const REC_FORMAT: u16 = 0x041E;
const REC_BOF: u16 = 0x0809;

/// Check whether `bytes` is an OLE2 Compound File containing a `Workbook` or `Book` stream.
#[must_use]
pub fn is_xls_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 8 || &bytes[0..8] != b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1" {
        return false;
    }
    let Ok(comp) = CompoundFile::open(Cursor::new(bytes)) else {
        return false;
    };
    comp.exists("/Workbook")
        || comp.exists("Workbook")
        || comp.exists("/Book")
        || comp.exists("Book")
}

/// Parse a legacy BIFF8 `.xls` file from `bytes` into a sparse [`Workbook`].
///
/// # Errors
///
/// Returns [`Error::Encrypted`] if a `FILEPASS` (`0x002F`) record is encountered,
/// [`Error::MemoryLimitExceeded`] if the stream exceeds `max_stream_bytes`, or
/// [`Error::ParseError`] if the CFB container or BIFF stream is invalid.
#[allow(clippy::too_many_lines)]
pub fn parse_xls(bytes: &[u8], max_stream_bytes: usize) -> Result<Workbook> {
    let cursor = Cursor::new(bytes);
    let mut comp = CompoundFile::open(cursor)
        .map_err(|e| Error::ParseError(format!("Invalid OLE2 container for XLS: {e}")))?;

    let mut wb = Workbook::default();
    if comp.exists("/_VBA_PROJECT_CUR")
        || comp.exists("_VBA_PROJECT_CUR")
        || comp.exists("/VBA")
        || comp.exists("VBA")
    {
        wb.forensic.vba_project_present = true;
    }

    let stream_path = if comp.exists("/Workbook") {
        "/Workbook"
    } else if comp.exists("Workbook") {
        "Workbook"
    } else if comp.exists("/Book") {
        "/Book"
    } else if comp.exists("Book") {
        "Book"
    } else {
        return Err(Error::ParseError(
            "Missing Workbook/Book stream in XLS container".to_string(),
        ));
    };

    let data = read_cfb_stream(&mut comp, stream_path, max_stream_bytes)?;
    let records = collect_biff_records(&data)?;

    // First pass: Workbook globals (before first Worksheet BOF)
    let mut sst: Vec<String> = Vec::new();
    let mut sheet_defs: Vec<(String, SheetVisibility)> = Vec::new();
    let mut in_globals = false;
    let mut idx = 0usize;

    while idx < records.len() {
        let (rec_type, payload) = &records[idx];
        match *rec_type {
            REC_BOF => {
                let dt = if payload.len() >= 4 {
                    u16::from_le_bytes([payload[2], payload[3]])
                } else {
                    0
                };
                if dt == 0x0005 {
                    in_globals = true;
                } else if in_globals {
                    // End of globals
                    break;
                }
            }
            REC_EOF if in_globals => {
                break;
            }
            REC_FILEPASS => {
                return Err(Error::Encrypted(
                    "Legacy XLS workbook is encrypted (FILEPASS record 0x002F)".to_string(),
                ));
            }
            REC_PROTECT if in_globals => {
                if payload.len() >= 2 && u16::from_le_bytes([payload[0], payload[1]]) == 1 {
                    wb.forensic.workbook_protected = true;
                }
            }
            REC_DATEMODE => {
                if payload.len() >= 2 && u16::from_le_bytes([payload[0], payload[1]]) == 1 {
                    wb.date_system = DateSystem::Date1904;
                }
            }
            REC_BOUNDSHEET8 => {
                if let Some((name, vis)) = parse_boundsheet8(payload) {
                    match vis {
                        SheetVisibility::Visible => {
                            wb.forensic.visible_sheet_names.push(name.clone());
                        }
                        SheetVisibility::Hidden => {
                            wb.forensic.hidden_sheet_names.push(name.clone());
                        }
                        SheetVisibility::VeryHidden => {
                            wb.forensic.very_hidden_sheet_names.push(name.clone());
                        }
                    }
                    sheet_defs.push((name, vis));
                }
            }
            REC_FORMAT => {
                if let Some((fmt_id, code)) = parse_biff_format(payload) {
                    wb.styles.num_fmts.insert(fmt_id, code);
                }
            }
            REC_FONT => {
                if let Some(font) = parse_biff_font(payload) {
                    // BIFF8 font table skips index 4: insert a placeholder at index 4 so 0-based Vec index matches ifnt!
                    if wb.styles.fonts.len() == 4 {
                        wb.styles.fonts.push(ExcelFont::default());
                    }
                    wb.styles.fonts.push(font);
                }
            }
            REC_XF => {
                if let Some(xf) = parse_biff_xf(payload, &mut wb.styles.fills) {
                    wb.styles.cell_xfs.push(xf);
                }
            }
            REC_LBL => {
                if let Some(dn) = parse_biff_lbl(payload) {
                    wb.defined_names.push(dn);
                }
            }
            REC_SST => {
                // Collect SST + any immediately following CONTINUE (0x003C) records
                let mut continuations: Vec<&[u8]> = vec![payload.as_slice()];
                while idx + 1 < records.len() && records[idx + 1].0 == REC_CONTINUE {
                    idx += 1;
                    continuations.push(records[idx].1.as_slice());
                }
                sst = parse_biff8_sst(&continuations);
            }
            _ => {}
        }
        idx += 1;
    }

    if wb.styles.fonts.is_empty() {
        wb.styles.fonts.push(ExcelFont::default());
    }
    if wb.styles.cell_xfs.is_empty() {
        wb.styles.cell_xfs.push(CellXf::default());
    }
    wb.forensic.defined_names_count = wb.defined_names.len();

    let fmt_opts = FormatOptions {
        date_system: wb.date_system,
        ..FormatOptions::default()
    };

    // Second pass: Worksheet substreams
    let mut sheet_cursor = 0usize;
    let mut cur_sheet: Option<Sheet> = None;
    let mut sub_bof_depth: usize = 0;
    let mut pending_formula_cell: Option<(u32, u32, u32)> = None;
    // Cell comments: an `OBJ` (its id) is followed by a `TXO` whose text is in the next
    // `CONTINUE` records; the `NOTE` records at the end of the sheet point back to that id.
    let mut last_obj_id: Option<u16> = None;
    let mut note_texts: BTreeMap<u16, String> = BTreeMap::new();

    while idx < records.len() {
        let (rec_type, payload) = &records[idx];
        match *rec_type {
            REC_FILEPASS => {
                return Err(Error::Encrypted(
                    "Legacy XLS worksheet is encrypted (FILEPASS record 0x002F)".to_string(),
                ));
            }
            REC_BOF => {
                let dt = if payload.len() >= 4 {
                    u16::from_le_bytes([payload[2], payload[3]])
                } else {
                    0
                };
                if cur_sheet.is_none() && dt == 0x0010 {
                    let (name, visibility) =
                        sheet_defs.get(sheet_cursor).cloned().unwrap_or_else(|| {
                            (
                                format!("Sheet{}", sheet_cursor + 1),
                                SheetVisibility::Visible,
                            )
                        });
                    #[allow(clippy::cast_possible_truncation)]
                    let sheet_id = (sheet_cursor + 1) as u32;
                    sheet_cursor += 1;
                    cur_sheet = Some(Sheet {
                        name,
                        sheet_id,
                        visibility,
                        ..Sheet::default()
                    });
                    sub_bof_depth = 0;
                    last_obj_id = None;
                    note_texts.clear();
                } else if cur_sheet.is_some() {
                    sub_bof_depth += 1;
                }
            }
            REC_EOF => {
                if cur_sheet.is_some() {
                    if sub_bof_depth > 0 {
                        sub_bof_depth -= 1;
                    } else if let Some(done_sheet) = cur_sheet.take() {
                        wb.sheets.push(done_sheet);
                    }
                }
            }
            REC_OBJ if cur_sheet.is_some() && sub_bof_depth == 0 => {
                // First subrecord is `ftCmo` (0x0015): ft, cb, ot, id.
                if payload.len() >= 8 && u16::from_le_bytes([payload[0], payload[1]]) == 0x0015 {
                    last_obj_id = Some(u16::from_le_bytes([payload[6], payload[7]]));
                }
            }
            REC_TXO if cur_sheet.is_some() && sub_bof_depth == 0 => {
                let text = read_txo_text(payload, &records[idx + 1..]);
                if let Some(id) = last_obj_id.take() {
                    note_texts.insert(id, text);
                }
            }
            REC_NOTE if sub_bof_depth == 0 => {
                if let Some(sheet) = cur_sheet.as_mut()
                    && payload.len() >= 8
                {
                    let row = u32::from(u16::from_le_bytes([payload[0], payload[1]]));
                    let col = u32::from(u16::from_le_bytes([payload[2], payload[3]]));
                    let obj_id = u16::from_le_bytes([payload[6], payload[7]]);
                    let author = read_xl_unicode_string(&payload[8..])
                        .map(|(s, _)| s)
                        .unwrap_or_default();
                    if !author.is_empty() && !wb.forensic.comment_authors.contains(&author) {
                        wb.forensic.comment_authors.push(author.clone());
                    }
                    sheet.comments.insert(
                        (row, col),
                        SheetComment {
                            author,
                            text: note_texts.remove(&obj_id).unwrap_or_default(),
                            replies: Vec::new(),
                        },
                    );
                }
            }
            _ if cur_sheet.is_some() && sub_bof_depth == 0 => {
                if let Some(sheet) = cur_sheet.as_mut() {
                    parse_worksheet_biff_record(
                        *rec_type,
                        payload,
                        &sst,
                        &fmt_opts,
                        &mut pending_formula_cell,
                        sheet,
                        &mut wb,
                    );
                }
            }
            _ => {}
        }
        idx += 1;
    }

    if let Some(done_sheet) = cur_sheet.take() {
        wb.sheets.push(done_sheet);
    }

    // If BOUNDSHEET8 declared sheets that had no parsed substream, include them as empty sheets
    while sheet_cursor < sheet_defs.len() {
        let (name, visibility) = sheet_defs[sheet_cursor].clone();
        #[allow(clippy::cast_possible_truncation)]
        let sheet_id = (sheet_cursor + 1) as u32;
        wb.sheets.push(Sheet {
            name,
            sheet_id,
            visibility,
            ..Sheet::default()
        });
        sheet_cursor += 1;
    }

    wb.styles.borders.push(ExcelBorder::default());
    Ok(wb)
}

#[allow(clippy::too_many_lines)]
fn parse_worksheet_biff_record(
    rec_type: u16,
    payload: &[u8],
    sst: &[String],
    fmt_opts: &FormatOptions,
    pending_formula_cell: &mut Option<(u32, u32, u32)>,
    sheet: &mut Sheet,
    wb: &mut Workbook,
) {
    match rec_type {
        REC_PROTECT => {
            if payload.len() >= 2 && u16::from_le_bytes([payload[0], payload[1]]) == 1 {
                sheet.protected = true;
                if !wb.forensic.protected_sheet_names.contains(&sheet.name) {
                    wb.forensic.protected_sheet_names.push(sheet.name.clone());
                }
            }
        }
        REC_COLINFO if payload.len() >= 10 => {
            let c1 = u32::from(u16::from_le_bytes([payload[0], payload[1]]));
            let c2 = u32::from(u16::from_le_bytes([payload[2], payload[3]])).max(c1);
            let coldx = f32::from(u16::from_le_bytes([payload[4], payload[5]])) / 256.0;
            let ixfe = u32::from(u16::from_le_bytes([payload[6], payload[7]]));
            let grbit = u16::from_le_bytes([payload[8], payload[9]]);
            let hidden = (grbit & 0x0001) != 0 || coldx <= 0.0;
            #[allow(clippy::cast_possible_truncation)]
            let outline_level = ((grbit >> 8) & 0x07) as u8;
            if hidden {
                wb.forensic.hidden_col_count += (c2 - c1 + 1) as usize;
            }
            sheet.col_spans.push(ColumnSpan {
                min_col: c1,
                max_col: c2,
                width_pt: excel_chars_to_points(coldx),
                hidden,
                style_idx: ixfe,
                outline_level,
            });
        }
        REC_ROW if payload.len() >= 16 => {
            let rw = u32::from(u16::from_le_bytes([payload[0], payload[1]]));
            let miy_rw = u16::from_le_bytes([payload[6], payload[7]]) & 0x7FFF;
            let height_pt = (f32::from(miy_rw) / 20.0).clamp(0.0, 409.0);
            let grbit = u32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]);
            let hidden = (grbit & 0x0020) != 0 || height_pt <= 0.0;
            let custom_height = (grbit & 0x0040) != 0;
            #[allow(clippy::cast_possible_truncation)]
            let outline_level = (grbit & 0x07) as u8;
            let style_idx = if (grbit & 0x0080) != 0 {
                Some((grbit >> 16) & 0x0FFF)
            } else {
                None
            };
            if hidden {
                wb.forensic.hidden_row_count += 1;
            }
            sheet.row_props.insert(
                rw,
                RowProps {
                    height_pt,
                    custom_height,
                    hidden,
                    style_idx,
                    outline_level,
                },
            );
        }
        REC_BLANK if payload.len() >= 6 => {
            let (r, c, ixfe) = read_cell_header(payload);
            let cell = finalize_cell(CellValue::Empty, None, ixfe, fmt_opts, wb);
            sheet.cells.insert((r, c), cell);
        }
        REC_MULBLANK if payload.len() >= 6 => {
            let r = u32::from(u16::from_le_bytes([payload[0], payload[1]]));
            let col_first = u32::from(u16::from_le_bytes([payload[2], payload[3]]));
            let col_last = u32::from(u16::from_le_bytes([
                payload[payload.len() - 2],
                payload[payload.len() - 1],
            ]));
            let mut pos = 4usize;
            let mut c = col_first;
            while c <= col_last && pos + 2 <= payload.len() - 2 {
                let ixfe = u32::from(u16::from_le_bytes([payload[pos], payload[pos + 1]]));
                let cell = finalize_cell(CellValue::Empty, None, ixfe, fmt_opts, wb);
                sheet.cells.insert((r, c), cell);
                pos += 2;
                c += 1;
            }
        }
        REC_NUMBER if payload.len() >= 14 => {
            let (r, c, ixfe) = read_cell_header(payload);
            let mut b = [0u8; 8];
            b.copy_from_slice(&payload[6..14]);
            let val = f64::from_le_bytes(b);
            let cell = finalize_cell(CellValue::Number(val), None, ixfe, fmt_opts, wb);
            sheet.cells.insert((r, c), cell);
        }
        REC_RK if payload.len() >= 10 => {
            let (r, c, ixfe) = read_cell_header(payload);
            let rk = u32::from_le_bytes([payload[6], payload[7], payload[8], payload[9]]);
            let val = decode_rk_number(rk);
            let cell = finalize_cell(CellValue::Number(val), None, ixfe, fmt_opts, wb);
            sheet.cells.insert((r, c), cell);
        }
        REC_MULRK if payload.len() >= 6 => {
            let r = u32::from(u16::from_le_bytes([payload[0], payload[1]]));
            let col_first = u32::from(u16::from_le_bytes([payload[2], payload[3]]));
            let col_last = u32::from(u16::from_le_bytes([
                payload[payload.len() - 2],
                payload[payload.len() - 1],
            ]));
            let mut pos = 4usize;
            let mut c = col_first;
            while c <= col_last && pos + 6 <= payload.len() - 2 {
                let ixfe = u32::from(u16::from_le_bytes([payload[pos], payload[pos + 1]]));
                let rk = u32::from_le_bytes([
                    payload[pos + 2],
                    payload[pos + 3],
                    payload[pos + 4],
                    payload[pos + 5],
                ]);
                let val = decode_rk_number(rk);
                let cell = finalize_cell(CellValue::Number(val), None, ixfe, fmt_opts, wb);
                sheet.cells.insert((r, c), cell);
                pos += 6;
                c += 1;
            }
        }
        REC_LABELSST if payload.len() >= 10 => {
            let (r, c, ixfe) = read_cell_header(payload);
            let isst =
                u32::from_le_bytes([payload[6], payload[7], payload[8], payload[9]]) as usize;
            let txt = sst.get(isst).cloned().unwrap_or_default();
            let cell = finalize_cell(CellValue::Text(txt), None, ixfe, fmt_opts, wb);
            sheet.cells.insert((r, c), cell);
        }
        REC_LABEL if payload.len() >= 8 => {
            let (r, c, ixfe) = read_cell_header(payload);
            let (txt, _) = read_xl_unicode_string(&payload[6..]).unwrap_or_default();
            let cell = finalize_cell(CellValue::Text(txt), None, ixfe, fmt_opts, wb);
            sheet.cells.insert((r, c), cell);
        }
        REC_BOOLERR if payload.len() >= 8 => {
            let (r, c, ixfe) = read_cell_header(payload);
            let b_val = payload[6];
            let is_err = payload[7] == 1;
            let val = if is_err {
                CellValue::Error(decode_biff_err_code(b_val).to_string())
            } else {
                CellValue::Bool(b_val != 0)
            };
            let cell = finalize_cell(val, None, ixfe, fmt_opts, wb);
            sheet.cells.insert((r, c), cell);
        }
        REC_FORMULA if payload.len() >= 20 => {
            let (r, c, ixfe) = read_cell_header(payload);
            let res_bytes = &payload[6..14];
            let mut wait_for_string = false;
            let val = if res_bytes[6] == 0xFF && res_bytes[7] == 0xFF {
                match res_bytes[0] {
                    0x00 => {
                        wait_for_string = true;
                        CellValue::Text(String::new())
                    }
                    0x01 => CellValue::Bool(res_bytes[2] != 0),
                    0x02 => CellValue::Error(decode_biff_err_code(res_bytes[2]).to_string()),
                    _ => CellValue::Empty,
                }
            } else {
                let mut b = [0u8; 8];
                b.copy_from_slice(res_bytes);
                CellValue::Number(f64::from_le_bytes(b))
            };
            let formula = Some(FormulaInfo {
                expression: "BIFF8_FORMULA".to_string(),
                is_array: false,
                ref_range: None,
                has_cached_value: true,
            });
            let cell = finalize_cell(val, formula, ixfe, fmt_opts, wb);
            sheet.cells.insert((r, c), cell);
            if wait_for_string {
                *pending_formula_cell = Some((r, c, ixfe));
            } else {
                *pending_formula_cell = None;
            }
        }
        REC_STRING => {
            if let Some((r, c, ixfe)) = pending_formula_cell.take()
                && let Some((txt, _)) = read_xl_unicode_string(payload)
            {
                let formula = Some(FormulaInfo {
                    expression: "BIFF8_FORMULA".to_string(),
                    is_array: false,
                    ref_range: None,
                    has_cached_value: true,
                });
                let cell = finalize_cell(CellValue::Text(txt), formula, ixfe, fmt_opts, wb);
                sheet.cells.insert((r, c), cell);
            }
        }
        REC_MERGEDCELLS if payload.len() >= 2 => {
            let count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
            let mut pos = 2usize;
            for _ in 0..count {
                if pos + 8 > payload.len() {
                    break;
                }
                let r1 = u32::from(u16::from_le_bytes([payload[pos], payload[pos + 1]]));
                let r2 = u32::from(u16::from_le_bytes([payload[pos + 2], payload[pos + 3]]));
                let c1 = u32::from(u16::from_le_bytes([payload[pos + 4], payload[pos + 5]]));
                let c2 = u32::from(u16::from_le_bytes([payload[pos + 6], payload[pos + 7]]));
                sheet.merged_ranges.push(CellRange {
                    start_row: r1.min(r2),
                    start_col: c1.min(c2),
                    end_row: r1.max(r2),
                    end_col: c1.max(c2),
                });
                pos += 8;
            }
        }
        REC_PANE if payload.len() >= 4 => {
            let x_split = u32::from(u16::from_le_bytes([payload[0], payload[1]]));
            let y_split = u32::from(u16::from_le_bytes([payload[2], payload[3]]));
            sheet.pane = Some(PaneInfo {
                frozen_rows: y_split,
                frozen_cols: x_split,
                top_left_cell: None,
            });
        }
        REC_SETUP if payload.len() >= 12 => {
            let scale = u16::from_le_bytes([payload[2], payload[3]]);
            let fit_w = u16::from_le_bytes([payload[6], payload[7]]);
            let fit_h = u16::from_le_bytes([payload[8], payload[9]]);
            let grbit = u16::from_le_bytes([payload[10], payload[11]]);
            if (grbit & 0x0002) == 0 {
                // In BIFF8 SETUP, bit 1 (`fPortrait`) == 0 means Landscape
                sheet.print_setup.orientation = PageOrientation::Landscape;
            }
            if (10..=400).contains(&scale) {
                sheet.print_setup.scale_pct = scale;
            }
            if fit_w > 0 {
                sheet.print_setup.fit_to_width = Some(fit_w);
            }
            if fit_h > 0 {
                sheet.print_setup.fit_to_height = Some(fit_h);
            }
        }
        REC_LEFT_MARGIN | REC_RIGHT_MARGIN | REC_TOP_MARGIN | REC_BOTTOM_MARGIN
            if payload.len() >= 8 =>
        {
            let mut b = [0u8; 8];
            b.copy_from_slice(&payload[0..8]);
            #[allow(clippy::cast_possible_truncation)]
            let pt = (f64::from_le_bytes(b) * 72.0).clamp(0.0, 216.0) as f32;
            match rec_type {
                REC_LEFT_MARGIN => sheet.print_setup.margin_left_pt = pt,
                REC_RIGHT_MARGIN => sheet.print_setup.margin_right_pt = pt,
                REC_TOP_MARGIN => sheet.print_setup.margin_top_pt = pt,
                REC_BOTTOM_MARGIN => sheet.print_setup.margin_bottom_pt = pt,
                _ => {}
            }
        }
        REC_HEADER => {
            if let Some((txt, _)) = read_xl_unicode_string(payload) {
                sheet.print_setup.header_text = Some(txt);
            }
        }
        REC_FOOTER => {
            if let Some((txt, _)) = read_xl_unicode_string(payload) {
                sheet.print_setup.footer_text = Some(txt);
            }
        }
        _ => {}
    }
}

fn read_cell_header(payload: &[u8]) -> (u32, u32, u32) {
    let r = u32::from(u16::from_le_bytes([payload[0], payload[1]]));
    let c = u32::from(u16::from_le_bytes([payload[2], payload[3]]));
    let ixfe = u32::from(u16::from_le_bytes([payload[4], payload[5]]));
    (r, c, ixfe)
}

/// Decode a 32-bit BIFF `RK` number into `f64` per `[MS-XLS]` §2.5.122.
#[must_use]
#[allow(clippy::cast_possible_wrap)]
pub fn decode_rk_number(rk: u32) -> f64 {
    let div_100 = (rk & 0x01) != 0;
    let is_int = (rk & 0x02) != 0;
    let base = if is_int {
        let signed = (rk as i32) >> 2;
        f64::from(signed)
    } else {
        let bits = u64::from(rk & !0x03) << 32;
        f64::from_bits(bits)
    };
    if div_100 { base / 100.0 } else { base }
}

fn decode_biff_err_code(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        _ => "#ERROR!",
    }
}

fn parse_boundsheet8(payload: &[u8]) -> Option<(String, SheetVisibility)> {
    if payload.len() < 8 {
        return None;
    }
    let hs_state = payload[4];
    let visibility = match hs_state {
        0x01 => SheetVisibility::Hidden,
        0x02 => SheetVisibility::VeryHidden,
        _ => SheetVisibility::Visible,
    };
    let cch = payload[6] as usize;
    let flags = payload[7];
    let str_bytes = payload.get(8..)?;
    let name = decode_biff_chars(str_bytes, cch, (flags & 0x01) != 0)?.0;
    Some((name, visibility))
}

fn parse_biff_format(payload: &[u8]) -> Option<(u16, String)> {
    if payload.len() < 5 {
        return None;
    }
    let fmt_id = u16::from_le_bytes([payload[0], payload[1]]);
    let (code, _) = read_xl_unicode_string(&payload[2..])?;
    Some((fmt_id, code))
}

fn parse_biff_font(payload: &[u8]) -> Option<ExcelFont> {
    if payload.len() < 16 {
        return None;
    }
    let dy_height = u16::from_le_bytes([payload[0], payload[1]]);
    let grbit = u16::from_le_bytes([payload[2], payload[3]]);
    let icv = u16::from_le_bytes([payload[4], payload[5]]);
    let bls = u16::from_le_bytes([payload[6], payload[7]]);
    let uls = payload[10];
    let cch = payload[14] as usize;
    let flags = payload[15];
    let (name, _) = decode_biff_chars(payload.get(16..)?, cch, (flags & 0x01) != 0)?;

    Some(ExcelFont {
        name: if name.is_empty() {
            "Arial".to_string()
        } else {
            name
        },
        size_pt: (f32::from(dy_height) / 20.0).clamp(4.0, 144.0),
        bold: bls >= 700,
        italic: (grbit & 0x0002) != 0,
        underline: uls != 0,
        strikethrough: (grbit & 0x0008) != 0,
        color: biff_indexed_color(icv),
    })
}

#[allow(clippy::similar_names)]
fn parse_biff_xf(payload: &[u8], fills: &mut Vec<ExcelFill>) -> Option<CellXf> {
    if payload.len() < 20 {
        return None;
    }
    let ifnt = u32::from(u16::from_le_bytes([payload[0], payload[1]]));
    let ifmt = u16::from_le_bytes([payload[2], payload[3]]);
    let align_byte = payload[6];
    let h_align = match align_byte & 0x07 {
        1 => CellHAlign::Left,
        2 | 6 => CellHAlign::Center,
        3 => CellHAlign::Right,
        4 | 5 | 7 => CellHAlign::Justify,
        _ => CellHAlign::General,
    };
    let wrap_text = (align_byte & 0x08) != 0;
    let v_align = match (align_byte >> 4) & 0x07 {
        0 => CellVAlign::Top,
        1 => CellVAlign::Center,
        _ => CellVAlign::Bottom,
    };

    // Check pattern fill in bytes 14..20
    let fill_pat_dword = u32::from_le_bytes([payload[14], payload[15], payload[16], payload[17]]);
    let fls = (fill_pat_dword >> 26) & 0x3F;
    let icv_bytes = u16::from_le_bytes([payload[18], payload[19]]);
    let icv_fore = icv_bytes & 0x007F;

    let fill_id = if fls == 1 {
        let fg_color = biff_indexed_color(icv_fore);
        #[allow(clippy::cast_possible_truncation)]
        let id = fills.len() as u32;
        fills.push(ExcelFill {
            pattern_type: "solid".to_string(),
            fg_color,
            bg_color: None,
        });
        id
    } else {
        0
    };

    Some(CellXf {
        num_fmt_id: ifmt,
        font_id: ifnt,
        fill_id,
        border_id: 0,
        h_align,
        v_align,
        wrap_text,
        shrink_to_fit: false,
        indent: 0,
        text_rotation: 0,
    })
}

fn parse_biff_lbl(payload: &[u8]) -> Option<DefinedName> {
    if payload.len() < 15 {
        return None;
    }
    let grbit = u16::from_le_bytes([payload[0], payload[1]]);
    let cch = payload[3] as usize;
    let itab = u16::from_le_bytes([payload[8], payload[9]]);
    let flags = payload[14];
    let str_bytes = payload.get(15..)?;
    let (raw_name, _) = decode_biff_chars(str_bytes, cch, (flags & 0x01) != 0)?;
    let name = if (grbit & 0x0020) != 0 && raw_name.len() == 1 {
        match raw_name.as_bytes()[0] {
            0x06 => "_xlnm.Print_Area".to_string(),
            0x07 => "_xlnm.Print_Titles".to_string(),
            _ => raw_name,
        }
    } else {
        raw_name
    };
    Some(DefinedName {
        name,
        formula: String::new(),
        local_sheet_id: if itab > 0 {
            Some(u32::from(itab - 1))
        } else {
            None
        },
        hidden: (grbit & 0x0001) != 0,
    })
}

fn parse_biff8_sst(chunks: &[&[u8]]) -> Vec<String> {
    let Some(first) = chunks.first() else {
        return Vec::new();
    };
    if first.len() < 8 {
        return Vec::new();
    }
    let unique_count =
        u32::from_le_bytes([first[4], first[5], first[6], first[7]]).min(200_000) as usize;
    let mut out = Vec::with_capacity(unique_count.min(4096));

    let mut chunk_idx = 0usize;
    let mut pos = 8usize;

    for _ in 0..unique_count {
        if !advance_to_available(chunks, &mut chunk_idx, &mut pos) {
            break;
        }
        let cur = chunks[chunk_idx];
        if pos + 3 > cur.len() {
            break;
        }
        let cch = u16::from_le_bytes([cur[pos], cur[pos + 1]]) as usize;
        let flags = cur[pos + 2];
        pos += 3;

        let mut is_utf16 = (flags & 0x01) != 0;
        let has_ext = (flags & 0x04) != 0;
        let has_rich = (flags & 0x08) != 0;

        let run_count = if has_rich {
            if pos + 2 > chunks[chunk_idx].len() {
                break;
            }
            let rc =
                u16::from_le_bytes([chunks[chunk_idx][pos], chunks[chunk_idx][pos + 1]]) as usize;
            pos += 2;
            rc
        } else {
            0
        };

        let ext_len = if has_ext {
            if pos + 4 > chunks[chunk_idx].len() {
                break;
            }
            let el = u32::from_le_bytes([
                chunks[chunk_idx][pos],
                chunks[chunk_idx][pos + 1],
                chunks[chunk_idx][pos + 2],
                chunks[chunk_idx][pos + 3],
            ]) as usize;
            pos += 4;
            el
        } else {
            0
        };

        let mut s = String::with_capacity(cch);
        let mut remaining_chars = cch;
        while remaining_chars > 0 {
            if pos >= chunks[chunk_idx].len() {
                chunk_idx += 1;
                if chunk_idx >= chunks.len() || chunks[chunk_idx].is_empty() {
                    break;
                }
                // First byte of a CONTINUE record continuing character data is the new grbit compression flag!
                is_utf16 = (chunks[chunk_idx][0] & 0x01) != 0;
                pos = 1;
            }
            let avail_bytes = chunks[chunk_idx].len().saturating_sub(pos);
            let bytes_per_char = if is_utf16 { 2 } else { 1 };
            let chars_here = (avail_bytes / bytes_per_char).min(remaining_chars);
            if chars_here == 0 {
                break;
            }
            let byte_slice = &chunks[chunk_idx][pos..pos + chars_here * bytes_per_char];
            if let Some((part, _)) = decode_biff_chars(byte_slice, chars_here, is_utf16) {
                s.push_str(&part);
            }
            pos += chars_here * bytes_per_char;
            remaining_chars -= chars_here;
        }

        // Skip rich text run formatting (4 bytes per run) and phonetic data (`ext_len` bytes)
        skip_sst_bytes(chunks, &mut chunk_idx, &mut pos, run_count * 4 + ext_len);
        out.push(s);
    }

    out
}

fn advance_to_available(chunks: &[&[u8]], chunk_idx: &mut usize, pos: &mut usize) -> bool {
    while *chunk_idx < chunks.len() && *pos >= chunks[*chunk_idx].len() {
        *chunk_idx += 1;
        *pos = 0;
    }
    *chunk_idx < chunks.len()
}

fn skip_sst_bytes(chunks: &[&[u8]], chunk_idx: &mut usize, pos: &mut usize, mut to_skip: usize) {
    while to_skip > 0 && advance_to_available(chunks, chunk_idx, pos) {
        let avail = chunks[*chunk_idx].len() - *pos;
        let step = avail.min(to_skip);
        *pos += step;
        to_skip -= step;
    }
}

/// Text of a `TXO` record: `cchText` characters spread over the following `CONTINUE`
/// records, each of which starts with its own high-byte flag.
fn read_txo_text(txo: &[u8], following: &[(u16, Vec<u8>)]) -> String {
    let cch = txo
        .get(10..12)
        .map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]])));
    let mut text = String::new();
    let mut remaining = cch;
    for (rec_type, chunk) in following {
        if remaining == 0 || *rec_type != REC_CONTINUE {
            break;
        }
        let Some((&flag, chars)) = chunk.split_first() else {
            continue;
        };
        let is_utf16 = flag & 0x01 != 0;
        let available = if is_utf16 {
            chars.len() / 2
        } else {
            chars.len()
        };
        let take = remaining.min(available);
        if let Some((s, _)) = decode_biff_chars(chars, take, is_utf16) {
            text.push_str(&s);
        }
        remaining -= take;
    }
    text
}

fn read_xl_unicode_string(bytes: &[u8]) -> Option<(String, usize)> {
    if bytes.len() < 3 {
        return None;
    }
    let cch = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    let flags = bytes[2];
    let is_utf16 = (flags & 0x01) != 0;
    let mut offset = 3usize;
    if (flags & 0x08) != 0 {
        offset += 2;
    }
    if (flags & 0x04) != 0 {
        offset += 4;
    }
    let slice = bytes.get(offset..)?;
    let (s, used) = decode_biff_chars(slice, cch, is_utf16)?;
    Some((s, offset + used))
}

fn decode_biff_chars(bytes: &[u8], cch: usize, is_utf16: bool) -> Option<(String, usize)> {
    if is_utf16 {
        let needed = cch.checked_mul(2)?;
        let slice = bytes.get(0..needed)?;
        let mut units = Vec::with_capacity(cch);
        let mut i = 0usize;
        while i + 2 <= slice.len() {
            units.push(u16::from_le_bytes([slice[i], slice[i + 1]]));
            i += 2;
        }
        Some((String::from_utf16_lossy(&units), needed))
    } else {
        let slice = bytes.get(0..cch)?;
        let s: String = slice.iter().map(|&b| char::from(b)).collect();
        Some((s, cch))
    }
}

fn collect_biff_records(data: &[u8]) -> Result<Vec<(u16, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        let rec_type = u16::from_le_bytes([data[pos], data[pos + 1]]);
        let rec_len = u16::from_le_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if pos + rec_len > data.len() {
            break;
        }
        out.push((rec_type, data[pos..pos + rec_len].to_vec()));
        pos += rec_len;
        if out.len() > 2_000_000 {
            return Err(Error::ParseError(
                "Exceeded maximum BIFF8 record count".to_string(),
            ));
        }
    }
    Ok(out)
}

fn read_cfb_stream(
    comp: &mut CompoundFile<Cursor<&[u8]>>,
    path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut stream = comp
        .open_stream(path)
        .map_err(|e| Error::ParseError(format!("Failed to open XLS stream {path}: {e}")))?;
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
