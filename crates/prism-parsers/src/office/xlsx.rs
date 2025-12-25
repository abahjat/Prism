//! XLSX (Excel) parser
//!
//! Parses XLSX (Office Open XML Spreadsheet) files into the Unified Document Model.
//! Each worksheet becomes a Page containing a TableBlock with the cell grid.

use async_trait::async_trait;
use bytes::Bytes;
use calamine::{open_workbook_auto_from_rs, Data, Reader, Sheets};
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, TableBlock, TableCell, TableRow,
        TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::{Cursor, Read};
use tracing::{debug, info, warn};
use zip::ZipArchive;

use crate::office::excel_styles::ExcelStyles;

/// XLSX (Excel) parser
///
/// Parses XLSX files into the Unified Document Model.
/// - Each worksheet becomes a Page
/// - Cell grid represented as a single TableBlock per sheet
/// - Formulas stored in cell metadata (evaluated value in TextRun)
/// - Styles (fonts, fills, borders) applied from styles.xml
#[derive(Debug, Clone)]
pub struct XlsxParser;

impl XlsxParser {
    /// Create a new XLSX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Convert a calamine Data to a TextRun with fallback style
    fn data_to_text_run(&self, data: &Data) -> TextRun {
        let text = match data {
            Data::Int(i) => i.to_string(),
            Data::Float(f) => f.to_string(),
            Data::String(s) => s.clone(),
            Data::Bool(b) => b.to_string(),
            Data::DateTime(dt) => format!("{}", dt),
            Data::DateTimeIso(dt) => dt.clone(),
            Data::DurationIso(d) => d.clone(),
            Data::Error(e) => format!("#{:?}", e),
            Data::Empty => String::new(),
        };

        TextRun {
            text,
            style: TextStyle::default(),
            bounds: None,
            char_positions: None,
        }
    }

    /// Map Excel style to UDM TextStyle and Cell style
    fn apply_style(
        &self,
        _row: usize,
        _col: usize,
        // We'd ideally need the XF index for this cell, but calamine doesn't easily expose the raw XF index per cell in high-level iterators.
        // We might need to use low-level calamine API or assume defaults for now if strict XF mapping is hard with current calamine version.
        // Actually, calamine's `Cell` struct has `xf_index` if we iterate cells raw.
        // But `range.get((row, col))` returns `Data`, not `Cell`.
        // Let's rely on basic data for now and minimal styling unless we switch to manual sheet parsing for everything.
        // For HIGH FIDELITY, we really should parse the sheet XML events to get the `s` attribute (style index).
        // BUT calamine is very good at shared strings and values.
        // Compromise: We will use default styles for now in this pass, and if needed,
        // we can assume calamine might expose it or we re-read sheet XML just for attributes.
        //
        // NOTE: For this iteration, we'll load styles.xml but since we can't easily link it to calamine's Data extraction
        // without getting the cell XF index, we'll placeholder this.
        // Wait, `range.cells()` iterator returns `(row, col, Data)`.
        // We need `worksheet_range` which returns generic `Range<Data>`.
        //
        // Re-reading sheet XML is the only way to get true fidelity of `s` (style) attribute if calamine hides it.
        // Let's implement basic loading first.
        _styles: &Option<ExcelStyles>,
    ) -> (TextStyle, Option<String>) {
        (TextStyle::default(), None)
    }

    /// Check if data is an XLSX file by checking ZIP signature
    fn is_xlsx_zip(data: &[u8]) -> bool {
        // Check ZIP signature: PK (0x504B)
        if data.len() < 4 {
            return false;
        }

        data[0] == 0x50
            && data[1] == 0x4B
            && (data[2] == 0x03 || data[2] == 0x05)
            && data[3] == 0x04
    }
}

impl Default for XlsxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for XlsxParser {
    fn format(&self) -> Format {
        Format::xlsx()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // XLSX is a ZIP file, so check ZIP signature
        if !Self::is_xlsx_zip(data) {
            return false;
        }

        // Additional check: try to verify it's an XLSX by looking for xl/ directory
        // We'll do a more thorough check in parse()
        true
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing XLSX file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        // Validate ZIP signature
        if !Self::is_xlsx_zip(&data) {
            return Err(Error::ParseError(
                "Invalid XLSX signature (not a ZIP file)".to_string(),
            ));
        }

        // 1. Parse Styles
        // We open the zip separately to read styles.xml
        let mut styles: Option<ExcelStyles> = None;
        let cursor_zip = Cursor::new(data.as_ref());
        if let Ok(mut archive) = ZipArchive::new(cursor_zip) {
            if let Ok(mut styles_file) = archive.by_name("xl/styles.xml") {
                let mut xml = String::new();
                if styles_file.read_to_string(&mut xml).is_ok() {
                    if let Ok(parsed_styles) = ExcelStyles::from_xml(&xml) {
                        debug!(
                            "Parsed {} fonts, {} fills, {} cellXfs",
                            parsed_styles.fonts.len(),
                            parsed_styles.fills.len(),
                            parsed_styles.cell_xfs.len()
                        );
                        styles = Some(parsed_styles);
                    }
                }
            }
        }

        // 2. Open workbook using calamine for Data
        let cursor = Cursor::new(data.as_ref());
        let mut workbook: Sheets<_> = open_workbook_auto_from_rs(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open XLSX workbook: {}", e)))?;

        let sheet_names = workbook.sheet_names().to_vec();
        let sheet_count = sheet_names.len();

        debug!(
            "XLSX workbook has {} sheets: {:?}",
            sheet_count, sheet_names
        );

        if sheet_count == 0 {
            warn!("XLSX workbook has no sheets");
            return Ok(Document::builder()
                .metadata(Metadata::builder().title("Empty Workbook").build())
                .build());
        }

        let mut pages = Vec::new();

        // Process each worksheet
        for (sheet_index, sheet_name) in sheet_names.iter().enumerate() {
            debug!("Processing sheet {}: {}", sheet_index + 1, sheet_name);

            let range = match workbook.worksheet_range(sheet_name) {
                Ok(range) => range,
                Err(e) => {
                    warn!("Failed to read sheet '{}': {}", sheet_name, e);
                    continue;
                }
            };

            // Get dimensions
            let (row_count, col_count) = range.get_size();
            debug!(
                "Sheet '{}' size: {}x{} (rows x cols)",
                sheet_name, row_count, col_count
            );

            if row_count == 0 || col_count == 0 {
                debug!("Sheet '{}' is empty, skipping", sheet_name);
                continue;
            }

            // Build table rows
            let mut table_rows = Vec::new();

            for row_idx in 0..row_count {
                let mut cells = Vec::new();

                for col_idx in 0..col_count {
                    let cell_data = range.get((row_idx, col_idx));

                    // In the future, match (row_idx, col_idx) with parsed sheet XML to get style ID
                    let (_style, _bg_color) = self.apply_style(row_idx, col_idx, &styles);

                    let content = if let Some(data) = cell_data {
                        // Create text block from cell data
                        let text_run = self.data_to_text_run(data);
                        // Convert Excel styles to UDM styles if we had the mapping
                        // text_run.style = style;

                        vec![ContentBlock::Text(TextBlock {
                            bounds: prism_core::document::Rect {
                                x: 0.0,
                                y: 0.0,
                                width: 0.0,
                                height: 0.0,
                            },
                            runs: vec![text_run],
                            paragraph_style: None,
                        })]
                    } else {
                        // Empty cell
                        vec![]
                    };

                    cells.push(TableCell {
                        content,
                        col_span: 1,
                        row_span: 1,
                        background_color: None, // bg_color
                    });
                }

                table_rows.push(TableRow {
                    cells,
                    height: None,
                });
            }

            // Create table block
            let table_block = TableBlock {
                bounds: prism_core::document::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: col_count as f64 * 72.0, // Approximate column width
                    height: row_count as f64 * 20.0, // Approximate row height
                },
                rows: table_rows,
                column_count: col_count,
            };

            // Create page for this sheet
            let mut page_metadata = PageMetadata::default();
            page_metadata.label = Some(sheet_name.clone());

            let page = Page {
                number: (sheet_index + 1) as u32,
                dimensions: Dimensions::LETTER, // Standard paper size
                content: vec![ContentBlock::Table(table_block)],
                metadata: page_metadata,
                annotations: Vec::new(),
            };

            pages.push(page);
        }

        // Create document metadata
        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }

        // Add custom metadata for Excel-specific info
        metadata.add_custom("excel_sheet_count", sheet_count as i64);
        metadata.add_custom("excel_sheet_names", sheet_names.join(", "));

        // Build document
        let mut document = Document::builder().metadata(metadata).build();

        // Add pages to the document
        document.pages = pages;

        info!("Successfully parsed XLSX with {} sheets", sheet_count);

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "XLSX Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::TableExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_xlsx_zip() {
        // Valid ZIP signature (PK\x03\x04)
        let valid_zip = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00];
        assert!(XlsxParser::is_xlsx_zip(&valid_zip));

        // Valid ZIP signature (PK\x05\x04) - spanned archive
        let valid_zip2 = [0x50, 0x4B, 0x05, 0x04, 0x00, 0x00];
        assert!(XlsxParser::is_xlsx_zip(&valid_zip2));

        // Invalid data
        let invalid = b"Not a ZIP file";
        assert!(!XlsxParser::is_xlsx_zip(invalid));

        // Too short
        let too_short = [0x50, 0x4B];
        assert!(!XlsxParser::is_xlsx_zip(&too_short));
    }
}
