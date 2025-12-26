// SPDX-License-Identifier: AGPL-3.0-only
use bytes::Bytes;
use flate2::read::GzDecoder;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Rect, TableBlock, TableCell, TableRow, TextBlock,
        TextRun,
    },
    error::{Error, Result},
    parser::ParseContext,
};
use std::io::{Cursor, Read};

// Import tar parse function to delegate if needed
use super::tar;

pub async fn parse(context: ParseContext, data: Bytes) -> Result<Document> {
    let cursor = Cursor::new(&data);
    let mut decoder = GzDecoder::new(cursor);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| Error::ParseError(format!("Gzip decompression failed: {}", e)))?;

    // Check if it's a TAR file
    if is_tar(&decompressed) {
        let decompressed_bytes = Bytes::from(decompressed);
        // Delegate to TAR parser
        return tar::parse(context, decompressed_bytes).await;
    }

    // Otherwise, treat as a single file
    let mut rows = Vec::new();

    rows.push(TableRow {
        cells: vec![
            create_header_cell("Properties"),
            create_header_cell("Value"),
        ],
        height: None,
    });

    let original_size = data.len() as u64;
    let decompressed_size = decompressed.len() as u64;

    rows.push(create_prop_row("Type", "GZIP Compressed File"));
    rows.push(create_prop_row(
        "Original Size",
        &format_size(original_size),
    ));
    rows.push(create_prop_row(
        "Decompressed Size",
        &format_size(decompressed_size),
    ));
    rows.push(create_prop_row(
        "Ratio",
        &format!(
            "{:.1}%",
            (original_size as f64 / decompressed_size as f64) * 100.0
        ),
    ));

    let mut document = Document::new();
    let mut page = prism_core::document::Page::new(1, Dimensions::LETTER);

    let table = TableBlock {
        bounds: Rect::new(50.0, 50.0, 500.0, 200.0),
        rows,
        column_count: 2,
        style: Default::default(),
        rotation: 0.0,
    };

    page.add_content(ContentBlock::Table(table));
    document.pages.push(page);

    Ok(document)
}

fn is_tar(data: &[u8]) -> bool {
    if data.len() < 512 {
        return false;
    }
    // Check USTAR magic at offset 257 (5 bytes of "ustar" followed by NUL or space)
    // "ustar\0" or "ustar "
    let magic = &data[257..263]; // 6 bytes
    magic == b"ustar\0" || magic == b"ustar "
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

fn create_prop_row(key: &str, value: &str) -> TableRow {
    TableRow {
        cells: vec![create_text_cell(key), create_text_cell(value)],
        height: None,
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
