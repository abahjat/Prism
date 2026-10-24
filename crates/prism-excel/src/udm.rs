// SPDX-License-Identifier: AGPL-3.0-only
//! Conversion from [`Workbook`] to [`prism_core::document::Document`] (Unified Document Model).

use crate::model::{
    Cell, Sheet, Workbook, WorkbookStyles, WorksheetDrawingKind, col_index_to_letters,
};
use prism_core::document::{
    CellBorders, ContentBlock, Dimensions, Document, ImageBlock, Page, PageMetadata, Rect,
    ShapeStyle, TableBlock, TableCell, TableRow, TextBlock, TextRun, TextStyle,
};
use prism_core::metadata::Metadata;
use prism_layout::model::Rgba;
use std::collections::HashMap;
use std::fmt::Write;

/// Convert a [`Workbook`] into a [`prism_core::document::Document`].
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn workbook_to_udm(wb: &Workbook, format_label: &str) -> Document {
    let mut pages = Vec::new();

    for (idx, sheet) in wb.sheets.iter().enumerate() {
        let mut content = Vec::new();

        push_sheet_cells(sheet, &wb.styles, &mut content);

        // Comments are hidden on the sheet; keep them in the extracted text.
        for (&(r, c), cm) in &sheet.comments {
            let mut line = format!(
                "Comment {}{} ({}): {}",
                col_index_to_letters(c),
                r + 1,
                cm.author,
                cm.text
            );
            for reply in &cm.replies {
                line.push_str(" | ");
                line.push_str(reply);
            }
            content.push(text_block(line));
        }

        for d in &sheet.drawings {
            match &d.kind {
                WorksheetDrawingKind::Image { resource_id } => {
                    content.push(ContentBlock::Image(ImageBlock {
                        bounds: Rect::new(
                            36.0,
                            36.0,
                            f64::from(d.anchor.width_pt),
                            f64::from(d.anchor.height_pt),
                        ),
                        resource_id: resource_id.clone(),
                        alt_text: Some(d.name.clone()),
                        format: None,
                        original_size: None,
                        style: ShapeStyle::default(),
                        rotation: 0.0,
                    }));
                }
                WorksheetDrawingKind::Chart(chart) => {
                    if let Some(ref title) = chart.title {
                        content.push(ContentBlock::Text(TextBlock {
                            runs: vec![TextRun::new(title)],
                            paragraph_style: None,
                            bounds: Rect::default(),
                            vertical_alignment: None,
                            style: ShapeStyle::default(),
                            rotation: 0.0,
                        }));
                    }
                }
                WorksheetDrawingKind::Shape { text, .. } => {
                    if !text.is_empty() {
                        content.push(ContentBlock::Text(TextBlock {
                            runs: vec![TextRun::new(text)],
                            paragraph_style: None,
                            bounds: Rect::default(),
                            vertical_alignment: None,
                            style: ShapeStyle::default(),
                            rotation: 0.0,
                        }));
                    }
                }
            }
        }

        #[allow(clippy::cast_possible_truncation)]
        pages.push(Page {
            number: (idx + 1) as u32,
            dimensions: Dimensions::LETTER,
            content,
            annotations: Vec::new(),
            metadata: PageMetadata {
                label: Some(sheet.name.clone()),
                rotation: 0,
                margins: None,
            },
        });
    }

    if pages.is_empty() {
        pages.push(Page::new(1, Dimensions::LETTER));
    }

    let mut metadata = Metadata::new();
    metadata.title.clone_from(&wb.title);
    metadata.author.clone_from(&wb.author);
    metadata.add_custom("format", format_label);
    #[allow(clippy::cast_possible_wrap)]
    {
        metadata.add_custom("sheet_count", wb.sheets.len() as i64);
        metadata.add_custom("excel_sheet_count", wb.sheets.len() as i64);
    }

    let mut document = Document::builder().metadata(metadata).build();
    document.pages = pages;
    document.resources.images = wb.images.values().cloned().collect();
    document
}

/// Widest table emitted per sheet; cells in later columns are listed as text after it.
const MAX_TABLE_COLS: u32 = 256;
/// Most table cells emitted per sheet; later rows are listed as text after it.
const MAX_TABLE_CELLS: usize = 1_000_000;

fn rgba_to_hex(c: Rgba) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

fn text_block(text: String) -> ContentBlock {
    text_block_styled(text, TextStyle::default())
}

fn text_block_styled(text: String, style: TextStyle) -> ContentBlock {
    ContentBlock::Text(TextBlock {
        runs: vec![TextRun {
            text,
            style,
            bounds: None,
            char_positions: None,
        }],
        paragraph_style: None,
        bounds: Rect::default(),
        vertical_alignment: None,
        style: ShapeStyle::default(),
        rotation: 0.0,
    })
}

/// Extractable text of a cell: the displayed string, or the stored value when the number
/// format hides it (`;;;`), so concealed values stay searchable.
fn cell_text(cell: &Cell) -> String {
    if cell.display_text.is_empty() {
        cell.value.raw_string()
    } else {
        cell.display_text.clone()
    }
}

fn build_table_cell(
    cell_opt: Option<&Cell>,
    styles: &WorkbookStyles,
    col_span: usize,
    row_span: usize,
) -> TableCell {
    let Some(cell) = cell_opt else {
        return TableCell {
            content: vec![text_block(String::new())],
            col_span,
            row_span,
            background_color: None,
            borders: None,
        };
    };

    let mut text_style = TextStyle::default();
    if let Some(font) = styles.font_for_style(cell.style_idx) {
        text_style.font_family = Some(font.name.clone());
        text_style.font_size = Some(f64::from(font.size_pt));
        text_style.bold = font.bold;
        text_style.italic = font.italic;
        text_style.underline = font.underline;
        text_style.strikethrough = font.strikethrough;
        text_style.color = font.color.map(rgba_to_hex);
    }
    if let Some(dc) = cell.display_color {
        text_style.color = Some(rgba_to_hex(dc));
    }

    let background_color = styles.fill_color_for_style(cell.style_idx).map(rgba_to_hex);

    let borders = styles.border_for_style(cell.style_idx).and_then(|b| {
        if b.top.is_none() && b.bottom.is_none() && b.left.is_none() && b.right.is_none() {
            None
        } else {
            Some(CellBorders {
                top_color: b.top.as_ref().map(|e| rgba_to_hex(e.color)),
                top_width: b.top.as_ref().map(|e| f64::from(e.width_pt)),
                bottom_color: b.bottom.as_ref().map(|e| rgba_to_hex(e.color)),
                bottom_width: b.bottom.as_ref().map(|e| f64::from(e.width_pt)),
                left_color: b.left.as_ref().map(|e| rgba_to_hex(e.color)),
                left_width: b.left.as_ref().map(|e| f64::from(e.width_pt)),
                right_color: b.right.as_ref().map(|e| rgba_to_hex(e.color)),
                right_width: b.right.as_ref().map(|e| f64::from(e.width_pt)),
            })
        }
    });

    TableCell {
        content: vec![text_block_styled(cell_text(cell), text_style)],
        col_span,
        row_span,
        background_color,
        borders,
    }
}

/// Build one table row from its column-ordered `cells`. A merge anchor spans the cells to its
/// right, which are then left out of the row so the columns after it stay aligned. Only the
/// horizontal span is applied: the layout keeps every row's cells in column order, so later
/// rows of a tall merge still list their own (normally empty) cells. A value stored in a
/// covered cell is hidden by Excel but kept, in `extra`.
fn merged_row_cells(
    cells: &[Option<&Cell>],
    r: u32,
    merge_anchors: &HashMap<(u32, u32), usize>,
    styles: &WorkbookStyles,
    extra: &mut String,
) -> Vec<TableCell> {
    let mut row_out = Vec::with_capacity(cells.len());
    let mut col_idx = 0usize;
    while col_idx < cells.len() {
        #[allow(clippy::cast_possible_truncation)]
        let c = col_idx as u32;
        let col_span = merge_anchors
            .get(&(r, c))
            .copied()
            .unwrap_or(1)
            .clamp(1, cells.len() - col_idx);
        row_out.push(build_table_cell(cells[col_idx], styles, col_span, 1));
        for (covered_idx, covered) in cells
            .iter()
            .enumerate()
            .skip(col_idx + 1)
            .take(col_span - 1)
        {
            if let Some(cell) = covered {
                let text = cell_text(cell);
                if !text.is_empty() {
                    #[allow(clippy::cast_possible_truncation)]
                    let cc = covered_idx as u32;
                    let _ = write!(extra, "{}{}: {text}	", col_index_to_letters(cc), r + 1);
                }
            }
        }
        col_idx += col_span;
    }
    row_out
}

/// Emit the populated rows of `sheet` as a table (bounded by [`MAX_TABLE_COLS`] and
/// [`MAX_TABLE_CELLS`]), followed by one tab-separated text line per row for any cells that
/// fall outside it. Only populated rows are visited, so a sparse sheet stays cheap and no
/// cell is dropped from the extracted text.
fn push_sheet_cells(sheet: &Sheet, styles: &WorkbookStyles, content: &mut Vec<ContentBlock>) {
    let Some(mut max_c) = sheet.cells.keys().map(|&(_, c)| c).max() else {
        return;
    };
    for mr in &sheet.merged_ranges {
        if sheet.cells.contains_key(&(mr.start_row, mr.start_col)) && mr.end_col < MAX_TABLE_COLS {
            max_c = max_c.max(mr.end_col);
        }
    }
    let table_cols = max_c.min(MAX_TABLE_COLS - 1) + 1;
    let col_widths: Vec<f64> = (0..table_cols)
        .map(|c| f64::from(sheet.col_width_and_hidden(c).0))
        .collect();

    let merge_anchors: HashMap<(u32, u32), usize> = sheet
        .merged_ranges
        .iter()
        .map(|mr| {
            let cs = (mr.end_col.saturating_sub(mr.start_col) + 1).max(1) as usize;
            ((mr.start_row, mr.start_col), cs)
        })
        .collect();

    let mut rows = Vec::new();
    let mut table_cells = 0usize;
    let mut overflow = Vec::new();
    let mut iter = sheet.cells.iter().peekable();
    while let Some(&(&(r, _), _)) = iter.peek() {
        let mut row_cells: Vec<(u32, &Cell)> = Vec::new();
        while let Some(&(&(cr, c), cell)) = iter.peek() {
            if cr != r {
                break;
            }
            row_cells.push((c, cell));
            iter.next();
        }

        let fits = table_cells + table_cols as usize <= MAX_TABLE_CELLS;
        let mut extra = String::new();
        if fits {
            let mut cells: Vec<Option<&Cell>> = vec![None; table_cols as usize];
            for (c, cell) in row_cells {
                if c < table_cols {
                    cells[c as usize] = Some(cell);
                } else {
                    let text = cell_text(cell);
                    if !text.is_empty() {
                        let _ = write!(extra, "{}{}: {text}\t", col_index_to_letters(c), r + 1);
                    }
                }
            }
            table_cells += cells.len();
            let row_out = merged_row_cells(&cells, r, &merge_anchors, styles, &mut extra);
            rows.push(TableRow {
                cells: row_out,
                height: Some(f64::from(sheet.row_height_and_hidden(r).0)),
            });
        } else {
            for (c, cell) in row_cells {
                let text = cell_text(cell);
                if !text.is_empty() {
                    let _ = write!(extra, "{}{}: {text}\t", col_index_to_letters(c), r + 1);
                }
            }
        }
        if !extra.is_empty() {
            overflow.push(extra);
        }
    }

    if !rows.is_empty() {
        content.push(ContentBlock::Table(TableBlock {
            rows,
            bounds: Rect::new(36.0, 36.0, 540.0, 720.0),
            column_count: col_widths.len(),
            column_widths: col_widths,
            style: ShapeStyle::default(),
            rotation: 0.0,
        }));
    }
    content.extend(overflow.into_iter().map(text_block));
}
