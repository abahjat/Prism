// SPDX-License-Identifier: AGPL-3.0-only
use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, Rect, ShapeStyle, TableBlock, TableCell,
        TableRow, TextBlock, TextRun,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use rusqlite::Connection;
use std::io::Write;
use tempfile::NamedTempFile;

/// `SQLite` database parser
#[derive(Debug, Clone)]
pub struct SqliteParser;

impl SqliteParser {
    /// Create a new `SQLite` parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SqliteParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for SqliteParser {
    fn format(&self) -> Format {
        Format::sqlite()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // SQLite header: "SQLite format 3\0"
        if data.len() < 16 {
            return false;
        }
        &data[0..16] == b"SQLite format 3\0"
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        // rusqlite requires a file path, so we must write to a temp file
        let mut temp_file = NamedTempFile::new().map_err(|e| Error::ParseError(e.to_string()))?;
        temp_file
            .write_all(&data)
            .map_err(|e| Error::ParseError(e.to_string()))?;
        let path = temp_file.path();

        let conn = Connection::open(path).map_err(|e| Error::ParseError(e.to_string()))?;

        // Get list of tables
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .map_err(|e| Error::ParseError(e.to_string()))?;

        let table_names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| Error::ParseError(e.to_string()))?
            .filter_map(std::result::Result::ok)
            .collect();

        let mut pages = Vec::new();
        let mut page_num = 1;

        // If no tables, return empty doc
        if table_names.is_empty() {
            let mut page = Page::new(page_num, Dimensions::LETTER);
            page.add_content(ContentBlock::Text(create_text_block(
                "Empty SQLite Database",
            )));
            pages.push(page);
        }

        // Limit to first 5 tables to avoid huge docs
        for table_name in table_names.iter().take(5) {
            let mut page = Page::new(page_num, Dimensions::LETTER);

            // Title for the table
            let mut title_block = create_text_block(&format!("Table: {table_name}"));
            // Make title bold/large?
            title_block.runs[0].style.bold = true;
            page.add_content(ContentBlock::Text(title_block));

            // Query data (limit 50 rows)
            // We need column names first
            // PRAGMA table_info(table_name) is safer, but SELECT * LIMIT 0 gives columns too?
            // rusqlite stmt.column_names() works.

            let query = format!("SELECT * FROM \"{table_name}\" LIMIT 50");
            let mut stmt = conn
                .prepare(&query)
                .map_err(|e| Error::ParseError(e.to_string()))?;

            let col_count = stmt.column_count();
            let col_names: Vec<String> =
                stmt.column_names().into_iter().map(String::from).collect();

            let mut rows = Vec::new();

            // Header row
            rows.push(TableRow {
                cells: col_names.iter().map(|n| create_header_cell(n)).collect(),
                height: None,
            });

            // Data rows
            let mut query_rows = stmt
                .query([])
                .map_err(|e| Error::ParseError(e.to_string()))?;

            while let Some(row) = query_rows
                .next()
                .map_err(|e| Error::ParseError(e.to_string()))?
            {
                let mut cells = Vec::new();
                for i in 0..col_count {
                    // Get value as generic types
                    // We can try getting as String, or generic Value
                    let val: rusqlite::types::Value =
                        row.get(i).unwrap_or(rusqlite::types::Value::Null);
                    cells.push(create_text_cell(&format_sqlite_value(&val)));
                }
                rows.push(TableRow {
                    cells,
                    height: None,
                });
            }

            #[allow(clippy::cast_precision_loss)]
            let table_height = rows.len() as f64 * 20.0;
            let table = TableBlock {
                bounds: Rect::new(50.0, 100.0, 500.0, table_height),
                rows,
                column_count: col_count,
                column_widths: Vec::new(),
                style: ShapeStyle::default(),
                rotation: 0.0,
            };

            page.add_content(ContentBlock::Table(table));
            pages.push(page);
            page_num += 1;
        }

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "SQLite");
        metadata.add_custom("table_count", i64::try_from(table_names.len()).unwrap_or(0));

        let mut doc = Document::new();
        doc.pages = pages;
        doc.metadata = metadata;

        Ok(doc)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "SQLite Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TableExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

fn format_sqlite_value(val: &rusqlite::types::Value) -> String {
    match val {
        rusqlite::types::Value::Null => String::new(),
        rusqlite::types::Value::Integer(i) => i.to_string(),
        rusqlite::types::Value::Real(f) => f.to_string(),
        rusqlite::types::Value::Text(s) => s.clone(),
        rusqlite::types::Value::Blob(_) => "<BLOB>".to_string(),
    }
}

fn create_text_block(text: &str) -> TextBlock {
    let run = TextRun::new(text);
    TextBlock {
        vertical_alignment: None,
        bounds: Rect::default(),
        runs: vec![run],
        paragraph_style: None,
        style: ShapeStyle::default(),
        rotation: 0.0,
    }
}

fn create_header_cell(text: &str) -> TableCell {
    let mut run = TextRun::new(text);
    run.style.bold = true;
    let block = TextBlock {
        vertical_alignment: None,
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
