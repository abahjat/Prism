// SPDX-License-Identifier: AGPL-3.0-only
//! Conversion from [`Workbook`] to [`prism_core::document::Document`] (Unified Document Model).

use crate::model::{Cell, Sheet, Workbook, WorksheetDrawingKind, col_index_to_letters};
use prism_core::document::{
    ContentBlock, Dimensions, Document, ImageBlock, Page, PageMetadata, Rect, ShapeStyle,
    TableBlock, TableCell, TableRow, TextBlock, TextRun, TextStyle,
};
use prism_core::metadata::Metadata;
use std::fmt::Write;

/// Convert a [`Workbook`] into a [`prism_core::document::Document`].
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn workbook_to_udm(wb: &Workbook, format_label: &str) -> Document {
    let mut pages = Vec::new();

    for (idx, sheet) in wb.sheets.iter().enumerate() {
        let mut content = Vec::new();

        push_sheet_cells(sheet, &mut content);

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
    metadata.add_custom("sheet_count", wb.sheets.len() as i64);

    let mut document = Document::builder().metadata(metadata).build();
    document.pages = pages;
    document.resources.images = wb.images.values().cloned().collect();
    document
}

/// Widest table emitted per sheet; cells in later columns are listed as text after it.
const MAX_TABLE_COLS: u32 = 256;
/// Most table cells emitted per sheet; later rows are listed as text after it.
const MAX_TABLE_CELLS: usize = 1_000_000;

fn text_block(text: String) -> ContentBlock {
    ContentBlock::Text(TextBlock {
        runs: vec![TextRun {
            text,
            style: TextStyle::default(),
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

/// Emit the populated rows of `sheet` as a table (bounded by [`MAX_TABLE_COLS`] and
/// [`MAX_TABLE_CELLS`]), followed by one tab-separated text line per row for any cells that
/// fall outside it. Only populated rows are visited, so a sparse sheet stays cheap and no
/// cell is dropped from the extracted text.
fn push_sheet_cells(sheet: &Sheet, content: &mut Vec<ContentBlock>) {
    let Some(max_c) = sheet.cells.keys().map(|&(_, c)| c).max() else {
        return;
    };
    let table_cols = max_c.min(MAX_TABLE_COLS - 1) + 1;
    let col_widths: Vec<f64> = (0..table_cols)
        .map(|c| f64::from(sheet.col_width_and_hidden(c).0))
        .collect();

    let mut rows = Vec::new();
    let mut table_cells = 0usize;
    let mut overflow = Vec::new();
    let mut iter = sheet.cells.iter().peekable();
    while let Some(&(&(r, _), _)) = iter.peek() {
        let mut row_cells: Vec<(u32, String)> = Vec::new();
        while let Some(&(&(cr, c), cell)) = iter.peek() {
            if cr != r {
                break;
            }
            row_cells.push((c, cell_text(cell)));
            iter.next();
        }

        let fits = table_cells + table_cols as usize <= MAX_TABLE_CELLS;
        let mut extra = String::new();
        if fits {
            let mut cells = vec![String::new(); table_cols as usize];
            for (c, text) in row_cells {
                if c < table_cols {
                    cells[c as usize] = text;
                } else if !text.is_empty() {
                    let _ = write!(extra, "{}{}: {text}\t", col_index_to_letters(c), r + 1);
                }
            }
            table_cells += cells.len();
            rows.push(TableRow {
                cells: cells
                    .into_iter()
                    .map(|text| TableCell {
                        content: vec![text_block(text)],
                        col_span: 1,
                        row_span: 1,
                        background_color: None,
                        borders: None,
                    })
                    .collect(),
                height: Some(f64::from(sheet.row_height_and_hidden(r).0)),
            });
        } else {
            for (c, text) in row_cells {
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
