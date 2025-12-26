// SPDX-License-Identifier: AGPL-3.0-only
use bytes::Bytes;
use chrono::NaiveDateTime;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Rect, TableBlock, TableCell, TableRow, TextBlock,
        TextRun,
    },
    error::{Error, Result},
    parser::ParseContext,
};
use std::io::Cursor;
use zip::ZipArchive;

pub async fn parse(_context: ParseContext, data: Bytes) -> Result<Document> {
    let reader = Cursor::new(data);
    let mut archive = ZipArchive::new(reader).map_err(|e| Error::ParseError(e.to_string()))?;

    let mut rows = Vec::new();

    // Header row
    rows.push(TableRow {
        cells: vec![
            create_header_cell("Path"),
            create_header_cell("Size"),
            create_header_cell("Compressed"),
            create_header_cell("Modified"),
        ],
        height: None,
    });

    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| Error::ParseError(e.to_string()))?;

        // Format date
        let dt = file.last_modified();
        // ZipDateTime to string
        let modified = format!(
            "{}-{}-{} {}:{}:{}",
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second()
        );

        rows.push(TableRow {
            cells: vec![
                create_text_cell(file.name()),
                create_text_cell(&format_size(file.size())),
                create_text_cell(&format_size(file.compressed_size())),
                create_text_cell(&modified),
            ],
            height: None,
        });
    }

    let mut document = Document::new();
    let mut page = prism_core::document::Page::new(1, Dimensions::LETTER);

    let table = TableBlock {
        bounds: Rect::new(50.0, 50.0, 500.0, rows.len() as f64 * 20.0), // Approximate
        rows,
        column_count: 4,
        style: Default::default(),
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
        bounds: Default::default(),
        runs: vec![run],
        paragraph_style: None,
        style: Default::default(),
        rotation: 0.0,
    };

    TableCell {
        content: vec![ContentBlock::Text(block)],
        col_span: 1,
        row_span: 1,
        background_color: Some("#CCCCCC".to_string()),
    }
}

fn create_text_cell(text: &str) -> TableCell {
    let run = TextRun::new(text);

    let block = TextBlock {
        bounds: Default::default(),
        runs: vec![run],
        paragraph_style: None,
        style: Default::default(),
        rotation: 0.0,
    };

    TableCell {
        content: vec![ContentBlock::Text(block)],
        col_span: 1,
        row_span: 1,
        background_color: None,
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
