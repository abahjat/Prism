//! XLSX (Excel) parser
//!
//! Parses XLSX (Office Open XML Spreadsheet) files into the Unified Document Model.
//! Each worksheet becomes a Page containing a TableBlock with the cell grid.

use async_trait::async_trait;
use bytes::Bytes;
use calamine::{open_workbook_auto_from_rs, Data, Reader, Sheets};
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, TableBlock, TableCell,
        TableRow, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::{debug, info, warn};

/// XLSX (Excel) parser
///
/// Parses XLSX files into the Unified Document Model.
/// - Each worksheet becomes a Page
/// - Cell grid represented as a single TableBlock per sheet
/// - Formulas stored in cell metadata (evaluated value in TextRun)
#[derive(Debug, Clone)]
pub struct XlsxParser;

impl XlsxParser {
    /// Create a new XLSX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Convert a calamine Data to a TextRun
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

    /// Check if data is an XLSX file by checking ZIP signature
    fn is_xlsx_zip(data: &[u8]) -> bool {
        // Check ZIP signature: PK (0x504B)
        if data.len() < 4 {
            return false;
        }

        data[0] == 0x50 && data[1] == 0x4B && (data[2] == 0x03 || data[2] == 0x05) && data[3] == 0x04
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
            context.size,
            context.filename
        );

        // Validate ZIP signature
        if !Self::is_xlsx_zip(&data) {
            return Err(Error::ParseError("Invalid XLSX signature (not a ZIP file)".to_string()));
        }

        // Open workbook using calamine
        let cursor = Cursor::new(data.as_ref());
        let mut workbook: Sheets<_> = open_workbook_auto_from_rs(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open XLSX workbook: {}", e)))?;

        let sheet_names = workbook.sheet_names().to_vec();
        let sheet_count = sheet_names.len();

        debug!("XLSX workbook has {} sheets: {:?}", sheet_count, sheet_names);

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
            debug!("Sheet '{}' size: {}x{} (rows x cols)", sheet_name, row_count, col_count);

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

                    let content = if let Some(data) = cell_data {
                        // Create text block from cell data
                        let text_run = self.data_to_text_run(data);
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
        let mut document = Document::builder()
            .metadata(metadata)
            .build();

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

    #[test]
    fn test_data_to_text_run() {
        let parser = XlsxParser::new();

        let int_data = Data::Int(42);
        let run = parser.data_to_text_run(&int_data);
        assert_eq!(run.text, "42");

        let float_data = Data::Float(3.14);
        let run = parser.data_to_text_run(&float_data);
        assert_eq!(run.text, "3.14");

        let string_data = Data::String("Hello".to_string());
        let run = parser.data_to_text_run(&string_data);
        assert_eq!(run.text, "Hello");

        let bool_data = Data::Bool(true);
        let run = parser.data_to_text_run(&bool_data);
        assert_eq!(run.text, "true");

        let empty_data = Data::Empty;
        let run = parser.data_to_text_run(&empty_data);
        assert_eq!(run.text, "");
    }

    #[test]
    fn test_parser_metadata() {
        let parser = XlsxParser::new();
        let metadata = parser.metadata();

        assert_eq!(metadata.name, "XLSX Parser");
        assert!(!metadata.requires_sandbox);
        assert!(metadata.features.contains(&ParserFeature::TextExtraction));
        assert!(metadata.features.contains(&ParserFeature::TableExtraction));
        assert!(metadata.features.contains(&ParserFeature::MetadataExtraction));
    }

    #[test]
    fn test_can_parse() {
        let parser = XlsxParser::new();

        // Valid ZIP signature
        let zip_data = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00];
        assert!(parser.can_parse(&zip_data));

        // Invalid data
        let invalid = b"Not XLSX";
        assert!(!parser.can_parse(invalid));
    }
}
