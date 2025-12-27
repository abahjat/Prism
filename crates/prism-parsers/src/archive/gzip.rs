// SPDX-License-Identifier: AGPL-3.0-only
//! GZIP archive parser

use bytes::Bytes;
use flate2::read::GzDecoder;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, ShapeStyle, TableBlock,
        TableCell, TableRow, TextBlock, TextRun,
    },
    error::{Error, Result},
    metadata::Metadata,
    parser::ParseContext,
};
use std::io::Read;
use tracing::debug;

/// Parse a `GZIP` archive
///
/// # Errors
///
/// Returns an error if decompression fails or the inner content is invalid.
pub fn parse(context: ParseContext, data: &Bytes) -> Result<Document> {
    let mut decoder = GzDecoder::new(&data[..]);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| Error::ParseError(format!("Gzip decompression failed: {e}")))?;

    // Check if inner content is a TAR archive
    if decompressed.len() > 512 && is_tar(&decompressed) {
        debug!("GZIP contains a TAR archive, delegating to tar::parse");
        return crate::archive::tar::parse(context, Bytes::from(decompressed));
    }

    // Otherwise, treat as a single file and create metadata
    let mut document = Document::new();

    // Create table with metadata
    let mut rows = Vec::new();
    rows.push(create_prop_row(
        "Filename",
        context.filename.as_deref().unwrap_or("unknown"),
    ));
    rows.push(create_prop_row(
        "Compressed Size",
        &format!("{} bytes", context.size),
    ));
    rows.push(create_prop_row(
        "Decompressed Size",
        &format!("{} bytes", decompressed.len()),
    ));

    #[allow(clippy::cast_precision_loss)]
    let ratio = (context.size as f64 / decompressed.len() as f64) * 100.0;
    rows.push(create_prop_row(
        "Compression Ratio",
        &format!("{ratio:.2}%"),
    ));

    let table = TableBlock {
        bounds: Rect::default(),
        rows,
        column_count: 2,
        style: ShapeStyle::default(),
        rotation: 0.0,
    };

    let page = Page {
        number: 1,
        dimensions: Dimensions::LETTER,
        content: vec![ContentBlock::Table(table)],
        metadata: PageMetadata::default(),
        annotations: Vec::new(),
    };

    let mut metadata = Metadata::default();
    if let Some(filename) = context.filename {
        metadata.title = Some(filename);
    }
    metadata.add_custom("format", "GZIP");
    metadata.add_custom(
        "decompressed_size",
        i64::try_from(decompressed.len()).unwrap_or(0),
    );

    document.pages = vec![page];
    document.metadata = metadata;

    Ok(document)
}

fn is_tar(data: &[u8]) -> bool {
    // Check USTAR magic at offset 257 (5 bytes of "ustar" followed by NUL or space)
    // "ustar\0" or "ustar "
    let magic = &data[257..263]; // 6 bytes
    magic == b"ustar\0" || magic == b"ustar "
}

fn create_header_cell(text: &str) -> TableCell {
    let mut run = TextRun::new(text);
    run.style.bold = true;

    let block = TextBlock {
        bounds: Rect::default(),
        runs: vec![run],
        paragraph_style: None,
        style: ShapeStyle::default(),
        rotation: 0.0,
    };

    TableCell {
        content: vec![ContentBlock::Text(block)],
        col_span: 1,
        row_span: 1,
        background_color: None,
    }
}

fn create_text_cell(text: &str) -> TableCell {
    let block = TextBlock {
        bounds: Rect::default(),
        runs: vec![TextRun::new(text)],
        paragraph_style: None,
        style: ShapeStyle::default(),
        rotation: 0.0,
    };

    TableCell {
        content: vec![ContentBlock::Text(block)],
        col_span: 1,
        row_span: 1,
        background_color: None,
    }
}

fn create_prop_row(name: &str, value: &str) -> TableRow {
    TableRow {
        cells: vec![create_header_cell(name), create_text_cell(value)],
        height: None,
    }
}
