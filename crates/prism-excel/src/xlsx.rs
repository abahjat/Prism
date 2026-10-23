// SPDX-License-Identifier: AGPL-3.0-only
//! Sparse OOXML `SpreadsheetML` (`.xlsx`, `.xlsm`, `.xltx`) reader producing a [`Workbook`].
//!
//! Uses streaming `quick-xml` event parsing on `xl/sharedStrings.xml` and `xl/worksheets/sheet*.xml`
//! so that worksheets spanning `1,048,576 × 16,384` cells with only a few populated cells never
//! allocate dense arrays or full DOM trees.

use crate::model::{
    AnchoredDrawing, BorderEdge, Cell, CellHAlign, CellRange, CellVAlign, CellValue, CellXf,
    ColumnSpan, CondOperator, ConditionalFormatRule, ConditionalRuleKind, DefinedName,
    DrawingAnchor, DxfStyle, ExcelBorder, ExcelFill, ExcelFont, FormulaInfo, PageOrientation,
    PaneInfo, RichTextRun, RowProps, Sheet, SheetComment, SheetTable, SheetVisibility, Workbook,
    WorkbookStyles, WorksheetDrawingKind, parse_a1_cell_ref, parse_a1_range,
};
use prism_core::document::ImageResource;
use prism_core::error::{Error, Result};
use prism_drawingml::xml::resolve_general_ref;
use prism_drawingml::{
    ColorTransform, DrawingTheme, apply_color_transforms, parse_chart_xml, parse_drawing_theme,
    parse_hex_rgba,
};
use prism_layout::model::Rgba;
use prism_numfmt::{
    CellInputValue, DateSystem, FormatOptions, format_cell_value, is_concealed_format,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use zip::ZipArchive;

/// Default maximum decompressed bytes per ZIP entry (`64 MiB`).
pub const DEFAULT_MAX_XLSX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

/// English Metric Units per typographic point (`12,700`).
const EMU_PER_POINT: f32 = 12_700.0;

/// Check whether `bytes` is a ZIP archive containing `xl/workbook.xml`.
#[must_use]
pub fn is_xlsx_zip(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || &bytes[0..4] != b"PK\x03\x04" {
        return false;
    }
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    archive.by_name("xl/workbook.xml").is_ok()
}

/// Shared string entry (`<si>`).
#[derive(Debug, Clone)]
enum SharedStringItem {
    Plain(String),
    Rich(Vec<RichTextRun>),
}

#[derive(Debug, Clone)]
struct RelEntry {
    rel_type: String,
    target: String,
    external: bool,
}

/// Parse an `.xlsx` or `.xlsm` archive from `bytes` into a sparse [`Workbook`].
///
/// # Errors
///
/// Returns [`Error::ParseError`] if the archive is malformed or [`Error::MemoryLimitExceeded`]
/// if an entry exceeds `max_entry_bytes`.
#[allow(clippy::too_many_lines)]
pub fn parse_xlsx(bytes: &[u8], max_entry_bytes: usize) -> Result<Workbook> {
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|e| Error::ParseError(format!("Invalid XLSX ZIP archive: {e}")))?;

    let mut wb = Workbook::default();

    if archive.by_name("xl/vbaProject.bin").is_ok() {
        wb.forensic.vba_project_present = true;
    }

    // 1. Parse docProps/core.xml
    if let Ok(core_xml) = read_zip_string(&mut archive, "docProps/core.xml", max_entry_bytes) {
        parse_core_props(&core_xml, &mut wb);
    }

    // 2. Parse xl/theme/theme1.xml
    if let Ok(theme_xml) = read_zip_string(&mut archive, "xl/theme/theme1.xml", max_entry_bytes) {
        wb.theme = parse_drawing_theme(&theme_xml);
    }

    // 3. Parse xl/styles.xml
    if let Ok(styles_xml) = read_zip_string(&mut archive, "xl/styles.xml", max_entry_bytes) {
        wb.styles = parse_styles_xml(&styles_xml, &wb.theme);
    }

    // 4. Parse xl/sharedStrings.xml
    let shared_strings = if let Ok(sst_xml) =
        read_zip_string(&mut archive, "xl/sharedStrings.xml", max_entry_bytes)
    {
        parse_shared_strings(&sst_xml, &wb.theme)
    } else {
        Vec::new()
    };

    // 5. Parse xl/persons/person*.xml for threaded comments
    let entry_names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();
    let mut persons_map: BTreeMap<String, String> = BTreeMap::new();
    for name in &entry_names {
        if name.starts_with("xl/persons/")
            && is_xml_ext(name)
            && let Ok(pxml) = read_zip_string(&mut archive, name, max_entry_bytes)
        {
            parse_persons_xml(&pxml, &mut persons_map);
        }
        // Record externalLinks and connections
        if (name.starts_with("xl/externalLinks/_rels/") || name == "xl/connections.xml")
            && let Ok(exml) = read_zip_string(&mut archive, name, max_entry_bytes)
        {
            extract_external_targets(&exml, &mut wb);
        }
    }

    // 6. Parse xl/workbook.xml and xl/_rels/workbook.xml.rels
    let wb_xml = read_zip_string(&mut archive, "xl/workbook.xml", max_entry_bytes)
        .map_err(|_| Error::ParseError("Missing xl/workbook.xml".to_string()))?;
    let wb_rels = parse_part_rels(&mut archive, "xl/workbook.xml", max_entry_bytes);
    record_external_rels(&wb_rels, &mut wb);

    let sheet_stubs = parse_workbook_xml(&wb_xml, &wb_rels, &mut wb);
    wb.forensic.defined_names_count = wb.defined_names.len();

    // 7. Parse each worksheet in tab order
    let mut distinct_authors = BTreeSet::new();
    for (sheet_idx, stub) in sheet_stubs.into_iter().enumerate() {
        match stub.visibility {
            SheetVisibility::Visible => wb.forensic.visible_sheet_names.push(stub.name.clone()),
            SheetVisibility::Hidden => wb.forensic.hidden_sheet_names.push(stub.name.clone()),
            SheetVisibility::VeryHidden => {
                wb.forensic.very_hidden_sheet_names.push(stub.name.clone());
            }
        }

        let mut sheet = if let Some(ref part_path) = stub.part_path
            && let Ok(s) = parse_worksheet_part(
                part_path,
                &stub.name,
                stub.sheet_id,
                stub.visibility,
                &shared_strings,
                &persons_map,
                &mut distinct_authors,
                &mut archive,
                max_entry_bytes,
                &mut wb,
            ) {
            s
        } else {
            Sheet {
                name: stub.name,
                sheet_id: stub.sheet_id,
                visibility: stub.visibility,
                ..Sheet::default()
            }
        };

        // Apply workbook-level Print_Area and Print_Titles defined names for this sheet
        #[allow(clippy::cast_possible_truncation)]
        apply_defined_names_to_sheet(&mut sheet, sheet_idx as u32, &wb.defined_names);

        wb.sheets.push(sheet);
    }

    wb.forensic.comment_authors = distinct_authors.into_iter().collect();
    Ok(wb)
}

#[derive(Debug, Clone)]
struct SheetStub {
    name: String,
    sheet_id: u32,
    visibility: SheetVisibility,
    part_path: Option<String>,
}

fn parse_workbook_xml(
    xml: &str,
    rels: &BTreeMap<String, RelEntry>,
    wb: &mut Workbook,
) -> Vec<SheetStub> {
    let mut stubs = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut in_defined_name = false;
    let mut cur_dn = DefinedName::default();

    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let qn = e.name();
                let local = local_name(qn.as_ref());
                match local {
                    "workbookPr" => {
                        if attr_val(e, "date1904")
                            .is_some_and(|v| matches!(v.as_str(), "1" | "true"))
                        {
                            wb.date_system = DateSystem::Date1904;
                        }
                    }
                    "workbookProtection" => {
                        wb.forensic.workbook_protected = true;
                    }
                    "calcPr" => {
                        wb.forensic.calc_id = attr_val(e, "calcId");
                    }
                    "sheet" => {
                        let name = attr_val(e, "name").unwrap_or_else(|| "Sheet".to_string());
                        let sheet_id = attr_val(e, "sheetId")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(1);
                        let visibility = match attr_val(e, "state").as_deref() {
                            Some("hidden") => SheetVisibility::Hidden,
                            Some("veryHidden") => SheetVisibility::VeryHidden,
                            _ => SheetVisibility::Visible,
                        };
                        let rid = attr_val(e, "r:id").or_else(|| attr_val(e, "id"));
                        let part_path = rid
                            .and_then(|r| rels.get(&r))
                            .filter(|rel| !rel.external)
                            .map(|rel| resolve_zip_path("xl/workbook.xml", &rel.target));
                        stubs.push(SheetStub {
                            name,
                            sheet_id,
                            visibility,
                            part_path,
                        });
                    }
                    "definedName" => {
                        in_defined_name = true;
                        cur_dn = DefinedName {
                            name: attr_val(e, "name").unwrap_or_default(),
                            formula: String::new(),
                            local_sheet_id: attr_val(e, "localSheetId")
                                .and_then(|v| v.parse::<u32>().ok()),
                            hidden: attr_val(e, "hidden")
                                .is_some_and(|v| matches!(v.as_str(), "1" | "true")),
                        };
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_defined_name => {
                let txt = quick_xml::escape::unescape(t.as_ref())
                    .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                cur_dn.formula.push_str(&txt);
            }
            Ok(Event::End(ref e)) => {
                let qn = e.name();
                if local_name(qn.as_ref()) == "definedName" && in_defined_name {
                    in_defined_name = false;
                    wb.defined_names.push(std::mem::take(&mut cur_dn));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    stubs
}

fn apply_defined_names_to_sheet(sheet: &mut Sheet, sheet_idx: u32, defined_names: &[DefinedName]) {
    for dn in defined_names {
        let applies = dn.local_sheet_id == Some(sheet_idx)
            || (dn.local_sheet_id.is_none()
                && dn
                    .formula
                    .split('!')
                    .next()
                    .is_some_and(|prefix| prefix.trim_matches('\'') == sheet.name));
        if !applies {
            continue;
        }
        if dn.name.eq_ignore_ascii_case("_xlnm.Print_Area") {
            let first_part = dn.formula.split(',').next().unwrap_or(&dn.formula);
            if let Some(rng) = parse_a1_range(first_part) {
                sheet.print_setup.print_area = Some(rng);
            }
        } else if dn.name.eq_ignore_ascii_case("_xlnm.Print_Titles") {
            for part in dn.formula.split(',') {
                let expr = part.rsplit('!').next().unwrap_or(part).trim();
                if let Some((a, b)) = expr.split_once(':') {
                    let a_clean = a.trim_matches('$');
                    let b_clean = b.trim_matches('$');
                    if let (Ok(r1), Ok(r2)) = (a_clean.parse::<u32>(), b_clean.parse::<u32>())
                        && r1 >= 1
                        && r2 >= r1
                    {
                        sheet.print_setup.repeat_rows = Some((r1 - 1, r2 - 1));
                    } else if let (Some((_, c1)), Some((_, c2))) = (
                        parse_a1_cell_ref(&format!("{a_clean}1")),
                        parse_a1_cell_ref(&format!("{b_clean}1")),
                    ) {
                        sheet.print_setup.repeat_cols = Some((c1.min(c2), c1.max(c2)));
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_lines, clippy::similar_names)]
fn parse_styles_xml(xml: &str, theme: &DrawingTheme) -> WorkbookStyles {
    let mut styles = WorkbookStyles::default();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut in_fonts = false;
    let mut cur_font: Option<ExcelFont> = None;

    let mut in_fills = false;
    let mut cur_fill: Option<ExcelFill> = None;

    let mut in_borders = false;
    let mut cur_border: Option<ExcelBorder> = None;
    let mut cur_border_side: Option<String> = None;
    let mut cur_edge: Option<BorderEdge> = None;

    let mut in_cell_xfs = false;
    let mut cur_xf: Option<CellXf> = None;

    let mut in_dxfs = false;
    let mut cur_dxf: Option<DxfStyle> = None;
    let mut in_dxf_font = false;
    let mut in_dxf_fill = false;

    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e)) => {
                let qn = e.name();
                let local = local_name(qn.as_ref());
                match local {
                    "numFmt" => {
                        if let (Some(id_s), Some(code)) =
                            (attr_val(e, "numFmtId"), attr_val(e, "formatCode"))
                            && let Ok(id) = id_s.parse::<u16>()
                        {
                            if let Some(ref mut dxf) = cur_dxf {
                                dxf.num_fmt_code = Some(code);
                            } else {
                                styles.num_fmts.insert(id, code);
                            }
                        }
                    }
                    "fonts" => in_fonts = true,
                    "font" if in_fonts => cur_font = Some(ExcelFont::default()),
                    "font" if in_dxfs => in_dxf_font = true,
                    "fills" => in_fills = true,
                    "fill" if in_fills => cur_fill = Some(ExcelFill::default()),
                    "fill" if in_dxfs => in_dxf_fill = true,
                    "patternFill" => {
                        let pt = attr_val(e, "patternType").unwrap_or_default();
                        if let Some(ref mut f) = cur_fill {
                            f.pattern_type = pt;
                        }
                    }
                    "borders" => in_borders = true,
                    "border" if in_borders => cur_border = Some(ExcelBorder::default()),
                    "left" | "right" | "top" | "bottom" if cur_border.is_some() => {
                        cur_border_side = Some(local.to_string());
                        if let Some(st) = attr_val(e, "style")
                            && st != "none"
                        {
                            cur_edge = Some(BorderEdge {
                                width_pt: border_style_width(&st),
                                style: st,
                                color: Rgba::rgb(0xD0, 0xD7, 0xDE),
                            });
                        } else {
                            cur_edge = None;
                        }
                    }
                    "cellXfs" => in_cell_xfs = true,
                    "xf" if in_cell_xfs => {
                        cur_xf = Some(parse_xf_attrs(e));
                    }
                    "alignment" if cur_xf.is_some() => {
                        if let Some(ref mut xf) = cur_xf {
                            apply_xf_alignment(e, xf);
                        }
                    }
                    "dxfs" => in_dxfs = true,
                    "dxf" if in_dxfs => cur_dxf = Some(DxfStyle::default()),
                    "b" | "i" | "u" | "strike" | "name" | "sz" | "color" | "fgColor"
                    | "bgColor" => {
                        handle_style_leaf(
                            local,
                            e,
                            theme,
                            &mut cur_font,
                            &mut cur_fill,
                            &mut cur_edge,
                            &mut cur_dxf,
                            in_dxf_font,
                            in_dxf_fill,
                        );
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let qn = e.name();
                let local = local_name(qn.as_ref());
                match local {
                    "numFmt" => {
                        if let (Some(id_s), Some(code)) =
                            (attr_val(e, "numFmtId"), attr_val(e, "formatCode"))
                            && let Ok(id) = id_s.parse::<u16>()
                        {
                            if let Some(ref mut dxf) = cur_dxf {
                                dxf.num_fmt_code = Some(code);
                            } else {
                                styles.num_fmts.insert(id, code);
                            }
                        }
                    }
                    "xf" if in_cell_xfs => {
                        styles.cell_xfs.push(parse_xf_attrs(e));
                    }
                    "alignment" if cur_xf.is_some() => {
                        if let Some(ref mut xf) = cur_xf {
                            apply_xf_alignment(e, xf);
                        }
                    }
                    "patternFill" => {
                        let pt = attr_val(e, "patternType").unwrap_or_default();
                        if let Some(ref mut f) = cur_fill {
                            f.pattern_type = pt;
                        }
                    }
                    "b" | "i" | "u" | "strike" | "name" | "sz" | "color" | "fgColor"
                    | "bgColor" => {
                        handle_style_leaf(
                            local,
                            e,
                            theme,
                            &mut cur_font,
                            &mut cur_fill,
                            &mut cur_edge,
                            &mut cur_dxf,
                            in_dxf_font,
                            in_dxf_fill,
                        );
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let qn = e.name();
                let local = local_name(qn.as_ref());
                match local {
                    "fonts" => in_fonts = false,
                    "font" if in_fonts => {
                        if let Some(f) = cur_font.take() {
                            styles.fonts.push(f);
                        }
                    }
                    "font" if in_dxfs => in_dxf_font = false,
                    "fills" => in_fills = false,
                    "fill" if in_fills => {
                        if let Some(f) = cur_fill.take() {
                            styles.fills.push(f);
                        }
                    }
                    "fill" if in_dxfs => in_dxf_fill = false,
                    "borders" => in_borders = false,
                    "border" if in_borders => {
                        if let Some(b) = cur_border.take() {
                            styles.borders.push(b);
                        }
                    }
                    "left" | "right" | "top" | "bottom" if cur_border.is_some() => {
                        if let (Some(b), Some(side)) = (&mut cur_border, cur_border_side.take()) {
                            match side.as_str() {
                                "left" => b.left = cur_edge.take(),
                                "right" => b.right = cur_edge.take(),
                                "top" => b.top = cur_edge.take(),
                                "bottom" => b.bottom = cur_edge.take(),
                                _ => {}
                            }
                        }
                    }
                    "cellXfs" => in_cell_xfs = false,
                    "xf" if in_cell_xfs => {
                        if let Some(xf) = cur_xf.take() {
                            styles.cell_xfs.push(xf);
                        }
                    }
                    "dxfs" => in_dxfs = false,
                    "dxf" if in_dxfs => {
                        if let Some(dxf) = cur_dxf.take() {
                            styles.dxfs.push(dxf);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    if styles.fonts.is_empty() {
        styles.fonts.push(ExcelFont::default());
    }
    if styles.cell_xfs.is_empty() {
        styles.cell_xfs.push(CellXf::default());
    }
    styles
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn handle_style_leaf(
    local: &str,
    e: &BytesStart<'_>,
    theme: &DrawingTheme,
    cur_font: &mut Option<ExcelFont>,
    cur_fill: &mut Option<ExcelFill>,
    cur_edge: &mut Option<BorderEdge>,
    cur_dxf: &mut Option<DxfStyle>,
    in_dxf_font: bool,
    in_dxf_fill: bool,
) {
    let flag_on = attr_val(e, "val").is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "none"));
    match local {
        "b" => {
            if let Some(f) = cur_font {
                f.bold = flag_on;
            } else if in_dxf_font && let Some(d) = cur_dxf {
                d.bold = Some(flag_on);
            }
        }
        "i" => {
            if let Some(f) = cur_font {
                f.italic = flag_on;
            } else if in_dxf_font && let Some(d) = cur_dxf {
                d.italic = Some(flag_on);
            }
        }
        "u" => {
            if let Some(f) = cur_font {
                f.underline = flag_on;
            }
        }
        "strike" => {
            if let Some(f) = cur_font {
                f.strikethrough = flag_on;
            }
        }
        "name" => {
            if let Some(f) = cur_font
                && let Some(v) = attr_val(e, "val")
            {
                f.name = v;
            }
        }
        "sz" => {
            if let Some(f) = cur_font
                && let Some(sz) = attr_val(e, "val").and_then(|v| v.parse::<f32>().ok())
            {
                f.size_pt = sz.clamp(4.0, 144.0);
            }
        }
        "color" => {
            if let Some(c) = resolve_excel_color(e, theme) {
                if let Some(f) = cur_font {
                    f.color = Some(c);
                } else if let Some(ed) = cur_edge {
                    ed.color = c;
                } else if in_dxf_font && let Some(d) = cur_dxf {
                    d.font_color = Some(c);
                }
            }
        }
        "fgColor" => {
            if let Some(c) = resolve_excel_color(e, theme) {
                if let Some(fl) = cur_fill {
                    fl.fg_color = Some(c);
                } else if in_dxf_fill && let Some(d) = cur_dxf {
                    d.fill_color = Some(c);
                }
            }
        }
        "bgColor" => {
            if let Some(c) = resolve_excel_color(e, theme) {
                if let Some(fl) = cur_fill {
                    fl.bg_color = Some(c);
                } else if in_dxf_fill
                    && let Some(d) = cur_dxf
                    && d.fill_color.is_none()
                {
                    d.fill_color = Some(c);
                }
            }
        }
        _ => {}
    }
}

fn parse_xf_attrs(e: &BytesStart<'_>) -> CellXf {
    CellXf {
        num_fmt_id: attr_val(e, "numFmtId")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(0),
        font_id: attr_val(e, "fontId")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0),
        fill_id: attr_val(e, "fillId")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0),
        border_id: attr_val(e, "borderId")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0),
        ..CellXf::default()
    }
}

fn apply_xf_alignment(e: &BytesStart<'_>, xf: &mut CellXf) {
    if let Some(h) = attr_val(e, "horizontal") {
        xf.h_align = match h.as_str() {
            "left" => CellHAlign::Left,
            "center" | "centerContinuous" => CellHAlign::Center,
            "right" => CellHAlign::Right,
            "justify" | "distributed" | "fill" => CellHAlign::Justify,
            _ => CellHAlign::General,
        };
    }
    if let Some(v) = attr_val(e, "vertical") {
        xf.v_align = match v.as_str() {
            "top" => CellVAlign::Top,
            "center" | "justify" | "distributed" => CellVAlign::Center,
            _ => CellVAlign::Bottom,
        };
    }
    if attr_val(e, "wrapText").is_some_and(|v| matches!(v.as_str(), "1" | "true")) {
        xf.wrap_text = true;
    }
    if attr_val(e, "shrinkToFit").is_some_and(|v| matches!(v.as_str(), "1" | "true")) {
        xf.shrink_to_fit = true;
    }
    if let Some(ind) = attr_val(e, "indent").and_then(|v| v.parse::<u8>().ok()) {
        xf.indent = ind;
    }
    if let Some(rot) = attr_val(e, "textRotation").and_then(|v| v.parse::<u16>().ok()) {
        xf.text_rotation = rot;
    }
}

fn border_style_width(style: &str) -> f32 {
    match style {
        "medium" | "mediumDashed" | "mediumDashDot" => 1.5,
        "thick" | "double" => 2.25,
        "hair" => 0.5,
        _ => 0.75,
    }
}

#[allow(clippy::many_single_char_names)]
fn resolve_excel_color(e: &BytesStart<'_>, theme: &DrawingTheme) -> Option<Rgba> {
    if let Some(rgb_hex) = attr_val(e, "rgb") {
        // Excel ARGB is AARRGGBB (8 hex chars) or RRGGBB (6 hex chars)
        let clean = rgb_hex.trim().trim_start_matches('#');
        if clean.len() == 8 && clean.bytes().all(|b| b.is_ascii_hexdigit()) {
            let a = u8::from_str_radix(&clean[0..2], 16).ok()?;
            let r = u8::from_str_radix(&clean[2..4], 16).ok()?;
            let g = u8::from_str_radix(&clean[4..6], 16).ok()?;
            let b = u8::from_str_radix(&clean[6..8], 16).ok()?;
            return Some(Rgba::new(r, g, b, a));
        }
        return parse_hex_rgba(clean);
    }
    if let Some(theme_idx) = attr_val(e, "theme").and_then(|v| v.parse::<usize>().ok()) {
        // OOXML SpreadsheetML theme index order: 0=lt1, 1=dk1, 2=lt2, 3=dk2, 4..9=accent1..6, 10=hlink, 11=folHlink
        let slot = match theme_idx {
            0 => "lt1",
            2 => "lt2",
            3 => "dk2",
            4 => "accent1",
            5 => "accent2",
            6 => "accent3",
            7 => "accent4",
            8 => "accent5",
            9 => "accent6",
            10 => "hlink",
            11 => "folHlink",
            _ => "dk1",
        };
        let base = theme.resolve_scheme_color(slot)?;
        if let Some(tint) = attr_val(e, "tint").and_then(|v| v.parse::<f32>().ok()) {
            let tx = if tint >= 0.0 {
                // Positive tint lightens toward white: c' = c * (1 - tint) + 1 * tint
                ColorTransform::Tint(1.0 - tint)
            } else {
                // Negative tint darkens toward black: c' = c * (1 + tint)
                ColorTransform::Shade(1.0 + tint)
            };
            return Some(apply_color_transforms(base, &[tx]));
        }
        return Some(base);
    }
    if let Some(idx) = attr_val(e, "indexed").and_then(|v| v.parse::<u16>().ok()) {
        return biff_indexed_color(idx);
    }
    None
}

/// Resolve a legacy BIFF8 / OOXML `indexed` palette color (`0..=64`).
#[must_use]
pub fn biff_indexed_color(idx: u16) -> Option<Rgba> {
    match idx {
        0 | 8 | 64 => Some(Rgba::rgb(0x00, 0x00, 0x00)),
        1 | 9 => Some(Rgba::rgb(0xFF, 0xFF, 0xFF)),
        2 | 10 => Some(Rgba::rgb(0xFF, 0x00, 0x00)),
        3 | 11 => Some(Rgba::rgb(0x00, 0xFF, 0x00)),
        4 | 12 => Some(Rgba::rgb(0x00, 0x00, 0xFF)),
        5 | 13 => Some(Rgba::rgb(0xFF, 0xFF, 0x00)),
        6 | 14 => Some(Rgba::rgb(0xFF, 0x00, 0xFF)),
        7 | 15 => Some(Rgba::rgb(0x00, 0xFF, 0xFF)),
        16 => Some(Rgba::rgb(0x80, 0x00, 0x00)),
        17 => Some(Rgba::rgb(0x00, 0x80, 0x00)),
        18 => Some(Rgba::rgb(0x00, 0x00, 0x80)),
        19 => Some(Rgba::rgb(0x80, 0x80, 0x00)),
        20 => Some(Rgba::rgb(0x80, 0x00, 0x80)),
        21 => Some(Rgba::rgb(0x00, 0x80, 0x80)),
        22 => Some(Rgba::rgb(0xC0, 0xC0, 0xC0)),
        23 => Some(Rgba::rgb(0x80, 0x80, 0x80)),
        24 => Some(Rgba::rgb(0x99, 0x99, 0xFF)),
        25 => Some(Rgba::rgb(0x99, 0x33, 0x66)),
        26 => Some(Rgba::rgb(0xFF, 0xFF, 0xCC)),
        27 | 41 => Some(Rgba::rgb(0xCC, 0xFF, 0xFF)),
        40 => Some(Rgba::rgb(0x00, 0xCC, 0xFF)),
        42 => Some(Rgba::rgb(0xCC, 0xFF, 0xCC)),
        43 => Some(Rgba::rgb(0xFF, 0xFF, 0x99)),
        44 => Some(Rgba::rgb(0x99, 0xCC, 0xFF)),
        45 => Some(Rgba::rgb(0xFF, 0x99, 0xCC)),
        46 => Some(Rgba::rgb(0xCC, 0x99, 0xFF)),
        47 => Some(Rgba::rgb(0xFF, 0xCC, 0x99)),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
fn parse_shared_strings(xml: &str, theme: &DrawingTheme) -> Vec<SharedStringItem> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut in_si = false;
    let mut in_r = false;
    let mut in_rph = false;
    let mut in_t = false;
    let mut runs: Vec<RichTextRun> = Vec::new();
    let mut cur_run = RichTextRun::default();
    let mut plain_buf = String::new();
    let mut has_rich_runs = false;

    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "si" => {
                        in_si = true;
                        runs.clear();
                        plain_buf.clear();
                        has_rich_runs = false;
                    }
                    "r" if in_si => {
                        in_r = true;
                        has_rich_runs = true;
                        cur_run = RichTextRun::default();
                    }
                    "rPh" if in_si => {
                        // Phonetic ruby text — skip so main text isn't duplicated
                        in_rph = true;
                    }
                    "t" if in_si && !in_rph => {
                        in_t = true;
                    }
                    "b" | "i" | "u" | "strike" | "rFont" | "sz" | "color" if in_r => {
                        apply_rich_run_tag(local_name(qn.as_ref()), e, theme, &mut cur_run);
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let qn = e.name();
                if in_r {
                    apply_rich_run_tag(local_name(qn.as_ref()), e, theme, &mut cur_run);
                }
            }
            Ok(Event::Text(t)) if in_t && !in_rph => {
                let txt = quick_xml::escape::unescape(t.as_ref())
                    .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                if in_r {
                    cur_run.text.push_str(&txt);
                } else {
                    plain_buf.push_str(&txt);
                }
            }
            Ok(Event::CData(c)) if in_t && !in_rph => {
                if in_r {
                    cur_run.text.push_str(c.as_ref());
                } else {
                    plain_buf.push_str(c.as_ref());
                }
            }
            Ok(Event::End(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "t" => in_t = false,
                    "rPh" => in_rph = false,
                    "r" if in_r => {
                        in_r = false;
                        runs.push(std::mem::take(&mut cur_run));
                    }
                    "si" if in_si => {
                        in_si = false;
                        if has_rich_runs {
                            let any_styled = runs.iter().any(|r| {
                                r.bold
                                    || r.italic
                                    || r.underline
                                    || r.strikethrough
                                    || r.color.is_some()
                                    || r.font_family.is_some()
                                    || r.font_size_pt.is_some()
                            });
                            if any_styled {
                                out.push(SharedStringItem::Rich(std::mem::take(&mut runs)));
                            } else {
                                let joined: String = runs.drain(..).map(|r| r.text).collect();
                                out.push(SharedStringItem::Plain(joined));
                            }
                        } else {
                            out.push(SharedStringItem::Plain(std::mem::take(&mut plain_buf)));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    out
}

fn apply_rich_run_tag(
    local: &str,
    e: &BytesStart<'_>,
    theme: &DrawingTheme,
    run: &mut RichTextRun,
) {
    let flag_on = attr_val(e, "val").is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "none"));
    match local {
        "b" => run.bold = flag_on,
        "i" => run.italic = flag_on,
        "u" => run.underline = flag_on,
        "strike" => run.strikethrough = flag_on,
        "rFont" => run.font_family = attr_val(e, "val"),
        "sz" => {
            run.font_size_pt = attr_val(e, "val").and_then(|v| v.parse::<f32>().ok());
        }
        "color" => {
            run.color = resolve_excel_color(e, theme);
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn parse_worksheet_part(
    part_path: &str,
    name: &str,
    sheet_id: u32,
    visibility: SheetVisibility,
    shared_strings: &[SharedStringItem],
    persons_map: &BTreeMap<String, String>,
    distinct_authors: &mut BTreeSet<String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    wb: &mut Workbook,
) -> Result<Sheet> {
    let xml = read_zip_string(archive, part_path, max_bytes)?;
    let rels = parse_part_rels(archive, part_path, max_bytes);
    record_external_rels(&rels, wb);

    let mut sheet = Sheet {
        name: name.to_string(),
        sheet_id,
        visibility,
        ..Sheet::default()
    };

    let fmt_opts = FormatOptions {
        date_system: wb.date_system,
        ..FormatOptions::default()
    };

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);

    // Current cell parsing state
    let mut cur_row_idx: u32 = 0;
    let mut cur_col_idx: u32 = 0;
    let mut in_cell = false;
    let mut cell_type = String::new();
    let mut cell_style_idx: u32 = 0;
    let mut in_v = false;
    let mut in_f = false;
    let mut in_is = false;
    let mut in_is_t = false;
    let mut in_rph = false;
    let mut v_buf = String::new();
    let mut f_buf = String::new();
    let mut is_buf = String::new();
    let mut f_is_array = false;
    let mut f_ref: Option<String> = None;
    let mut f_si: Option<u32> = None;
    let mut has_f = false;
    let mut has_v = false;
    let mut shared_formulas: BTreeMap<u32, String> = BTreeMap::new();

    // Conditional formatting parsing state
    let mut cur_cf_ranges: Vec<CellRange> = Vec::new();
    let mut cur_cf_rule: Option<ConditionalFormatRule> = None;
    let mut cf_formulas: Vec<String> = Vec::new();
    let mut in_cf_formula = false;
    let mut cf_formula_buf = String::new();
    let mut cf_colors: Vec<Rgba> = Vec::new();
    let mut in_data_bar = false;

    // Header/Footer state
    let mut in_odd_header = false;
    let mut in_odd_footer = false;
    let mut hf_buf = String::new();

    // Page break state
    let mut in_row_breaks = false;
    let mut in_col_breaks = false;

    // Linked part paths from relationships
    let mut drawing_rids: Vec<String> = Vec::new();
    let mut table_rids: Vec<String> = Vec::new();

    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e)) => {
                let qn = e.name();
                let local = local_name(qn.as_ref());
                match local {
                    "row" => {
                        if let Some(r1) = attr_val(e, "r").and_then(|v| v.parse::<u32>().ok()) {
                            cur_row_idx = r1.saturating_sub(1);
                        } else {
                            cur_row_idx = cur_row_idx.saturating_add(1);
                        }
                        cur_col_idx = 0;
                        parse_row_attrs(e, cur_row_idx, &mut sheet, wb);
                    }
                    "c" => {
                        in_cell = true;
                        if let Some(r_str) = attr_val(e, "r")
                            && let Some((r, c)) = parse_a1_cell_ref(&r_str)
                        {
                            cur_row_idx = r;
                            cur_col_idx = c;
                        }
                        cell_type = attr_val(e, "t").unwrap_or_default();
                        cell_style_idx = attr_val(e, "s")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(0);
                        v_buf.clear();
                        f_buf.clear();
                        is_buf.clear();
                        has_f = false;
                        has_v = false;
                        f_is_array = false;
                        f_ref = None;
                        f_si = None;
                    }
                    "f" if in_cell => {
                        in_f = true;
                        has_f = true;
                        let ft = attr_val(e, "t").unwrap_or_default();
                        f_is_array = ft == "array";
                        f_ref = attr_val(e, "ref");
                        f_si = attr_val(e, "si").and_then(|v| v.parse::<u32>().ok());
                    }
                    "v" if in_cell => {
                        in_v = true;
                        has_v = true;
                    }
                    "is" if in_cell => {
                        in_is = true;
                        has_v = true;
                    }
                    "rPh" if in_is => {
                        in_rph = true;
                    }
                    "t" if in_is && !in_rph => {
                        in_is_t = true;
                    }
                    "conditionalFormatting" => {
                        cur_cf_ranges.clear();
                        if let Some(sqref) = attr_val(e, "sqref") {
                            for token in sqref.split_whitespace() {
                                if let Some(rng) = parse_a1_range(token) {
                                    cur_cf_ranges.push(rng);
                                }
                            }
                        }
                    }
                    "cfRule" => {
                        cur_cf_rule = Some(start_cf_rule(e, &cur_cf_ranges));
                        cf_formulas.clear();
                        cf_colors.clear();
                        in_data_bar = false;
                    }
                    "formula" if cur_cf_rule.is_some() => {
                        in_cf_formula = true;
                        cf_formula_buf.clear();
                    }
                    "dataBar" if cur_cf_rule.is_some() => {
                        in_data_bar = true;
                    }
                    "oddHeader" => {
                        in_odd_header = true;
                        hf_buf.clear();
                    }
                    "oddFooter" => {
                        in_odd_footer = true;
                        hf_buf.clear();
                    }
                    "rowBreaks" => in_row_breaks = true,
                    "colBreaks" => in_col_breaks = true,
                    other => {
                        handle_sheet_empty_or_start_tag(
                            other,
                            e,
                            &rels,
                            &mut sheet,
                            wb,
                            &mut drawing_rids,
                            &mut table_rids,
                            &mut cf_colors,
                            cur_cf_rule.is_some(),
                            in_row_breaks,
                            in_col_breaks,
                        );
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let qn = e.name();
                let local = local_name(qn.as_ref());
                match local {
                    "row" => {
                        if let Some(r1) = attr_val(e, "r").and_then(|v| v.parse::<u32>().ok()) {
                            cur_row_idx = r1.saturating_sub(1);
                        }
                        parse_row_attrs(e, cur_row_idx, &mut sheet, wb);
                    }
                    "c" => {
                        // Self-closing <c r="A1" s="2"/> (blank styled cell)
                        if let Some(r_str) = attr_val(e, "r")
                            && let Some((r, c)) = parse_a1_cell_ref(&r_str)
                            && let Some(s_idx) =
                                attr_val(e, "s").and_then(|v| v.parse::<u32>().ok())
                            && s_idx > 0
                        {
                            let cell = finalize_cell(CellValue::Empty, None, s_idx, &fmt_opts, wb);
                            sheet.cells.insert((r, c), cell);
                        }
                    }
                    "f" if in_cell => {
                        has_f = true;
                        f_si = attr_val(e, "si").and_then(|v| v.parse::<u32>().ok());
                        if let Some(si) = f_si
                            && let Some(master_f) = shared_formulas.get(&si)
                        {
                            f_buf.clone_from(master_f);
                        }
                    }
                    "cfRule" => {
                        let mut rule = start_cf_rule(e, &cur_cf_ranges);
                        finish_cf_rule(&mut rule, &[], &[], false, wb);
                        sheet.conditional_rules.push(rule);
                    }
                    other => {
                        handle_sheet_empty_or_start_tag(
                            other,
                            e,
                            &rels,
                            &mut sheet,
                            wb,
                            &mut drawing_rids,
                            &mut table_rids,
                            &mut cf_colors,
                            cur_cf_rule.is_some(),
                            in_row_breaks,
                            in_col_breaks,
                        );
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if in_v || in_f || in_is_t || in_cf_formula || in_odd_header || in_odd_footer {
                    let txt = quick_xml::escape::unescape(t.as_ref())
                        .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                    if in_v {
                        v_buf.push_str(&txt);
                    } else if in_f {
                        f_buf.push_str(&txt);
                    } else if in_is_t {
                        is_buf.push_str(&txt);
                    } else if in_cf_formula {
                        cf_formula_buf.push_str(&txt);
                    } else if in_odd_header || in_odd_footer {
                        hf_buf.push_str(&txt);
                    }
                }
            }
            Ok(Event::CData(c)) => {
                if in_v {
                    v_buf.push_str(c.as_ref());
                } else if in_is_t {
                    is_buf.push_str(c.as_ref());
                }
            }
            Ok(Event::End(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "v" => in_v = false,
                    "f" => {
                        in_f = false;
                        if let Some(si) = f_si {
                            if !f_buf.is_empty() {
                                shared_formulas.insert(si, f_buf.clone());
                            } else if let Some(master_f) = shared_formulas.get(&si) {
                                f_buf.clone_from(master_f);
                            }
                        }
                    }
                    "is" => in_is = false,
                    "rPh" => in_rph = false,
                    "t" if in_is_t => in_is_t = false,
                    "c" if in_cell => {
                        in_cell = false;
                        let val = decode_xlsx_cell_value(
                            &cell_type,
                            &v_buf,
                            &is_buf,
                            has_v,
                            shared_strings,
                        );
                        let formula = if has_f {
                            if !has_v {
                                wb.forensic.formulas_without_cached_value += 1;
                            }
                            Some(FormulaInfo {
                                expression: f_buf.clone(),
                                is_array: f_is_array,
                                ref_range: f_ref.take(),
                                has_cached_value: has_v,
                            })
                        } else {
                            None
                        };
                        if !matches!(val, CellValue::Empty)
                            || formula.is_some()
                            || cell_style_idx > 0
                        {
                            let cell = finalize_cell(val, formula, cell_style_idx, &fmt_opts, wb);
                            sheet.cells.insert((cur_row_idx, cur_col_idx), cell);
                        }
                        cur_col_idx = cur_col_idx.saturating_add(1);
                    }
                    "formula" if in_cf_formula => {
                        in_cf_formula = false;
                        cf_formulas.push(std::mem::take(&mut cf_formula_buf));
                    }
                    "cfRule" => {
                        if let Some(mut rule) = cur_cf_rule.take() {
                            finish_cf_rule(&mut rule, &cf_formulas, &cf_colors, in_data_bar, wb);
                            sheet.conditional_rules.push(rule);
                        }
                    }
                    "oddHeader" if in_odd_header => {
                        in_odd_header = false;
                        sheet.print_setup.header_text = Some(std::mem::take(&mut hf_buf));
                    }
                    "oddFooter" if in_odd_footer => {
                        in_odd_footer = false;
                        sheet.print_setup.footer_text = Some(std::mem::take(&mut hf_buf));
                    }
                    "rowBreaks" => in_row_breaks = false,
                    "colBreaks" => in_col_breaks = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    // Parse linked comments, threaded comments, tables, and drawings
    for (rid, rel) in &rels {
        if rel.external {
            continue;
        }
        let target_path = resolve_zip_path(part_path, &rel.target);
        if rel.rel_type.ends_with("/comments") {
            if let Ok(cxml) = read_zip_string(archive, &target_path, max_bytes) {
                parse_comments_xml(&cxml, distinct_authors, &mut sheet);
            }
        } else if rel.rel_type.contains("threadedComment") {
            if let Ok(tcxml) = read_zip_string(archive, &target_path, max_bytes) {
                parse_threaded_comments_xml(&tcxml, persons_map, distinct_authors, &mut sheet);
            }
        } else if (rel.rel_type.ends_with("/table") || table_rids.contains(rid))
            && let Ok(txml) = read_zip_string(archive, &target_path, max_bytes)
            && let Some(tbl) = parse_table_xml(&txml)
        {
            sheet.tables.push(tbl);
        } else if (rel.rel_type.ends_with("/drawing") || drawing_rids.contains(rid))
            && let Ok(dxml) = read_zip_string(archive, &target_path, max_bytes)
        {
            parse_worksheet_drawing(&dxml, &target_path, archive, max_bytes, &mut sheet, wb);
        }
    }

    Ok(sheet)
}

fn parse_row_attrs(e: &BytesStart<'_>, row_idx: u32, sheet: &mut Sheet, wb: &mut Workbook) {
    let ht = attr_val(e, "ht").and_then(|v| v.parse::<f32>().ok());
    let custom_height =
        attr_val(e, "customHeight").is_some_and(|v| matches!(v.as_str(), "1" | "true"));
    let hidden = attr_val(e, "hidden").is_some_and(|v| matches!(v.as_str(), "1" | "true"))
        || ht.is_some_and(|h| h <= 0.0);
    let style_idx = attr_val(e, "s").and_then(|v| v.parse::<u32>().ok());
    let outline_level = attr_val(e, "outlineLevel")
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(0);

    if hidden {
        wb.forensic.hidden_row_count += 1;
    }

    if ht.is_some() || hidden || style_idx.is_some() || outline_level > 0 {
        sheet.row_props.insert(
            row_idx,
            RowProps {
                height_pt: ht.unwrap_or(sheet.default_row_height_pt).clamp(0.0, 409.0),
                custom_height,
                hidden,
                style_idx,
                outline_level,
            },
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    clippy::too_many_lines
)]
fn handle_sheet_empty_or_start_tag(
    local: &str,
    e: &BytesStart<'_>,
    rels: &BTreeMap<String, RelEntry>,
    sheet: &mut Sheet,
    wb: &mut Workbook,
    drawing_rids: &mut Vec<String>,
    table_rids: &mut Vec<String>,
    cf_colors: &mut Vec<Rgba>,
    in_cf_rule: bool,
    in_row_breaks: bool,
    in_col_breaks: bool,
) {
    match local {
        "tabColor" => {
            sheet.tab_color = resolve_excel_color(e, &wb.theme);
        }
        "sheetView" => {
            if let Some(sg) = attr_val(e, "showGridLines") {
                sheet.show_gridlines = !matches!(sg.as_str(), "0" | "false");
            }
            if let Some(sh) = attr_val(e, "showRowColHeaders") {
                sheet.show_row_col_headers = !matches!(sh.as_str(), "0" | "false");
            }
        }
        "pane" => {
            let frozen_cols = attr_val(e, "xSplit")
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(0, |v| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        v.max(0.0) as u32
                    }
                });
            let frozen_rows = attr_val(e, "ySplit")
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(0, |v| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        v.max(0.0) as u32
                    }
                });
            sheet.pane = Some(PaneInfo {
                frozen_rows,
                frozen_cols,
                top_left_cell: attr_val(e, "topLeftCell"),
            });
        }
        "sheetFormatPr" => {
            if let Some(dcw) = attr_val(e, "defaultColWidth").and_then(|v| v.parse::<f32>().ok()) {
                sheet.default_col_width_pt = excel_chars_to_points(dcw);
            }
            if let Some(drh) = attr_val(e, "defaultRowHeight").and_then(|v| v.parse::<f32>().ok()) {
                sheet.default_row_height_pt = drh.clamp(4.0, 409.0);
            }
        }
        "col" => {
            let min_1 = attr_val(e, "min")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(1)
                .max(1);
            let max_1 = attr_val(e, "max")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(min_1)
                .max(min_1);
            let width_chars = attr_val(e, "width").and_then(|v| v.parse::<f32>().ok());
            let hidden = attr_val(e, "hidden").is_some_and(|v| matches!(v.as_str(), "1" | "true"))
                || width_chars.is_some_and(|w| w <= 0.0);
            let width_pt = width_chars.map_or(sheet.default_col_width_pt, excel_chars_to_points);
            let style_idx = attr_val(e, "style")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            let outline_level = attr_val(e, "outlineLevel")
                .and_then(|v| v.parse::<u8>().ok())
                .unwrap_or(0);

            if hidden {
                wb.forensic.hidden_col_count += (max_1 - min_1 + 1) as usize;
            }
            sheet.col_spans.push(ColumnSpan {
                min_col: min_1 - 1,
                max_col: max_1 - 1,
                width_pt,
                hidden,
                style_idx,
                outline_level,
            });
        }
        "mergeCell" => {
            if let Some(r_str) = attr_val(e, "ref")
                && let Some(rng) = parse_a1_range(&r_str)
            {
                sheet.merged_ranges.push(rng);
            }
        }
        "sheetProtection" => {
            sheet.protected = true;
            if !wb.forensic.protected_sheet_names.contains(&sheet.name) {
                wb.forensic.protected_sheet_names.push(sheet.name.clone());
            }
        }
        "autoFilter" if sheet.auto_filter.is_none() => {
            if let Some(r_str) = attr_val(e, "ref") {
                sheet.auto_filter = parse_a1_range(&r_str);
            }
        }
        "hyperlink" => {
            if let Some(r_str) = attr_val(e, "ref")
                && let Some((r, c)) = parse_a1_cell_ref(&r_str)
            {
                let target = attr_val(e, "r:id")
                    .or_else(|| attr_val(e, "id"))
                    .and_then(|rid| rels.get(&rid))
                    .map(|rel| rel.target.clone())
                    .or_else(|| attr_val(e, "location"));
                if let Some(t) = target
                    && let Some(cell) = sheet.cells.get_mut(&(r, c))
                {
                    cell.hyperlink = Some(t);
                }
            }
        }
        "printOptions" => {
            if let Some(gl) = attr_val(e, "gridLines") {
                sheet.print_setup.print_gridlines = matches!(gl.as_str(), "1" | "true");
            }
            if let Some(hd) = attr_val(e, "headings") {
                sheet.print_setup.print_headings = matches!(hd.as_str(), "1" | "true");
            }
        }
        "pageMargins" => {
            if let Some(v) = attr_val(e, "left").and_then(|s| s.parse::<f32>().ok()) {
                sheet.print_setup.margin_left_pt = (v * 72.0).clamp(0.0, 216.0);
            }
            if let Some(v) = attr_val(e, "right").and_then(|s| s.parse::<f32>().ok()) {
                sheet.print_setup.margin_right_pt = (v * 72.0).clamp(0.0, 216.0);
            }
            if let Some(v) = attr_val(e, "top").and_then(|s| s.parse::<f32>().ok()) {
                sheet.print_setup.margin_top_pt = (v * 72.0).clamp(0.0, 216.0);
            }
            if let Some(v) = attr_val(e, "bottom").and_then(|s| s.parse::<f32>().ok()) {
                sheet.print_setup.margin_bottom_pt = (v * 72.0).clamp(0.0, 216.0);
            }
            if let Some(v) = attr_val(e, "header").and_then(|s| s.parse::<f32>().ok()) {
                sheet.print_setup.margin_header_pt = (v * 72.0).clamp(0.0, 216.0);
            }
            if let Some(v) = attr_val(e, "footer").and_then(|s| s.parse::<f32>().ok()) {
                sheet.print_setup.margin_footer_pt = (v * 72.0).clamp(0.0, 216.0);
            }
        }
        "pageSetup" => {
            if attr_val(e, "orientation").as_deref() == Some("landscape") {
                sheet.print_setup.orientation = PageOrientation::Landscape;
            }
            if let Some(sc) = attr_val(e, "scale").and_then(|s| s.parse::<u16>().ok()) {
                sheet.print_setup.scale_pct = sc.clamp(10, 400);
            }
            if let Some(fw) = attr_val(e, "fitToWidth").and_then(|s| s.parse::<u16>().ok()) {
                sheet.print_setup.fit_to_width = Some(fw);
            }
            if let Some(fh) = attr_val(e, "fitToHeight").and_then(|s| s.parse::<u16>().ok()) {
                sheet.print_setup.fit_to_height = Some(fh);
            }
        }
        "brk" => {
            if let Some(id_1) = attr_val(e, "id").and_then(|s| s.parse::<u32>().ok())
                && id_1 >= 1
            {
                if in_row_breaks {
                    sheet.print_setup.row_breaks.push(id_1 - 1);
                } else if in_col_breaks {
                    sheet.print_setup.col_breaks.push(id_1 - 1);
                }
            }
        }
        "drawing" => {
            if let Some(rid) = attr_val(e, "r:id").or_else(|| attr_val(e, "id")) {
                drawing_rids.push(rid);
            }
        }
        "tablePart" => {
            if let Some(rid) = attr_val(e, "r:id").or_else(|| attr_val(e, "id")) {
                table_rids.push(rid);
            }
        }
        "color" if in_cf_rule => {
            if let Some(c) = resolve_excel_color(e, &wb.theme) {
                cf_colors.push(c);
            }
        }
        _ => {}
    }
}

/// Convert Excel column width in character units (`0..=255`) into typographic points.
#[must_use]
pub fn excel_chars_to_points(chars: f32) -> f32 {
    if chars <= 0.0 {
        0.0
    } else {
        // 1 character in 11pt Calibri ≈ 7 pixels at 96 DPI (+5 px padding) = 5.25 pt/char + 3.75 pt
        (chars * 5.25 + 3.75).clamp(4.0, 1200.0)
    }
}

fn decode_xlsx_cell_value(
    cell_type: &str,
    v_buf: &str,
    is_buf: &str,
    has_v: bool,
    shared_strings: &[SharedStringItem],
) -> CellValue {
    match cell_type {
        "s" => {
            let Some(idx) = v_buf.trim().parse::<usize>().ok() else {
                return CellValue::Empty;
            };
            match shared_strings.get(idx) {
                Some(SharedStringItem::Plain(s)) => CellValue::Text(s.clone()),
                Some(SharedStringItem::Rich(runs)) => CellValue::RichText(runs.clone()),
                None => CellValue::Empty,
            }
        }
        "inlineStr" => CellValue::Text(is_buf.to_string()),
        "str" => CellValue::Text(v_buf.to_string()),
        "b" => CellValue::Bool(matches!(v_buf.trim(), "1" | "true" | "TRUE")),
        "e" => CellValue::Error(v_buf.trim().to_string()),
        "d" => CellValue::IsoDate(v_buf.trim().to_string()),
        _ => {
            if !has_v || v_buf.trim().is_empty() {
                CellValue::Empty
            } else if let Ok(n) = v_buf.trim().parse::<f64>() {
                CellValue::Number(n)
            } else {
                CellValue::Text(v_buf.to_string())
            }
        }
    }
}

/// Format a [`CellValue`] with its resolved number format and check for forensic concealment.
pub fn finalize_cell(
    value: CellValue,
    formula: Option<FormulaInfo>,
    style_idx: u32,
    fmt_opts: &FormatOptions,
    wb: &mut Workbook,
) -> Cell {
    let styles = &wb.styles;
    let num_fmt_id = styles.xf(style_idx).map_or(0, |xf| xf.num_fmt_id);
    let custom_code = styles.num_fmts.get(&num_fmt_id).map(String::as_str);

    let input = match &value {
        CellValue::Empty => CellInputValue::Empty,
        CellValue::Number(n) => CellInputValue::Number(*n),
        CellValue::Text(s) | CellValue::IsoDate(s) => CellInputValue::Text(s.clone()),
        CellValue::RichText(runs) => {
            CellInputValue::Text(runs.iter().map(|r| r.text.as_str()).collect())
        }
        CellValue::Bool(b) => CellInputValue::Bool(*b),
        CellValue::Error(e) => CellInputValue::Error(e.clone()),
    };

    let formatted = format_cell_value(&input, Some(u32::from(num_fmt_id)), custom_code, fmt_opts);
    let num_fmt_code = custom_code
        .map(str::to_string)
        .or_else(|| prism_numfmt::builtin_format_code(u32::from(num_fmt_id)).map(str::to_string))
        .unwrap_or_else(|| "General".to_string());

    let display_color = formatted.color.as_deref().and_then(parse_hex_rgba);

    // Check if cell has non-empty content concealed by `;;;` or matching font/fill color
    let has_non_empty_content = !matches!(value, CellValue::Empty);
    let same_font_and_fill = if let (Some(font), Some(fill_c)) = (
        styles.font_for_style(style_idx),
        styles.fill_color_for_style(style_idx),
    ) && let Some(font_c) = font.color
    {
        font_c.r == fill_c.r && font_c.g == fill_c.g && font_c.b == fill_c.b
    } else {
        false
    };

    let is_concealed = has_non_empty_content
        && (formatted.is_concealed || is_concealed_format(&num_fmt_code) || same_font_and_fill);
    if is_concealed {
        wb.forensic.concealed_cell_count += 1;
    }

    Cell {
        value,
        formula,
        style_idx,
        display_text: formatted.text,
        num_fmt_code,
        display_color,
        is_concealed,
        hyperlink: None,
    }
}

fn start_cf_rule(e: &BytesStart<'_>, ranges: &[CellRange]) -> ConditionalFormatRule {
    let rule_type = attr_val(e, "type").unwrap_or_else(|| "cellIs".to_string());
    let priority = attr_val(e, "priority")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(100);
    let stop_if_true =
        attr_val(e, "stopIfTrue").is_some_and(|v| matches!(v.as_str(), "1" | "true"));
    let dxf_id = attr_val(e, "dxfId").and_then(|v| v.parse::<u32>().ok());
    let op_str = attr_val(e, "operator").unwrap_or_else(|| "equal".to_string());

    let kind = match rule_type.as_str() {
        "cellIs" => {
            let operator = match op_str.as_str() {
                "lessThan" => CondOperator::LessThan,
                "lessThanOrEqual" => CondOperator::LessThanOrEqual,
                "notEqual" => CondOperator::NotEqual,
                "greaterThanOrEqual" => CondOperator::GreaterThanOrEqual,
                "greaterThan" => CondOperator::GreaterThan,
                "between" => CondOperator::Between,
                "notBetween" => CondOperator::NotBetween,
                _ => CondOperator::Equal,
            };
            ConditionalRuleKind::CellIs {
                operator,
                formula1: String::new(),
                formula2: None,
            }
        }
        "containsText" | "notContainsText" | "beginsWith" | "endsWith" => {
            ConditionalRuleKind::TextMatch {
                mode: rule_type,
                text: attr_val(e, "text").unwrap_or_default(),
            }
        }
        "containsBlanks" => ConditionalRuleKind::BlankCheck { is_blank: true },
        "notContainsBlanks" => ConditionalRuleKind::BlankCheck { is_blank: false },
        "containsErrors" => ConditionalRuleKind::ErrorCheck { is_error: true },
        "notContainsErrors" => ConditionalRuleKind::ErrorCheck { is_error: false },
        "duplicateValues" => ConditionalRuleKind::DuplicateUnique { duplicate: true },
        "uniqueValues" => ConditionalRuleKind::DuplicateUnique { duplicate: false },
        "top10" => ConditionalRuleKind::Top10 {
            rank: attr_val(e, "rank")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(10),
            percent: attr_val(e, "percent").is_some_and(|v| matches!(v.as_str(), "1" | "true")),
            bottom: attr_val(e, "bottom").is_some_and(|v| matches!(v.as_str(), "1" | "true")),
        },
        "aboveAverage" => ConditionalRuleKind::AboveAverage {
            above: attr_val(e, "aboveAverage").is_none_or(|v| !matches!(v.as_str(), "0" | "false")),
            equal_average: attr_val(e, "equalAverage")
                .is_some_and(|v| matches!(v.as_str(), "1" | "true")),
        },
        "colorScale" => ConditionalRuleKind::ColorScale {
            min_color: Rgba::rgb(0xF8, 0x69, 0x6B),
            mid_color: None,
            max_color: Rgba::rgb(0x63, 0xBE, 0x7B),
        },
        "dataBar" => ConditionalRuleKind::DataBar {
            color: Rgba::rgb(0x63, 0x8E, 0xC6),
            show_value: attr_val(e, "showValue")
                .is_none_or(|v| !matches!(v.as_str(), "0" | "false")),
        },
        "iconSet" => ConditionalRuleKind::IconSet {
            icon_set: attr_val(e, "iconSet").unwrap_or_else(|| "3Arrows".to_string()),
            reverse: attr_val(e, "reverse").is_some_and(|v| matches!(v.as_str(), "1" | "true")),
            show_value: attr_val(e, "showValue")
                .is_none_or(|v| !matches!(v.as_str(), "0" | "false")),
        },
        _ => ConditionalRuleKind::Expression {
            formula: String::new(),
        },
    };

    ConditionalFormatRule {
        ranges: ranges.to_vec(),
        priority,
        stop_if_true,
        dxf_id,
        kind,
    }
}

fn finish_cf_rule(
    rule: &mut ConditionalFormatRule,
    formulas: &[String],
    colors: &[Rgba],
    _in_data_bar: bool,
    wb: &mut Workbook,
) {
    match &mut rule.kind {
        ConditionalRuleKind::CellIs {
            formula1, formula2, ..
        } => {
            if let Some(f1) = formulas.first() {
                formula1.clone_from(f1);
            }
            if let Some(f2) = formulas.get(1) {
                *formula2 = Some(f2.clone());
            }
        }
        ConditionalRuleKind::TextMatch { text, .. } => {
            if text.is_empty()
                && let Some(f1) = formulas.first()
            {
                text.clone_from(f1);
            }
        }
        ConditionalRuleKind::ColorScale {
            min_color,
            mid_color,
            max_color,
        } => {
            if colors.len() >= 3 {
                *min_color = colors[0];
                *mid_color = Some(colors[1]);
                *max_color = colors[2];
            } else if colors.len() == 2 {
                *min_color = colors[0];
                *max_color = colors[1];
            }
        }
        ConditionalRuleKind::DataBar { color, .. } => {
            if let Some(&c) = colors.first() {
                *color = c;
            }
        }
        ConditionalRuleKind::Expression { formula } => {
            if let Some(f1) = formulas.first() {
                formula.clone_from(f1);
            }
            wb.forensic.unevaluated_conditional_rules += 1;
        }
        _ => {}
    }
}

fn parse_comments_xml(xml: &str, distinct_authors: &mut BTreeSet<String>, sheet: &mut Sheet) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut authors: Vec<String> = Vec::new();
    let mut in_author = false;
    let mut author_buf = String::new();

    let mut in_comment = false;
    let mut cur_ref: Option<(u32, u32)> = None;
    let mut cur_author_id: usize = 0;
    let mut in_t = false;
    let mut text_buf = String::new();

    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "author" => {
                        in_author = true;
                        author_buf.clear();
                    }
                    "comment" => {
                        in_comment = true;
                        cur_ref = attr_val(e, "ref").and_then(|r| parse_a1_cell_ref(&r));
                        cur_author_id = attr_val(e, "authorId")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        text_buf.clear();
                    }
                    "t" if in_comment => {
                        in_t = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if in_author || in_t {
                    let txt = quick_xml::escape::unescape(t.as_ref())
                        .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                    if in_author {
                        author_buf.push_str(&txt);
                    } else if in_t {
                        text_buf.push_str(&txt);
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "author" if in_author => {
                        in_author = false;
                        let a = author_buf.trim().to_string();
                        if !a.is_empty() {
                            distinct_authors.insert(a.clone());
                        }
                        authors.push(a);
                    }
                    "t" if in_t => in_t = false,
                    "comment" if in_comment => {
                        in_comment = false;
                        if let Some((r, c)) = cur_ref.take() {
                            let author = authors
                                .get(cur_author_id)
                                .cloned()
                                .unwrap_or_else(|| "Author".to_string());
                            sheet.comments.insert(
                                (r, c),
                                SheetComment {
                                    author,
                                    text: text_buf.trim().to_string(),
                                    replies: Vec::new(),
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn parse_persons_xml(xml: &str, persons: &mut BTreeMap<String, String>) {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let qn = e.name();
                if local_name(qn.as_ref()) == "person"
                    && let (Some(id), Some(name)) = (attr_val(e, "id"), attr_val(e, "displayName"))
                {
                    persons.insert(id, name);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn parse_threaded_comments_xml(
    xml: &str,
    persons: &BTreeMap<String, String>,
    distinct_authors: &mut BTreeSet<String>,
    sheet: &mut Sheet,
) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut cur_ref: Option<(u32, u32)> = None;
    let mut cur_author = String::new();
    let mut in_text = false;
    let mut text_buf = String::new();

    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "threadedComment" => {
                        cur_ref = attr_val(e, "ref").and_then(|r| parse_a1_cell_ref(&r));
                        let pid = attr_val(e, "personId").unwrap_or_default();
                        cur_author = persons
                            .get(&pid)
                            .cloned()
                            .unwrap_or_else(|| "Reviewer".to_string());
                        distinct_authors.insert(cur_author.clone());
                        text_buf.clear();
                    }
                    "text" => {
                        in_text = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_text => {
                let txt = quick_xml::escape::unescape(t.as_ref())
                    .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                text_buf.push_str(&txt);
            }
            Ok(Event::End(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "text" => in_text = false,
                    "threadedComment" => {
                        if let Some((r, c)) = cur_ref.take() {
                            let msg = text_buf.trim().to_string();
                            sheet
                                .comments
                                .entry((r, c))
                                .and_modify(|existing| {
                                    existing.replies.push(format!("{cur_author}: {msg}"));
                                })
                                .or_insert_with(|| SheetComment {
                                    author: cur_author.clone(),
                                    text: msg,
                                    replies: Vec::new(),
                                });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn parse_table_xml(xml: &str) -> Option<SheetTable> {
    let mut reader = Reader::from_str(xml);
    let mut tbl = SheetTable::default();
    let mut has_table = false;

    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "table" => {
                        has_table = true;
                        tbl.name = attr_val(e, "name").unwrap_or_default();
                        tbl.display_name =
                            attr_val(e, "displayName").unwrap_or_else(|| tbl.name.clone());
                        if let Some(r_str) = attr_val(e, "ref")
                            && let Some(rng) = parse_a1_range(&r_str)
                        {
                            tbl.range = rng;
                        }
                    }
                    "autoFilter" => {
                        tbl.has_autofilter = true;
                    }
                    "tableColumn" => {
                        if let Some(cn) = attr_val(e, "name") {
                            tbl.columns.push(cn);
                        }
                    }
                    "tableStyleInfo" => {
                        tbl.style_name = attr_val(e, "name");
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    if has_table { Some(tbl) } else { None }
}

#[allow(clippy::too_many_lines)]
fn parse_worksheet_drawing(
    xml: &str,
    drawing_path: &str,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    sheet: &mut Sheet,
    wb: &mut Workbook,
) {
    let rels = parse_part_rels(archive, drawing_path, max_bytes);
    record_external_rels(&rels, wb);

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut in_anchor = false;
    let mut cur_anchor = DrawingAnchor {
        from_row: 0,
        from_col: 0,
        from_col_off_pt: 0.0,
        from_row_off_pt: 0.0,
        to_row: None,
        to_col: None,
        to_col_off_pt: 0.0,
        to_row_off_pt: 0.0,
        width_pt: 300.0,
        height_pt: 180.0,
    };
    let mut in_from = false;
    let mut in_to = false;
    let mut coord_tag = String::new();

    let mut cur_name = String::new();
    let mut cur_pic_rid: Option<String> = None;
    let mut cur_chart_rid: Option<String> = None;
    let mut cur_preset_geom: Option<String> = None;
    let mut cur_shape_text = String::new();
    let mut in_shape_t = false;

    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e)) => {
                let qn = e.name();
                let local = local_name(qn.as_ref());
                match local {
                    "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor" => {
                        in_anchor = true;
                        cur_anchor = DrawingAnchor {
                            from_row: 0,
                            from_col: 0,
                            from_col_off_pt: 0.0,
                            from_row_off_pt: 0.0,
                            to_row: None,
                            to_col: None,
                            to_col_off_pt: 0.0,
                            to_row_off_pt: 0.0,
                            width_pt: 300.0,
                            height_pt: 180.0,
                        };
                        cur_name.clear();
                        cur_pic_rid = None;
                        cur_chart_rid = None;
                        cur_preset_geom = None;
                        cur_shape_text.clear();
                    }
                    "from" if in_anchor => in_from = true,
                    "to" if in_anchor => in_to = true,
                    "col" | "colOff" | "row" | "rowOff" if in_from || in_to => {
                        coord_tag = local.to_string();
                    }
                    "t" if in_anchor => in_shape_t = true,
                    other if in_anchor => {
                        handle_drawing_leaf(
                            other,
                            e,
                            &mut cur_anchor,
                            &mut cur_name,
                            &mut cur_pic_rid,
                            &mut cur_chart_rid,
                            &mut cur_preset_geom,
                        );
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) if in_anchor => {
                let qn = e.name();
                handle_drawing_leaf(
                    local_name(qn.as_ref()),
                    e,
                    &mut cur_anchor,
                    &mut cur_name,
                    &mut cur_pic_rid,
                    &mut cur_chart_rid,
                    &mut cur_preset_geom,
                );
            }
            Ok(Event::Text(t)) if in_anchor => {
                let txt = quick_xml::escape::unescape(t.as_ref())
                    .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                if (in_from || in_to) && !coord_tag.is_empty() {
                    if let Ok(v) = txt.trim().parse::<f32>() {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        match (in_from, coord_tag.as_str()) {
                            (true, "col") => cur_anchor.from_col = v.max(0.0) as u32,
                            (true, "row") => cur_anchor.from_row = v.max(0.0) as u32,
                            (true, "colOff") => cur_anchor.from_col_off_pt = v / EMU_PER_POINT,
                            (true, "rowOff") => cur_anchor.from_row_off_pt = v / EMU_PER_POINT,
                            (false, "col") => cur_anchor.to_col = Some(v.max(0.0) as u32),
                            (false, "row") => cur_anchor.to_row = Some(v.max(0.0) as u32),
                            (false, "colOff") => cur_anchor.to_col_off_pt = v / EMU_PER_POINT,
                            (false, "rowOff") => cur_anchor.to_row_off_pt = v / EMU_PER_POINT,
                            _ => {}
                        }
                    }
                } else if in_shape_t {
                    cur_shape_text.push_str(&txt);
                }
            }
            Ok(Event::End(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "from" => in_from = false,
                    "to" => in_to = false,
                    "col" | "colOff" | "row" | "rowOff" => coord_tag.clear(),
                    "t" => in_shape_t = false,
                    "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor" if in_anchor => {
                        in_anchor = false;
                        let obj_id = format!("d{}", sheet.drawings.len());
                        if let Some(ref rid) = cur_chart_rid
                            && let Some(rel) = rels.get(rid)
                            && !rel.external
                        {
                            let chart_path = resolve_zip_path(drawing_path, &rel.target);
                            if let Ok(cxml) = read_zip_string(archive, &chart_path, max_bytes)
                                && let Ok(chart) = parse_chart_xml(&cxml)
                            {
                                if let Some(ref ext) = chart.external_data_ref
                                    && !wb.forensic.external_links.contains(ext)
                                {
                                    wb.forensic.external_links.push(ext.clone());
                                }
                                sheet.drawings.push(AnchoredDrawing {
                                    id: obj_id,
                                    name: if cur_name.is_empty() {
                                        "Chart".to_string()
                                    } else {
                                        cur_name.clone()
                                    },
                                    anchor: cur_anchor.clone(),
                                    kind: WorksheetDrawingKind::Chart(Box::new(chart)),
                                });
                            }
                        } else if let Some(ref rid) = cur_pic_rid
                            && let Some(res_id) =
                                load_xlsx_image(rid, drawing_path, &rels, archive, max_bytes, wb)
                        {
                            sheet.drawings.push(AnchoredDrawing {
                                id: obj_id,
                                name: if cur_name.is_empty() {
                                    "Picture".to_string()
                                } else {
                                    cur_name.clone()
                                },
                                anchor: cur_anchor.clone(),
                                kind: WorksheetDrawingKind::Image {
                                    resource_id: res_id,
                                },
                            });
                        } else if let Some(ref prst) = cur_preset_geom {
                            sheet.drawings.push(AnchoredDrawing {
                                id: obj_id,
                                name: if cur_name.is_empty() {
                                    "Shape".to_string()
                                } else {
                                    cur_name.clone()
                                },
                                anchor: cur_anchor.clone(),
                                kind: WorksheetDrawingKind::Shape {
                                    preset_geom: prst.clone(),
                                    text: cur_shape_text.clone(),
                                    fill_color: Some(Rgba::rgb(0x4F, 0x81, 0xBD)),
                                    stroke_color: Some(Rgba::rgb(0x38, 0x5D, 0x8A)),
                                },
                            });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn handle_drawing_leaf(
    local: &str,
    e: &BytesStart<'_>,
    cur_anchor: &mut DrawingAnchor,
    cur_name: &mut String,
    cur_pic_rid: &mut Option<String>,
    cur_chart_rid: &mut Option<String>,
    cur_preset_geom: &mut Option<String>,
) {
    match local {
        "cNvPr" => {
            if cur_name.is_empty()
                && let Some(n) = attr_val(e, "name")
            {
                *cur_name = n;
            }
        }
        "ext" => {
            if let Some(cx) = attr_val(e, "cx").and_then(|v| v.parse::<f32>().ok()) {
                cur_anchor.width_pt = (cx / EMU_PER_POINT).max(8.0);
            }
            if let Some(cy) = attr_val(e, "cy").and_then(|v| v.parse::<f32>().ok()) {
                cur_anchor.height_pt = (cy / EMU_PER_POINT).max(8.0);
            }
        }
        "blip" => {
            *cur_pic_rid = attr_val(e, "r:embed").or_else(|| attr_val(e, "embed"));
        }
        "chart" => {
            *cur_chart_rid = attr_val(e, "r:id").or_else(|| attr_val(e, "id"));
        }
        "prstGeom" => {
            *cur_preset_geom = attr_val(e, "prst");
        }
        _ => {}
    }
}

fn load_xlsx_image(
    rid: &str,
    base_part: &str,
    rels: &BTreeMap<String, RelEntry>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    max_bytes: usize,
    wb: &mut Workbook,
) -> Option<String> {
    let rel = rels.get(rid)?;
    if rel.external {
        return None;
    }
    let zip_path = resolve_zip_path(base_part, &rel.target);
    if wb.images.contains_key(&zip_path) {
        return Some(zip_path);
    }
    let data = read_zip_bytes(archive, &zip_path, max_bytes).ok()?;
    let (width, height) = image::ImageReader::new(Cursor::new(&data))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((100, 100));
    let mime_type = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png".to_string()
    } else if data.starts_with(b"\xFF\xD8\xFF") {
        "image/jpeg".to_string()
    } else if data.starts_with(b"GIF8") {
        "image/gif".to_string()
    } else {
        "image/png".to_string()
    };
    wb.images.insert(
        zip_path.clone(),
        ImageResource {
            id: zip_path.clone(),
            mime_type,
            data: Some(data),
            url: None,
            width,
            height,
        },
    );
    Some(zip_path)
}

fn parse_core_props(xml: &str, wb: &mut Workbook) {
    let mut reader = Reader::from_str(xml);
    let mut in_title = false;
    let mut in_creator = false;
    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e)) => {
                let qn = e.name();
                match local_name(qn.as_ref()) {
                    "title" => in_title = true,
                    "creator" => in_creator = true,
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_title || in_creator => {
                let txt = quick_xml::escape::unescape(t.as_ref())
                    .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                let trimmed = txt.trim();
                if !trimmed.is_empty() {
                    if in_title {
                        wb.title = Some(trimmed.to_string());
                    } else if in_creator {
                        wb.author = Some(trimmed.to_string());
                    }
                }
            }
            Ok(Event::End(_)) => {
                in_title = false;
                in_creator = false;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn parse_part_rels(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    part_path: &str,
    max_bytes: usize,
) -> BTreeMap<String, RelEntry> {
    let rels_path = if let Some((dir, file)) = part_path.rsplit_once('/') {
        format!("{dir}/_rels/{file}.rels")
    } else {
        format!("_rels/{part_path}.rels")
    };
    let Ok(xml) = read_zip_string(archive, &rels_path, max_bytes) else {
        return BTreeMap::new();
    };
    let mut map = BTreeMap::new();
    let mut reader = Reader::from_str(&xml);
    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let qn = e.name();
                if local_name(qn.as_ref()) == "Relationship"
                    && let (Some(id), Some(target)) = (attr_val(e, "Id"), attr_val(e, "Target"))
                {
                    let external = attr_val(e, "TargetMode")
                        .is_some_and(|m| m.eq_ignore_ascii_case("External"));
                    let rel_type = attr_val(e, "Type").unwrap_or_default();
                    map.insert(
                        id,
                        RelEntry {
                            rel_type,
                            target,
                            external,
                        },
                    );
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    map
}

fn record_external_rels(rels: &BTreeMap<String, RelEntry>, wb: &mut Workbook) {
    for rel in rels.values() {
        if rel.external && !wb.forensic.external_links.contains(&rel.target) {
            wb.forensic.external_links.push(rel.target.clone());
        }
    }
}

fn extract_external_targets(xml: &str, wb: &mut Workbook) {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().map(resolve_general_ref) {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                for key in ["Target", "url", "connectionFile", "sourceFile"] {
                    if let Some(val) = attr_val(e, key)
                        && !val.is_empty()
                        && !wb.forensic.external_links.contains(&val)
                    {
                        wb.forensic.external_links.push(val);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn resolve_zip_path(base_part: &str, target: &str) -> String {
    let t = target.trim_start_matches('/');
    if target.starts_with('/') || t.starts_with("xl/") {
        return normalize_path(t);
    }
    let base_dir = base_part.rsplit_once('/').map_or("", |(d, _)| d);
    let combined = if base_dir.is_empty() {
        t.to_string()
    } else {
        format!("{base_dir}/{t}")
    };
    normalize_path(&combined)
}

fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn read_zip_bytes(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut file = archive
        .by_name(name)
        .map_err(|e| Error::ParseError(format!("Missing ZIP entry {name}: {e}")))?;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| Error::ParseError(format!("Read error in {name}: {e}")))?;
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

fn read_zip_string(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    max_bytes: usize,
) -> Result<String> {
    let bytes = read_zip_bytes(archive, name, max_bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn local_name(qname: &str) -> &str {
    qname.rsplit(':').next().unwrap_or(qname)
}

fn attr_val(e: &BytesStart<'_>, want: &str) -> Option<String> {
    let want_local = local_name(want);
    for attr in e.attributes().flatten() {
        let key = attr.key.as_ref();
        if key == want || local_name(key) == want_local {
            let raw = attr.value.as_ref();
            return Some(
                quick_xml::escape::unescape(raw)
                    .map_or_else(|_| raw.to_owned(), std::borrow::Cow::into_owned),
            );
        }
    }
    None
}

fn is_xml_ext(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
}
