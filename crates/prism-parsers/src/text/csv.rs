// SPDX-License-Identifier: AGPL-3.0-only
//! CSV parser implementation

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, ShapeStyle, TableBlock, TableCell,
        TableRow, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;
use tracing::{debug, info};

/// CSV file parser
#[derive(Debug, Clone)]
pub struct CsvParser;

impl CsvParser {
    /// Create a new CSV parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for CsvParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for CsvParser {
    fn format(&self) -> Format {
        Format::csv()
    }

    fn can_parse(&self, _data: &[u8]) -> bool {
        // Use naive check or assume dispatch handled it.
        // For CSV, almost any text file is valid CSV (single column).
        true
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing CSV file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        let cursor = Cursor::new(data.as_ref());
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false) // Treat all lines as data
            .flexible(true) // Allow variable length rows
            .from_reader(cursor);

        let mut pages = Vec::new();
        let rows_per_page = 100; // Chunk size for better rendering performance

        let mut current_rows = Vec::new();
        let mut max_cols = 0;
        let mut row_count = 0;

        for result in reader.records() {
            let record = result.map_err(|e| Error::ParseError(format!("CSV error: {}", e)))?;
            row_count += 1;

            let mut cells = Vec::new();
            for field in record.iter() {
                let run = TextRun {
                    text: field.to_string(),
                    style: TextStyle::default(),
                    bounds: None,
                    char_positions: None,
                };

                let content = if run.text.is_empty() {
                    vec![]
                } else {
                    vec![ContentBlock::Text(TextBlock {
                        bounds: prism_core::document::Rect::default(),
                        runs: vec![run],
                        paragraph_style: None,
                        style: ShapeStyle::default(),
                        rotation: 0.0,
                    })]
                };

                cells.push(TableCell {
                    content,
                    col_span: 1,
                    row_span: 1,
                    background_color: None,
                });
            }

            if cells.len() > max_cols {
                max_cols = cells.len();
            }

            current_rows.push(TableRow {
                cells,
                height: None,
            });

            if current_rows.len() >= rows_per_page {
                pages.push(Page {
                    number: (pages.len() + 1) as u32,
                    dimensions: Dimensions::LETTER, // Placeholder dimensions
                    content: vec![ContentBlock::Table(TableBlock {
                        bounds: prism_core::document::Rect::default(),
                        rows: current_rows,
                        column_count: max_cols,
                        style: ShapeStyle::default(),
                        rotation: 0.0,
                    })],
                    metadata: PageMetadata::default(),
                    annotations: vec![],
                });
                current_rows = Vec::new();
                max_cols = 0;
            }
        }

        // Flush remaining rows
        if !current_rows.is_empty() {
            pages.push(Page {
                number: (pages.len() + 1) as u32,
                dimensions: Dimensions::LETTER,
                content: vec![ContentBlock::Table(TableBlock {
                    bounds: prism_core::document::Rect::default(),
                    rows: current_rows,
                    column_count: max_cols,
                    style: ShapeStyle::default(),
                    rotation: 0.0,
                })],
                metadata: PageMetadata::default(),
                annotations: vec![],
            });
        }

        info!(
            "Successfully parsed CSV with {} rows into {} pages",
            row_count,
            pages.len()
        );

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("row_count", row_count as i64);

        let mut doc = Document::builder().metadata(metadata).build();
        doc.pages = pages;

        Ok(doc)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "CSV Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
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
    use prism_core::format::Format;
    use prism_core::metadata::MetadataValue;
    use prism_core::parser::ParseOptions;

    fn create_context(filename: &str, size: usize) -> ParseContext {
        ParseContext {
            format: Format::csv(),
            filename: Some(filename.to_string()),
            size,
            options: ParseOptions::default(),
        }
    }

    #[tokio::test]
    async fn test_csv_basic_parsing() {
        let parser = CsvParser::new();
        let csv_data = "col1,col2,col3\nval1,val2,val3\nval4,val5,val6";
        let context = create_context("test.csv", csv_data.len());

        let doc = parser
            .parse(Bytes::from(csv_data), context)
            .await
            .expect("Should parse CSV");

        assert_eq!(doc.pages.len(), 1);
        let page = &doc.pages[0];

        // Should have 1 table block
        assert_eq!(page.content.len(), 1);
        if let ContentBlock::Table(table) = &page.content[0] {
            assert_eq!(table.rows.len(), 3); // 3 rows total
            assert_eq!(table.column_count, 3);

            // Check first cell of first row
            let r0c0 = &table.rows[0].cells[0];
            if let ContentBlock::Text(text) = &r0c0.content[0] {
                assert_eq!(text.runs[0].text, "col1");
            } else {
                panic!("Expected text block");
            }

            // Check middle cell
            let r1c1 = &table.rows[1].cells[1];
            if let ContentBlock::Text(text) = &r1c1.content[0] {
                assert_eq!(text.runs[0].text, "val2");
            } else {
                panic!("Expected text block");
            }
        } else {
            panic!("Expected Table block");
        }

        // Check metadata
        if let Some(MetadataValue::Integer(row_count)) = doc.metadata.custom.get("row_count") {
            assert_eq!(*row_count, 3);
        } else {
            panic!("Expected row_count metadata");
        }
    }

    #[tokio::test]
    async fn test_csv_pagination() {
        let parser = CsvParser::new();
        // Generate 150 rows. 100 rows per page limit means 2 pages.
        let mut csv_string = String::new();
        for i in 0..150 {
            csv_string.push_str(&format!("row{},val{}\n", i, i));
        }

        let context = create_context("large.csv", csv_string.len());

        let doc = parser
            .parse(Bytes::from(csv_string), context)
            .await
            .expect("Should parse large CSV");

        assert_eq!(doc.pages.len(), 2);

        // Page 1 should have 100 rows
        if let ContentBlock::Table(table) = &doc.pages[0].content[0] {
            assert_eq!(table.rows.len(), 100);
        }

        // Page 2 should have 50 rows
        if let ContentBlock::Table(table) = &doc.pages[1].content[0] {
            assert_eq!(table.rows.len(), 50);
        }

        if let Some(MetadataValue::Integer(row_count)) = doc.metadata.custom.get("row_count") {
            assert_eq!(*row_count, 150);
        } else {
            panic!("Expected row_count metadata");
        }
    }

    #[tokio::test]
    async fn test_csv_flexible_rows() {
        let parser = CsvParser::new();
        // Row 1: 2 cols, Row 2: 3 cols
        let csv_data = "a,b\nc,d,e";
        let context = create_context("flexible.csv", csv_data.len());

        let doc = parser
            .parse(Bytes::from(csv_data), context)
            .await
            .expect("Should parse flexible CSV");

        if let ContentBlock::Table(table) = &doc.pages[0].content[0] {
            assert_eq!(table.rows.len(), 2);
            // The table's column_count should be the max found (3)
            assert_eq!(table.column_count, 3);

            assert_eq!(table.rows[0].cells.len(), 2);
            assert_eq!(table.rows[1].cells.len(), 3);
        }
    }
}
