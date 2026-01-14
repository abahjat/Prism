// SPDX-License-Identifier: AGPL-3.0-only
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Rect, TableBlock, TableCell, TableRow, TextBlock,
        TextRun,
    },
    error::{Error, Result},
    parser::ParseContext,
};
use std::io::Cursor;

/// Parse a 7z archive and return a document structure representing the file listing.
///
/// # Errors
///
/// Returns an error if the 7z archive is malformed or cannot be read.
pub fn parse(_context: ParseContext, data: Bytes) -> Result<Document> {
    let reader = Cursor::new(data);

    // sevenz_rust::SevenZReader::new takes a reader + size + password option
    // Wait, check standard API usage.
    // If exact API is unknown, I'll try standard approaches and rely on compiler.
    // sevenz_rust::SevenZReader::new(reader, file_len, password)

    let len = reader.get_ref().len() as u64;
    let password = sevenz_rust::Password::empty();
    let seven_z = sevenz_rust::SevenZReader::new(reader, len, password)
        .map_err(|e| Error::ParseError(e.to_string()))?;

    let mut rows = Vec::new();

    // Header row
    rows.push(TableRow {
        cells: vec![
            create_header_cell("Path"),
            create_header_cell("Size"),
            create_header_cell("Compressed"),
            create_header_cell("Attributes"),
        ],
        height: None,
    });

    let archive_entries = &seven_z.archive().files;

    for entry in archive_entries {
        rows.push(TableRow {
            cells: vec![
                create_text_cell(entry.name()),
                create_text_cell(&format_size(entry.size())),
                create_text_cell(&format_size(entry.compressed_size)),
                create_text_cell(if entry.is_directory() { "Dir" } else { "File" }),
            ],
            height: None,
        });
    }

    let mut document = Document::new();
    let mut page = prism_core::document::Page::new(1, Dimensions::LETTER);

    #[allow(clippy::cast_precision_loss)]
    let table_height = rows.len() as f64 * 20.0;
    let table = TableBlock {
        bounds: Rect::new(50.0, 50.0, 500.0, table_height), // Approximate
        rows,
        column_count: 4,
        column_widths: Vec::new(),
        style: prism_core::document::ShapeStyle::default(),
        rotation: 0.0,
    };

    page.add_content(ContentBlock::Table(table));
    document.pages.push(page);

    Ok(document)
}

fn create_header_cell(text: &str) -> TableCell {
    let mut run = TextRun::new(text);
    run.style.bold = true;
    let block = TextBlock {
        vertical_alignment: None,
        bounds: Rect::default(),
        runs: vec![run],
        paragraph_style: None,
        style: prism_core::document::ShapeStyle::default(),
        rotation: 0.0,
    };

    TableCell {
        content: vec![ContentBlock::Text(block)],
        col_span: 1,
        row_span: 1,
        background_color: Some("#CCCCCC".to_string()),
        borders: None,
    }
}

fn create_text_cell(text: &str) -> TableCell {
    let run = TextRun::new(text);

    let block = TextBlock {
        vertical_alignment: None,
        bounds: Rect::default(),
        runs: vec![run],
        paragraph_style: None,
        style: prism_core::document::ShapeStyle::default(),
        rotation: 0.0,
    };

    TableCell {
        content: vec![ContentBlock::Text(block)],
        col_span: 1,
        row_span: 1,
        background_color: None,
        borders: None,
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        #[allow(clippy::cast_precision_loss)]
        let kb = bytes as f64 / 1024.0;
        format!("{kb:.1} KB")
    } else {
        #[allow(clippy::cast_precision_loss)]
        let mb = bytes as f64 / (1024.0 * 1024.0);
        format!("{mb:.1} MB")
    }
}
