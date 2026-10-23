// SPDX-License-Identifier: AGPL-3.0-only
//! Excel print layout engine (`Workbook` → `LayoutDocument`) and standalone drawing/chart renderer.

use crate::condfmt::{ResolvedCellConditional, evaluate_sheet_conditionals};
use crate::model::{
    AnchoredDrawing, CellHAlign, CellRange, CellValue, CellXf, PageOrientation, Sheet,
    SheetVisibility, Workbook, WorksheetDrawingKind, col_index_to_letters,
};
use prism_core::error::{Error, Result};
use prism_drawingml::{build_preset_geometry_paths, render_chart};
use prism_layout::{
    Affine, FillPaint, FillRule, FontSystem, ImageId, ImageItem, ImageStore, Item, LayoutDocument,
    LayoutOptions, LayoutPage, LineCap, LineJoin, MarkupMode, ParagraphSpec, PathCommand, PathItem,
    Point, Rect, Rgba, Size, SpanSpec, StrokeStyle, TextAlign, TextLayoutEngine,
};
use std::collections::{BTreeMap, BTreeSet};

/// Last 0-based row of an Excel worksheet (row 1,048,576).
const MAX_ROW_INDEX: u32 = 1_048_575;
/// Last 0-based column of an Excel worksheet (column `XFD`).
const MAX_COL_INDEX: u32 = 16_383;

/// Lay out a [`Workbook`] into a paginated [`LayoutDocument`] honoring print areas,
/// repeating title rows/columns, conditional formatting, charts, and forensic visibility options.
///
/// # Errors
///
/// Returns an error if layout fails.
#[allow(clippy::too_many_lines)]
pub fn layout_workbook(wb: &Workbook, options: &LayoutOptions) -> Result<LayoutDocument> {
    let mut font_sys = FontSystem::new(options);
    let mut text_engine = TextLayoutEngine::default();
    let mut images = ImageStore::new();
    let mut image_id_map: BTreeMap<String, ImageId> = BTreeMap::new();

    for (id, img) in &wb.images {
        if let Some(ref raw) = img.data {
            let w = img.width.max(1);
            let h = img.height.max(1);
            if let Ok(img_id) = images.register(&img.mime_type, w, h, raw.clone(), options) {
                image_id_map.insert(id.clone(), img_id);
            }
        }
    }

    let mut pages = Vec::new();
    let mut page_idx: u32 = 0;

    for sheet in &wb.sheets {
        if sheet.visibility != SheetVisibility::Visible && !options.show_hidden_text {
            continue;
        }
        let sheet_pages = layout_single_sheet(
            wb,
            sheet,
            options,
            options.max_pages.saturating_sub(pages.len()),
            &image_id_map,
            &mut page_idx,
            &mut font_sys,
            &mut text_engine,
        )?;
        pages.extend(sheet_pages);
    }

    if pages.is_empty() {
        let mut p = LayoutPage::new(0, Size::new(612.0, 792.0));
        p.label = Some("Sheet1".to_string());
        pages.push(p);
    }

    Ok(LayoutDocument {
        pages,
        fonts: font_sys.store,
        images,
        font_report: font_sys.report,
    })
}

/// Render a single [`AnchoredDrawing`] (chart, image, or shape) into a standalone 1-page [`LayoutDocument`]
/// for `prism_workbook_render_object`.
///
/// # Errors
///
/// Returns an error if rendering fails.
pub fn render_worksheet_drawing_to_doc(
    wb: &Workbook,
    sheet: &Sheet,
    drawing: &AnchoredDrawing,
) -> Result<LayoutDocument> {
    let opts = LayoutOptions::default();
    let mut font_sys = FontSystem::new(&opts);
    let mut text_engine = TextLayoutEngine::default();
    let mut images = ImageStore::new();
    let mut image_id_map: BTreeMap<String, ImageId> = BTreeMap::new();

    for (id, img) in &wb.images {
        if let Some(ref raw) = img.data {
            let w = img.width.max(1);
            let h = img.height.max(1);
            if let Ok(img_id) = images.register(&img.mime_type, w, h, raw.clone(), &opts) {
                image_id_map.insert(id.clone(), img_id);
            }
        }
    }

    let (_, _, w_pt, h_pt) = compute_drawing_bounds_on_sheet(sheet, drawing, false);
    let width_pt = w_pt.clamp(80.0, 1600.0);
    let height_pt = h_pt.clamp(60.0, 1200.0);
    let rect = Rect::new(0.0, 0.0, width_pt, height_pt);

    let items = render_drawing_items(
        drawing,
        rect,
        &wb.theme,
        &image_id_map,
        0,
        &mut font_sys,
        &mut text_engine,
    );

    let mut page = LayoutPage::new(0, Size::new(width_pt, height_pt));
    page.items = items;

    Ok(LayoutDocument {
        pages: vec![page],
        fonts: font_sys.store,
        images,
        font_report: font_sys.report,
    })
}

/// Compute `(x_pt, y_pt, width_pt, height_pt)` in sheet grid coordinates for an [`AnchoredDrawing`].
#[must_use]
pub fn compute_drawing_bounds_on_sheet(
    sheet: &Sheet,
    drawing: &AnchoredDrawing,
    include_hidden: bool,
) -> (f32, f32, f32, f32) {
    let col_x = |target_col: u32| -> f32 {
        let mut x = 0.0;
        for c in 0..target_col.min(2048) {
            let (w, hidden) = sheet.col_width_and_hidden(c);
            if !hidden || include_hidden {
                x += w.max(if hidden { 24.0 } else { 0.0 });
            }
        }
        x
    };
    let row_y = |target_row: u32| -> f32 {
        let mut y = 0.0;
        for r in 0..target_row.min(100_000) {
            let (h, hidden) = sheet.row_height_and_hidden(r);
            if !hidden || include_hidden {
                y += h.max(if hidden { 15.0 } else { 0.0 });
            }
        }
        y
    };

    let x1 = col_x(drawing.anchor.from_col) + drawing.anchor.from_col_off_pt;
    let y1 = row_y(drawing.anchor.from_row) + drawing.anchor.from_row_off_pt;

    let (w, h) = if let (Some(to_c), Some(to_r)) = (drawing.anchor.to_col, drawing.anchor.to_row) {
        let x2 = col_x(to_c) + drawing.anchor.to_col_off_pt;
        let y2 = row_y(to_r) + drawing.anchor.to_row_off_pt;
        (
            (x2 - x1).max(drawing.anchor.width_pt.min(40.0)).max(24.0),
            (y2 - y1).max(drawing.anchor.height_pt.min(24.0)).max(20.0),
        )
    } else {
        (
            drawing.anchor.width_pt.max(24.0),
            drawing.anchor.height_pt.max(20.0),
        )
    };

    (x1, y1, w, h)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]
fn layout_single_sheet(
    wb: &Workbook,
    sheet: &Sheet,
    options: &LayoutOptions,
    max_pages: usize,
    image_id_map: &BTreeMap<String, ImageId>,
    page_idx: &mut u32,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
) -> Result<Vec<LayoutPage>> {
    let ps = &sheet.print_setup;
    let (page_w, page_h) = match ps.orientation {
        PageOrientation::Portrait => (ps.paper_width_pt, ps.paper_height_pt),
        PageOrientation::Landscape => (
            ps.paper_height_pt.max(ps.paper_width_pt),
            ps.paper_width_pt.min(ps.paper_height_pt),
        ),
    };

    let area = if let Some(pa) = ps.print_area {
        pa
    } else if let Some((max_r, max_c)) = sheet.used_bounds() {
        // The whole used range: pages with nothing on them are skipped below, as Excel does.
        CellRange {
            start_row: 0,
            start_col: 0,
            end_row: max_r,
            end_col: max_c,
        }
    } else {
        if max_pages == 0 {
            return Err(too_many_pages(options.max_pages));
        }
        let mut p = LayoutPage::new(*page_idx, Size::new(page_w, page_h));
        p.label = Some(sheet.name.clone());
        emit_sheet_header_banner(
            sheet,
            wb,
            1,
            1,
            page_w,
            page_h,
            *page_idx,
            font_sys,
            text_engine,
            &mut p.items,
        );
        *page_idx = page_idx.saturating_add(1);
        return Ok(vec![p]);
    };

    // Excel's grid is 1,048,576 rows × 16,384 columns. References past it (only possible in
    // hand-made files) stay in the model and the extracted text, but are not paginated.
    let area = CellRange {
        end_row: area.end_row.min(MAX_ROW_INDEX),
        end_col: area.end_col.min(MAX_COL_INDEX),
        ..area
    };

    let cond_map = evaluate_sheet_conditionals(sheet, &wb.styles);
    let show_hidden = options.show_hidden_text;

    let mut visible_cols: Vec<(u32, f32, bool)> = Vec::new();
    for c in area.start_col..=area.end_col {
        let (w, hidden) = sheet.col_width_and_hidden(c);
        if !hidden || show_hidden {
            let eff_w = if hidden { w.max(36.0) } else { w.max(16.0) };
            visible_cols.push((c, eff_w, hidden));
        }
    }

    let mut visible_rows: Vec<(u32, f32, bool)> = Vec::new();
    for r in area.start_row..=area.end_row {
        let (h, hidden) = sheet.row_height_and_hidden(r);
        if !hidden || show_hidden {
            let eff_h = if hidden { h.max(14.0) } else { h.max(12.0) };
            visible_rows.push((r, eff_h, hidden));
        }
    }

    if visible_cols.is_empty() {
        visible_cols.push((0, sheet.default_col_width_pt, false));
    }
    if visible_rows.is_empty() {
        visible_rows.push((0, sheet.default_row_height_pt, false));
    }

    let margin_top = ps.margin_top_pt.max(36.0) + 18.0;
    let margin_bottom = ps.margin_bottom_pt.max(36.0);
    let margin_left = ps.margin_left_pt.max(28.0);
    let margin_right = ps.margin_right_pt.max(28.0);
    let heading_col_w = if ps.print_headings { 26.0 } else { 0.0 };
    let heading_row_h = if ps.print_headings { 15.0 } else { 0.0 };

    let avail_w = (page_w - margin_left - margin_right - heading_col_w).max(120.0);
    let avail_h = (page_h - margin_top - margin_bottom - heading_row_h).max(120.0);

    let total_grid_w: f32 = visible_cols.iter().map(|(_, w, _)| *w).sum();
    let total_grid_h: f32 = visible_rows.iter().map(|(_, h, _)| *h).sum();

    let scale = if let Some(fw) = ps.fit_to_width
        && fw > 0
        && total_grid_w > 0.0
    {
        let sw = (avail_w * f32::from(fw)) / total_grid_w;
        if let Some(fh) = ps.fit_to_height
            && fh > 0
            && total_grid_h > 0.0
        {
            sw.min((avail_h * f32::from(fh)) / total_grid_h)
                .clamp(0.25, 1.0)
        } else {
            sw.clamp(0.25, 1.0)
        }
    } else {
        (f32::from(ps.scale_pct) / 100.0).clamp(0.25, 2.0)
    };

    let repeat_rows: Vec<(u32, f32, bool)> = if let Some((rr1, rr2)) = ps.repeat_rows {
        (rr1..=rr2.min(rr1 + 20))
            .map(|r| {
                let (h, hid) = sheet.row_height_and_hidden(r);
                (r, h.max(12.0), hid)
            })
            .collect()
    } else {
        Vec::new()
    };
    let repeat_cols: Vec<(u32, f32, bool)> = if let Some((rc1, rc2)) = ps.repeat_cols {
        (rc1..=rc2.min(rc1 + 10))
            .map(|c| {
                let (w, hid) = sheet.col_width_and_hidden(c);
                (c, w.max(16.0), hid)
            })
            .collect()
    } else {
        Vec::new()
    };

    let col_chunks =
        split_axis_into_chunks(&visible_cols, &repeat_cols, &ps.col_breaks, avail_w / scale);
    let row_chunks =
        split_axis_into_chunks(&visible_rows, &repeat_rows, &ps.row_breaks, avail_h / scale);

    // Excel does not print pages that have nothing on them, so only the (column chunk, row
    // chunk) pairs holding a cell, a merge or a drawing become pages. A stray value at
    // XFD1048576 adds one page instead of millions of blank ones.
    let mut occupied: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut mark = |r: u32, c: u32| {
        if let (Some(ci), Some(ri)) = (
            chunk_containing(&col_chunks, repeat_cols.len(), c),
            chunk_containing(&row_chunks, repeat_rows.len(), r),
        ) {
            occupied.insert((ci, ri));
        }
    };
    for &(r, c) in sheet
        .cells
        .range((area.start_row, 0)..=(area.end_row, u32::MAX))
        .map(|(k, _)| k)
    {
        if (area.start_col..=area.end_col).contains(&c) {
            mark(r, c);
        }
    }
    for mr in &sheet.merged_ranges {
        mark(mr.start_row, mr.start_col);
    }
    for d in &sheet.drawings {
        mark(d.anchor.from_row, d.anchor.from_col);
    }
    if occupied.is_empty() {
        occupied.insert((0, 0));
    }
    if occupied.len() > max_pages {
        return Err(too_many_pages(options.max_pages));
    }

    let total_pages = occupied.len();
    let mut out_pages = Vec::with_capacity(total_pages);

    // Column chunk first: Excel's default page order is down, then over.
    for (local_page_num, &(ci, ri)) in (1usize..).zip(&occupied) {
        let (col_chunk, row_chunk) = (&col_chunks[ci], &row_chunks[ri]);
        {
            let mut page = LayoutPage::new(*page_idx, Size::new(page_w, page_h));
            page.label = Some(sheet.name.clone());

            emit_sheet_header_banner(
                sheet,
                wb,
                local_page_num,
                total_pages,
                page_w,
                page_h,
                *page_idx,
                font_sys,
                text_engine,
                &mut page.items,
            );

            render_grid_chunk(
                wb,
                sheet,
                row_chunk,
                col_chunk,
                &cond_map,
                image_id_map,
                Rect::new(
                    margin_left,
                    margin_top,
                    avail_w + heading_col_w,
                    avail_h + heading_row_h,
                ),
                scale,
                show_hidden,
                options.markup_mode == MarkupMode::AllMarkup,
                *page_idx,
                font_sys,
                text_engine,
                &mut page.items,
            );

            out_pages.push(page);
            *page_idx = page_idx.saturating_add(1);
        }
    }

    Ok(out_pages)
}

fn too_many_pages(max_pages: usize) -> Error {
    Error::InvalidInput(format!(
        "Workbook print layout exceeds max_pages ({max_pages})"
    ))
}

/// Index of the chunk whose own (non-repeated) entries include `idx`, if any. Chunk entries
/// after the first `n_repeat` are in ascending index order.
fn chunk_containing(chunks: &[Vec<(u32, f32, bool)>], n_repeat: usize, idx: u32) -> Option<usize> {
    let pos = chunks.partition_point(|ch| ch.get(n_repeat).is_some_and(|e| e.0 <= idx));
    let ci = pos.checked_sub(1)?;
    chunks[ci]
        .get(n_repeat..)?
        .binary_search_by_key(&idx, |e| e.0)
        .ok()
        .map(|_| ci)
}

fn split_axis_into_chunks(
    entries: &[(u32, f32, bool)],
    repeats: &[(u32, f32, bool)],
    manual_breaks: &[u32],
    budget_pt: f32,
) -> Vec<Vec<(u32, f32, bool)>> {
    let manual_breaks: BTreeSet<u32> = manual_breaks.iter().copied().collect();
    let repeat_ids: BTreeSet<u32> = repeats.iter().map(|(id, _, _)| *id).collect();
    let repeat_sum: f32 = repeats.iter().map(|(_, sz, _)| *sz).sum();
    let eff_budget = (budget_pt - repeat_sum).max(60.0);

    let mut chunks: Vec<Vec<(u32, f32, bool)>> = Vec::new();
    let mut current: Vec<(u32, f32, bool)> = repeats.to_vec();
    let mut used = 0.0f32;

    for &(idx, sz, hid) in entries {
        if repeat_ids.contains(&idx) {
            continue;
        }
        if used > 0.0 && used + sz > eff_budget {
            chunks.push(current);
            current = repeats.to_vec();
            used = 0.0;
        }
        current.push((idx, sz, hid));
        used += sz;

        if manual_breaks.contains(&idx) {
            chunks.push(current);
            current = repeats.to_vec();
            used = 0.0;
        }
    }

    if current.len() > repeats.len() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[allow(clippy::too_many_arguments)]
fn emit_sheet_header_banner(
    sheet: &Sheet,
    wb: &Workbook,
    page_num: usize,
    total_pages: usize,
    page_w: f32,
    page_h: f32,
    page_index: u32,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
    items: &mut Vec<Item>,
) {
    let vis_badge = match sheet.visibility {
        SheetVisibility::Visible => "",
        SheetVisibility::Hidden => " [HIDDEN]",
        SheetVisibility::VeryHidden => " [VERY HIDDEN]",
    };
    let prot_badge = if sheet.protected { " [PROTECTED]" } else { "" };
    let banner = format!(
        "Sheet: {}{vis_badge}{prot_badge}  (Page {page_num} of {total_pages})",
        sheet.name
    );

    emit_text_box(
        items,
        &banner,
        Point::new(28.0, 20.0),
        Some(page_w - 56.0),
        8.5,
        true,
        false,
        Rgba::rgb(0x37, 0x41, 0x51),
        TextAlign::Left,
        "Calibri",
        page_index,
        font_sys,
        text_engine,
    );

    if let Some(ref hdr) = sheet.print_setup.header_text {
        let expanded =
            expand_hf_codes(hdr, &sheet.name, wb.title.as_deref(), page_num, total_pages);
        if !expanded.is_empty() {
            emit_text_box(
                items,
                &expanded,
                Point::new(28.0, 8.0),
                Some(page_w - 56.0),
                8.0,
                false,
                false,
                Rgba::rgb(0x6B, 0x72, 0x80),
                TextAlign::Center,
                "Calibri",
                page_index,
                font_sys,
                text_engine,
            );
        }
    }

    if let Some(ref ftr) = sheet.print_setup.footer_text {
        let expanded =
            expand_hf_codes(ftr, &sheet.name, wb.title.as_deref(), page_num, total_pages);
        if !expanded.is_empty() {
            emit_text_box(
                items,
                &expanded,
                Point::new(28.0, page_h - 22.0),
                Some(page_w - 56.0),
                8.0,
                false,
                false,
                Rgba::rgb(0x6B, 0x72, 0x80),
                TextAlign::Center,
                "Calibri",
                page_index,
                font_sys,
                text_engine,
            );
        }
    }
}

fn expand_hf_codes(
    raw: &str,
    sheet_name: &str,
    wb_title: Option<&str>,
    page_num: usize,
    total_pages: usize,
) -> String {
    raw.replace("&P", &page_num.to_string())
        .replace("&N", &total_pages.to_string())
        .replace("&A", sheet_name)
        .replace("&F", wb_title.unwrap_or("Workbook"))
        .replace("&L", " ")
        .replace("&C", " ")
        .replace("&R", " ")
        .trim()
        .to_string()
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools,
    clippy::similar_names
)]
fn render_grid_chunk(
    wb: &Workbook,
    sheet: &Sheet,
    rows: &[(u32, f32, bool)],
    cols: &[(u32, f32, bool)],
    cond_map: &BTreeMap<(u32, u32), ResolvedCellConditional>,
    image_id_map: &BTreeMap<String, ImageId>,
    origin_rect: Rect,
    scale: f32,
    show_hidden: bool,
    show_comments: bool,
    page_index: u32,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
    items: &mut Vec<Item>,
) {
    let ps = &sheet.print_setup;
    let heading_col_w = if ps.print_headings { 26.0 * scale } else { 0.0 };
    let heading_row_h = if ps.print_headings { 15.0 * scale } else { 0.0 };

    let grid_x0 = origin_rect.x + heading_col_w;
    let grid_y0 = origin_rect.y + heading_row_h;

    let mut col_offsets: BTreeMap<u32, (f32, f32, bool)> = BTreeMap::new();
    let mut cx = grid_x0;
    for &(c, w, hid) in cols {
        let sw = w * scale;
        col_offsets.insert(c, (cx, sw, hid));
        if ps.print_headings {
            items.push(Item::Path(PathItem {
                commands: rect_cmds(Rect::new(cx, origin_rect.y, sw, heading_row_h)),
                fill: Some(FillPaint::Solid(Rgba::rgb(0xF3, 0xF4, 0xF6))),
                stroke: Some(thin_stroke(Rgba::rgb(0xD1, 0xD5, 0xDB), 0.5)),
                fill_rule: FillRule::NonZero,
                transform: Affine::IDENTITY,
            }));
            emit_text_box(
                items,
                &col_index_to_letters(c),
                Point::new(cx, origin_rect.y + 2.0),
                Some(sw),
                (8.0 * scale).max(6.0),
                true,
                false,
                Rgba::rgb(0x37, 0x41, 0x51),
                TextAlign::Center,
                "Calibri",
                page_index,
                font_sys,
                text_engine,
            );
        }
        cx += sw;
    }

    let mut row_offsets: BTreeMap<u32, (f32, f32, bool)> = BTreeMap::new();
    let mut cy = grid_y0;
    for &(r, h, hid) in rows {
        let sh = h * scale;
        row_offsets.insert(r, (cy, sh, hid));
        if ps.print_headings {
            items.push(Item::Path(PathItem {
                commands: rect_cmds(Rect::new(origin_rect.x, cy, heading_col_w, sh)),
                fill: Some(FillPaint::Solid(Rgba::rgb(0xF3, 0xF4, 0xF6))),
                stroke: Some(thin_stroke(Rgba::rgb(0xD1, 0xD5, 0xDB), 0.5)),
                fill_rule: FillRule::NonZero,
                transform: Affine::IDENTITY,
            }));
            emit_text_box(
                items,
                &(r + 1).to_string(),
                Point::new(origin_rect.x, cy + 2.0),
                Some(heading_col_w),
                (8.0 * scale).max(6.0),
                true,
                false,
                Rgba::rgb(0x37, 0x41, 0x51),
                TextAlign::Center,
                "Calibri",
                page_index,
                font_sys,
                text_engine,
            );
        }
        cy += sh;
    }

    let mut covered_merged: std::collections::BTreeSet<(u32, u32)> =
        std::collections::BTreeSet::new();
    let mut merge_leader_bounds: BTreeMap<(u32, u32), Rect> = BTreeMap::new();
    for mr in &sheet.merged_ranges {
        let mut x_min = f32::INFINITY;
        let mut y_min = f32::INFINITY;
        let mut x_max = f32::NEG_INFINITY;
        let mut y_max = f32::NEG_INFINITY;
        // Only the merge's intersection with this page: a merge such as A1:XFD1048576 must
        // not be walked cell by cell.
        let page_rows = rows
            .iter()
            .map(|e| e.0)
            .filter(|r| (mr.start_row..=mr.end_row).contains(r));
        for r in page_rows {
            let page_cols = cols
                .iter()
                .map(|e| e.0)
                .filter(|c| (mr.start_col..=mr.end_col).contains(c));
            for c in page_cols {
                if (r, c) != (mr.start_row, mr.start_col) {
                    covered_merged.insert((r, c));
                }
                if let (Some(&(x, w, _)), Some(&(y, h, _))) =
                    (col_offsets.get(&c), row_offsets.get(&r))
                {
                    x_min = x_min.min(x);
                    y_min = y_min.min(y);
                    x_max = x_max.max(x + w);
                    y_max = y_max.max(y + h);
                }
            }
        }
        if x_min.is_finite() && y_min.is_finite() && x_max > x_min && y_max > y_min {
            merge_leader_bounds.insert(
                (mr.start_row, mr.start_col),
                Rect::new(x_min, y_min, x_max - x_min, y_max - y_min),
            );
        }
    }

    for &(r, _, r_hid) in rows {
        let (ry, rh, _) = row_offsets[&r];
        for (c_pos, &(c, _, c_hid)) in cols.iter().enumerate() {
            if covered_merged.contains(&(r, c)) {
                continue;
            }
            let (rx, rw, _) = col_offsets[&c];
            let cell_rect = merge_leader_bounds
                .get(&(r, c))
                .copied()
                .unwrap_or_else(|| Rect::new(rx, ry, rw, rh));

            let cell = sheet.cells.get(&(r, c));
            let style_idx = cell.map_or(0, |cl| cl.style_idx);
            let xf = wb
                .styles
                .xf(style_idx)
                .cloned()
                .unwrap_or_else(CellXf::default);
            let font = wb
                .styles
                .font_for_style(style_idx)
                .cloned()
                .unwrap_or_default();
            let cond = cond_map.get(&(r, c));

            let bg_fill = cond
                .and_then(|cd| cd.fill_color)
                .or_else(|| wb.styles.fill_color_for_style(style_idx))
                .or_else(|| {
                    if r_hid || c_hid {
                        Some(Rgba::rgb(0xFE, 0xF3, 0xC7))
                    } else {
                        None
                    }
                });

            let stroke = if ps.print_gridlines {
                Some(thin_stroke(Rgba::rgb(0xE5, 0xE7, 0xEB), 0.4))
            } else {
                None
            };

            if bg_fill.is_some() || stroke.is_some() {
                items.push(Item::Path(PathItem {
                    commands: rect_cmds(cell_rect),
                    fill: bg_fill.map(FillPaint::Solid),
                    stroke,
                    fill_rule: FillRule::NonZero,
                    transform: Affine::IDENTITY,
                }));
            }

            if let Some(bdr) = wb.styles.border_for_style(style_idx) {
                emit_cell_borders(cell_rect, bdr, items);
            }

            if let Some((frac, bar_color, _)) = cond.and_then(|cd| cd.data_bar)
                && frac > 0.01
            {
                let mut bc = bar_color;
                bc.a = 140;
                let bar_rect = Rect::new(
                    cell_rect.x + 1.5,
                    cell_rect.y + 1.5,
                    (cell_rect.width - 3.0).max(1.0) * frac,
                    (cell_rect.height - 3.0).max(1.0),
                );
                items.push(Item::Path(PathItem {
                    commands: rect_cmds(bar_rect),
                    fill: Some(FillPaint::Solid(bc)),
                    stroke: None,
                    fill_rule: FillRule::NonZero,
                    transform: Affine::IDENTITY,
                }));
            }

            let mut text_left_pad = 2.0 * scale;
            if let Some((ref glyph, icon_color)) = cond.and_then(|cd| cd.icon.clone()) {
                emit_text_box(
                    items,
                    glyph,
                    Point::new(cell_rect.x + 2.0, cell_rect.y + 1.5),
                    Some(12.0 * scale),
                    (font.size_pt * scale).clamp(6.0, 18.0),
                    true,
                    false,
                    icon_color,
                    TextAlign::Left,
                    &font.name,
                    page_index,
                    font_sys,
                    text_engine,
                );
                text_left_pad += 11.0 * scale;
            }

            if let Some(cl) = cell {
                let show_value = cond
                    .and_then(|cd| cd.data_bar.map(|(_, _, sv)| sv))
                    .unwrap_or(true);
                if !show_value {
                    continue;
                }
                let txt = if cl.is_concealed {
                    if show_hidden {
                        format!("[{}]", cl.value.raw_string())
                    } else {
                        String::new()
                    }
                } else {
                    cl.display_text.clone()
                };

                if !txt.is_empty() {
                    let align = match xf.h_align {
                        CellHAlign::Left => TextAlign::Left,
                        CellHAlign::Center => TextAlign::Center,
                        CellHAlign::Right => TextAlign::Right,
                        CellHAlign::Justify => TextAlign::Justify,
                        CellHAlign::General => match cl.value {
                            CellValue::Number(_) => TextAlign::Right,
                            CellValue::Bool(_) | CellValue::Error(_) => TextAlign::Center,
                            _ => TextAlign::Left,
                        },
                    };

                    let mut avail_text_w = (cell_rect.width - text_left_pad - 2.0).max(4.0);
                    if !xf.wrap_text
                        && matches!(align, TextAlign::Left)
                        && !merge_leader_bounds.contains_key(&(r, c))
                    {
                        for &(next_c, next_w, _) in cols.iter().skip(c_pos + 1) {
                            let next_empty = sheet
                                .cells
                                .get(&(r, next_c))
                                .is_none_or(|nc| matches!(nc.value, CellValue::Empty));
                            if next_empty && !covered_merged.contains(&(r, next_c)) {
                                avail_text_w += next_w * scale;
                            } else {
                                break;
                            }
                        }
                    }

                    let text_color = if cl.is_concealed && show_hidden {
                        Rgba::rgb(0xB4, 0x53, 0x09)
                    } else {
                        cond.and_then(|cd| cd.font_color)
                            .or(cl.display_color)
                            .or(font.color)
                            .unwrap_or(Rgba::rgb(0x11, 0x18, 0x27))
                    };
                    let is_bold = cond.and_then(|cd| cd.bold).unwrap_or(font.bold);
                    let is_italic = cond.and_then(|cd| cd.italic).unwrap_or(font.italic);

                    emit_text_box(
                        items,
                        &txt,
                        Point::new(cell_rect.x + text_left_pad, cell_rect.y + 1.5 * scale),
                        Some(avail_text_w),
                        (font.size_pt * scale).clamp(5.5, 36.0),
                        is_bold,
                        is_italic,
                        text_color,
                        align,
                        &font.name,
                        page_index,
                        font_sys,
                        text_engine,
                    );
                }
            }
        }
    }

    for drawing in &sheet.drawings {
        if let (Some(&(dx, _, _)), Some(&(dy, _, _))) = (
            col_offsets.get(&drawing.anchor.from_col),
            row_offsets.get(&drawing.anchor.from_row),
        ) {
            let (_, _, raw_w, raw_h) = compute_drawing_bounds_on_sheet(sheet, drawing, show_hidden);
            let rect = Rect::new(
                dx + drawing.anchor.from_col_off_pt * scale,
                dy + drawing.anchor.from_row_off_pt * scale,
                raw_w * scale,
                raw_h * scale,
            );
            items.extend(render_drawing_items(
                drawing,
                rect,
                &wb.theme,
                image_id_map,
                page_index,
                font_sys,
                text_engine,
            ));
        }
    }

    if show_comments && !sheet.comments.is_empty() {
        let mut comment_y = cy + 6.0;
        let first_row = row_offsets.keys().next().copied().unwrap_or(0);
        let last_row = row_offsets.keys().next_back().copied().unwrap_or(0);
        for (&(cr, cc), cm) in sheet.comments.range((first_row, 0)..=(last_row, u32::MAX)) {
            if row_offsets.contains_key(&cr) && col_offsets.contains_key(&cc) {
                let cell_ref = format!("{}{}", col_index_to_letters(cc), cr + 1);
                let mut line = format!("[Comment {cell_ref} — {}]: {}", cm.author, cm.text);
                if !cm.replies.is_empty() {
                    line.push_str(" | ");
                    line.push_str(&cm.replies.join(" | "));
                }
                emit_text_box(
                    items,
                    &line,
                    Point::new(grid_x0, comment_y),
                    Some(origin_rect.width),
                    7.5,
                    false,
                    false,
                    Rgba::rgb(0x92, 0x40, 0x0E),
                    TextAlign::Left,
                    "Calibri",
                    page_index,
                    font_sys,
                    text_engine,
                );
                comment_y += 10.0;
            }
        }
    }
}

fn render_drawing_items(
    drawing: &AnchoredDrawing,
    rect: Rect,
    theme: &prism_drawingml::DrawingTheme,
    image_id_map: &BTreeMap<String, ImageId>,
    page_index: u32,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
) -> Vec<Item> {
    let mut items = Vec::new();
    match &drawing.kind {
        WorksheetDrawingKind::Chart(chart) => {
            items.extend(render_chart(chart, rect, theme, font_sys, text_engine));
        }
        WorksheetDrawingKind::Image { resource_id } => {
            if let Some(&img_id) = image_id_map.get(resource_id) {
                items.push(Item::Image(ImageItem {
                    image: img_id,
                    rect,
                    transform: Affine::IDENTITY,
                    opacity: 1.0,
                    alt_text: Some(drawing.name.clone()),
                }));
            }
        }
        WorksheetDrawingKind::Shape {
            preset_geom,
            text,
            fill_color,
            stroke_color,
        } => {
            let cmds = build_preset_geometry_paths(preset_geom, rect, &[]);
            items.push(Item::Path(PathItem {
                commands: cmds,
                fill: fill_color.map(FillPaint::Solid),
                stroke: stroke_color.map(|c| thin_stroke(c, 1.0)),
                fill_rule: FillRule::NonZero,
                transform: Affine::IDENTITY,
            }));
            if !text.is_empty() {
                emit_text_box(
                    &mut items,
                    text,
                    Point::new(rect.x + 4.0, rect.y + 4.0),
                    Some((rect.width - 8.0).max(10.0)),
                    9.0,
                    false,
                    false,
                    Rgba::WHITE,
                    TextAlign::Center,
                    "Calibri",
                    page_index,
                    font_sys,
                    text_engine,
                );
            }
        }
    }
    items
}

fn emit_cell_borders(rect: Rect, bdr: &crate::model::ExcelBorder, items: &mut Vec<Item>) {
    let mut seg = |p1: Point, p2: Point, edge: &crate::model::BorderEdge| {
        items.push(Item::Path(PathItem {
            commands: vec![PathCommand::MoveTo(p1), PathCommand::LineTo(p2)],
            fill: None,
            stroke: Some(thin_stroke(edge.color, edge.width_pt)),
            fill_rule: FillRule::NonZero,
            transform: Affine::IDENTITY,
        }));
    };
    if let Some(ref top) = bdr.top {
        seg(
            Point::new(rect.x, rect.y),
            Point::new(rect.right(), rect.y),
            top,
        );
    }
    if let Some(ref bot) = bdr.bottom {
        seg(
            Point::new(rect.x, rect.bottom()),
            Point::new(rect.right(), rect.bottom()),
            bot,
        );
    }
    if let Some(ref left) = bdr.left {
        seg(
            Point::new(rect.x, rect.y),
            Point::new(rect.x, rect.bottom()),
            left,
        );
    }
    if let Some(ref right) = bdr.right {
        seg(
            Point::new(rect.right(), rect.y),
            Point::new(rect.right(), rect.bottom()),
            right,
        );
    }
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn emit_text_box(
    items: &mut Vec<Item>,
    text: &str,
    origin: Point,
    max_width: Option<f32>,
    font_size_pt: f32,
    bold: bool,
    italic: bool,
    color: Rgba,
    align: TextAlign,
    font_family: &str,
    page_index: u32,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
) {
    if text.trim().is_empty() {
        return;
    }
    let spec = ParagraphSpec {
        spans: vec![SpanSpec {
            text: text.to_string(),
            family: font_family.to_string(),
            size: font_size_pt,
            bold,
            italic,
            color,
            ..SpanSpec::default()
        }],
        align,
        max_width,
        origin,
        page_index,
        ..ParagraphSpec::default()
    };
    let shaped = text_engine.layout_paragraph(&spec, font_sys);
    for run in shaped.runs {
        items.push(Item::Glyphs(run));
    }
}

fn rect_cmds(r: Rect) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(Point::new(r.x, r.y)),
        PathCommand::LineTo(Point::new(r.right(), r.y)),
        PathCommand::LineTo(Point::new(r.right(), r.bottom())),
        PathCommand::LineTo(Point::new(r.x, r.bottom())),
        PathCommand::Close,
    ]
}

fn thin_stroke(color: Rgba, width_pt: f32) -> StrokeStyle {
    StrokeStyle {
        width: width_pt,
        color,
        line_cap: LineCap::Butt,
        line_join: LineJoin::Miter,
        dash_array: Vec::new(),
        dash_offset: 0.0,
    }
}
