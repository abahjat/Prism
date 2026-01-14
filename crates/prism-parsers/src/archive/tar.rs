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
use tar::Archive;

/// Parse a TAR archive and return a document structure representing the file listing.
///
/// # Errors
///
/// Returns an error if the TAR archive is malformed or cannot be read.
pub fn parse(_context: ParseContext, data: Bytes) -> Result<Document> {
    let reader = Cursor::new(data);
    let mut archive = Archive::new(reader);

    let mut rows = Vec::new();

    // Header row
    rows.push(TableRow {
        cells: vec![
            create_header_cell("Path"),
            create_header_cell("Size"),
            create_header_cell("Modified"),
        ],
        height: None,
    });

    // tar::Archive::entries() returns an iterator over Result<Entry>
    let entries = archive
        .entries()
        .map_err(|e| Error::ParseError(e.to_string()))?;

    for entry in entries {
        let entry = entry.map_err(|e| Error::ParseError(e.to_string()))?;

        // Skip directories? Usually they appear as explicit entries in TAR.
        // We can include them.

        let path = entry.path().map_or_else(
            |_| "[Unknown]".to_string(),
            |p| p.to_string_lossy().to_string(),
        );
        let size = entry.size();
        let mtime = entry.header().mtime().unwrap_or(0);
        let mtime_i64 = i64::try_from(mtime).unwrap_or(0);

        // Simple date formatting (manual implementation to avoid extra deps if possible, or use chrono)
        // Since we used chrono in zip.rs, we can use it here too if we interpret mtime as unix timestamp
        let modified = match chrono::DateTime::from_timestamp(mtime_i64, 0) {
            Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => "-".to_string(),
        };

        rows.push(TableRow {
            cells: vec![
                create_text_cell(&path),
                create_text_cell(&format_size(size)),
                create_text_cell(&modified),
            ],
            height: None,
        });
    }

    let mut document = Document::new();
    let mut page = prism_core::document::Page::new(1, Dimensions::LETTER);

    #[allow(clippy::cast_precision_loss)]
    let table_height = rows.len() as f64 * 20.0;
    let table = TableBlock {
        bounds: Rect::new(50.0, 50.0, 500.0, table_height),
        rows,
        column_count: 3,
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
        bounds: Rect::default(),
        runs: vec![run],
        paragraph_style: None,
        vertical_alignment: None,
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
        bounds: Rect::default(),
        runs: vec![run],
        paragraph_style: None,
        vertical_alignment: None,
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
