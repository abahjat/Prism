// SPDX-License-Identifier: AGPL-3.0-only
//! UNIX Compress (.z) parser
//!
//! Handles files compressed with the UNIX `compress` utility (LZW compression).

use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, ShapeStyle, TableBlock,
        TableCell, TableRow, TextBlock, TextRun,
    },
    error::{Error, Result},
    metadata::Metadata,
    parser::ParseContext,
};
use tracing::debug;

/// Parse a UNIX Compress (.z) file
///
/// # Errors
///
/// Returns an error if the file has an invalid UNIX Compress signature.
#[allow(clippy::needless_pass_by_value)]
pub fn parse(context: ParseContext, data: &[u8]) -> Result<Document> {
    debug!(
        "Parsing UNIX Compress file, size: {} bytes, filename: {:?}",
        context.size, context.filename
    );

    // Validate UNIX Compress signature (0x1F 0x9D)
    if data.len() < 3 || data[0] != 0x1F || data[1] != 0x9D {
        return Err(Error::ParseError(
            "Invalid UNIX Compress signature".to_string(),
        ));
    }

    // The third byte contains flags:
    // bit 7: block_mode (if set, use block compression)
    // bits 0-4: max_bits (LZW dictionary size, typically 9-16)
    let flags = data[2];
    let max_bits = flags & 0x1F;
    let block_mode = (flags & 0x80) != 0;

    // Get original filename if available (remove .Z extension)
    let original_name = context
        .filename
        .as_ref()
        .map(|f| {
            if f.to_lowercase().ends_with(".z") {
                f[..f.len() - 2].to_string()
            } else {
                f.clone()
            }
        })
        .unwrap_or_default();

    // Create table with metadata
    let rows = vec![
        create_prop_row("Format", "UNIX Compress (LZW)"),
        create_prop_row("Compressed Size", &format!("{} bytes", data.len())),
        create_prop_row("Max Bits", &format!("{max_bits}")),
        create_prop_row("Block Mode", if block_mode { "Yes" } else { "No" }),
        create_prop_row("Original Filename", &original_name),
    ];

    let table = TableBlock {
        bounds: Rect::new(50.0, 100.0, 600.0, 200.0),
        rows,
        column_count: 2,
        column_widths: Vec::new(),
        style: ShapeStyle::default(),
        rotation: 0.0,
    };

    // Create header text
    let header_run = TextRun {
        text: "UNIX Compress Archive (.Z)".to_string(),
        style: prism_core::document::TextStyle {
            bold: true,
            font_size: Some(18.0),
            ..Default::default()
        },
        bounds: None,
        char_positions: None,
    };

    let header_block = TextBlock {
        vertical_alignment: None,
        runs: vec![header_run],
        paragraph_style: None,
        bounds: Rect::new(50.0, 30.0, 500.0, 50.0),
        style: ShapeStyle::default(),
        rotation: 0.0,
    };

    // Create note text
    let note_run = TextRun {
        text: "Note: Full decompression requires the uncompress utility or compatible library."
            .to_string(),
        style: prism_core::document::TextStyle::default(),
        bounds: None,
        char_positions: None,
    };

    let note_block = TextBlock {
        vertical_alignment: None,
        runs: vec![note_run],
        paragraph_style: None,
        bounds: Rect::new(50.0, 320.0, 600.0, 50.0),
        style: ShapeStyle::default(),
        rotation: 0.0,
    };

    let page = Page {
        number: 1,
        dimensions: Dimensions::LETTER,
        content: vec![
            ContentBlock::Text(header_block),
            ContentBlock::Table(table),
            ContentBlock::Text(note_block),
        ],
        metadata: PageMetadata::default(),
        annotations: Vec::new(),
    };

    let mut metadata = Metadata::default();
    if let Some(ref filename) = context.filename {
        metadata.title = Some(filename.clone());
    }
    metadata.add_custom("format", "UNIX Compress");
    metadata.add_custom("max_bits", max_bits.to_string());
    metadata.add_custom("block_mode", block_mode.to_string());
    if !original_name.is_empty() {
        metadata.add_custom("original_filename", original_name);
    }

    let mut document = Document::new();
    document.pages = vec![page];
    document.metadata = metadata;

    debug!("Successfully parsed UNIX Compress file");

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
        style: ShapeStyle::default(),
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

fn create_text_cell(text: &str) -> TableCell {
    let block = TextBlock {
        bounds: Rect::default(),
        runs: vec![TextRun::new(text)],
        paragraph_style: None,
        vertical_alignment: None,
        style: ShapeStyle::default(),
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

fn create_prop_row(name: &str, value: &str) -> TableRow {
    TableRow {
        cells: vec![create_header_cell(name), create_text_cell(value)],
        height: None,
    }
}
