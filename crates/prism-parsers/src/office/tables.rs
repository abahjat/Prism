// SPDX-License-Identifier: AGPL-3.0-only
//! # Tables Module
//!
//! Parsing logic for Word and PowerPoint tables.

use crate::office::utils;
use prism_core::document::{ContentBlock, Rect, TableBlock, TableCell, TableRow, TextBlock};
use prism_core::error::{Error, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::BufRead;

/// Parse a table from the current position in the XML reader
/// Assumes we just read <w:tbl>
pub fn parse_table<R: BufRead>(reader: &mut Reader<R>) -> Result<TableBlock> {
    let mut rows = Vec::new();
    let mut buf = Vec::new();
    let mut depth = 1; // Started at <w:tbl>

    let mut current_row = None;
    let mut current_cell = None;
    let mut cell_content = Vec::new();

    // Track grid spans (merged_cells)
    let mut grid_span = 1;

    // We need to parse content exactly like the main parser but scoped to cells
    // For now, we'll do a simplified extraction of text within cells
    // TODO: Recursively call a common "parse_block_content" to handle rich text/images in cells

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:tbl" => depth += 1, // Nested table
                    b"w:tr" => {
                        current_row = Some(TableRow {
                            cells: Vec::new(),
                            height: None,
                        });
                    }
                    b"w:tc" => {
                        current_cell = Some(TableCell {
                            content: Vec::new(),
                            col_span: 1,
                            row_span: 1,
                            background_color: None,
                        });
                        cell_content.clear();
                        grid_span = 1;
                    }
                    b"w:gridSpan" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"w:val" {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<usize>() {
                                    grid_span = val;
                                }
                            }
                        }
                    }
                    b"w:shd" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"w:fill" {
                                let val = utils::attr_value(&attr.value);
                                if val != "auto" {
                                    if let Some(cell) = &mut current_cell {
                                        cell.background_color = Some(format!("#{}", val));
                                    }
                                }
                            }
                        }
                    }
                    // TODO: Handle vMerge for row spans
                    b"w:t" => {
                        // Capture text content
                        // Capture text content
                        let mut cell_text = String::new();
                        let mut depth = 1;
                        loop {
                            match reader.read_event_into(&mut buf) {
                                Ok(Event::Text(e)) => {
                                    if let Ok(text) = e.unescape() {
                                        cell_text.push_str(&text);
                                    }
                                }
                                Ok(Event::Start(_)) => depth += 1,
                                Ok(Event::End(_)) => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                Ok(Event::Eof) => break,
                                Err(_) => break,
                                _ => {}
                            }
                            buf.clear();
                        }

                        if !cell_text.is_empty() {
                            let mut block = TextBlock::new(Rect::default());
                            block.add_run(prism_core::document::TextRun::new(cell_text));
                            cell_content.push(ContentBlock::Text(block));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:tbl" => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b"w:tr" => {
                        if let Some(row) = current_row.take() {
                            rows.push(row);
                        }
                    }
                    b"w:tc" => {
                        if let Some(mut cell) = current_cell.take() {
                            cell.content = cell_content.clone();
                            cell.col_span = grid_span;
                            if let Some(row) = &mut current_row {
                                row.cells.push(cell);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => return Err(Error::ParseError("Unexpected EOF in table".to_string())),
            Err(e) => return Err(Error::ParseError(format!("XML error in table: {}", e))),
            _ => {}
        }
        buf.clear();
    }

    Ok(TableBlock {
        bounds: Rect::default(),
        rows,
        column_count: 0, // TODO: Calculate from max cells
        style: prism_core::document::ShapeStyle::default(),
        rotation: 0.0,
    })
}

/// Parse a DrawingML table (a:tbl) usually found in PPTX
pub fn parse_drawingml_table<R: BufRead>(reader: &mut Reader<R>) -> Result<TableBlock> {
    let mut rows = Vec::new();
    let mut buf = Vec::new();
    let mut depth = 1; // Started at <a:tbl>

    let mut current_row = None;
    let mut current_cell = None;
    let mut cell_content = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"a:tbl" => depth += 1,
                    b"a:tr" => {
                        // TODO: Parse h (height) attribute
                        current_row = Some(TableRow {
                            cells: Vec::new(),
                            height: None,
                        });
                    }
                    b"a:tc" => {
                        current_cell = Some(TableCell {
                            content: Vec::new(),
                            col_span: 1,            // TODO: Parse gridSpan
                            row_span: 1,            // TODO: Parse rowSpan
                            background_color: None, // TODO: Parse cell formatting
                        });
                        cell_content.clear();
                    }
                    b"a:txBody" => {
                        let text_runs =
                            crate::office::shapes::parse_text_body(reader, &mut buf, b"a:txBody");
                        if !text_runs.is_empty() {
                            let mut block = TextBlock::new(Rect::default());
                            for run in text_runs {
                                block.add_run(run);
                            }
                            cell_content.push(ContentBlock::Text(block));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"a:tbl" => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b"a:tr" => {
                        if let Some(row) = current_row.take() {
                            rows.push(row);
                        }
                    }
                    b"a:tc" => {
                        if let Some(mut cell) = current_cell.take() {
                            cell.content = cell_content.clone();
                            if let Some(row) = &mut current_row {
                                row.cells.push(cell);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => return Err(Error::ParseError("Unexpected EOF in table".to_string())),
            _ => {}
        }
        buf.clear();
    }

    Ok(TableBlock {
        bounds: Rect::default(),
        rows,
        column_count: 0,
        style: prism_core::document::ShapeStyle::default(),
        rotation: 0.0,
    })
}
