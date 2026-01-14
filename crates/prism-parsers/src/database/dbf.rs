// SPDX-License-Identifier: AGPL-3.0-only
use async_trait::async_trait;
use bytes::Bytes;
use dbase::FieldValue;
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
use std::io::Cursor;

/// DBF (dBase) database parser
#[derive(Debug, Clone)]
pub struct DbfParser;

impl DbfParser {
    /// Create a new DBF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for DbfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for DbfParser {
    fn format(&self) -> Format {
        Format::dbf()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // DBF files start with version byte (0x02, 0x03, 0x30, 0x31, etc.)
        // and have header structure. Simplest check is minimal size and maybe version byte range.
        if data.len() < 32 {
            return false;
        }
        // Valid version byte check is complex due to many variants, but we can trust extension/mime mostly
        true
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let cursor = Cursor::new(data);
        let mut reader =
            dbase::Reader::new(cursor).map_err(|e| Error::ParseError(e.to_string()))?;

        let mut rows = Vec::new();

        // Header row from fields
        let header_cells: Vec<TableCell> = reader
            .fields()
            .iter()
            .map(|f| create_header_cell(f.name()))
            .collect();
        let column_count = header_cells.len();

        rows.push(TableRow {
            cells: header_cells,
            height: None,
        });

        // Data rows (limit content for performance if needed, but we'll try all)
        for record_result in reader.iter_records() {
            let record = record_result.map_err(|e| Error::ParseError(e.to_string()))?;
            let mut cells = Vec::new();

            for (_name, value) in record {
                cells.push(create_text_cell(&format_field_value(&value)));
            }

            rows.push(TableRow {
                cells,
                height: None,
            });
        }

        let row_count = rows.len();

        let mut page = Page::new(1, Dimensions::LETTER);

        #[allow(clippy::cast_precision_loss)]
        let table_height = rows.len() as f64 * 20.0;
        let table = TableBlock {
            bounds: Rect::new(50.0, 50.0, 500.0, table_height),
            rows,
            column_count,
            column_widths: Vec::new(),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        page.add_content(ContentBlock::Table(table));

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "DBF");
        metadata.add_custom("record_count", i64::try_from(row_count - 1).unwrap_or(0));

        let mut doc = Document::new();
        doc.pages.push(page);
        doc.metadata = metadata;

        Ok(doc)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "DBF Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TableExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

fn format_field_value(value: &FieldValue) -> String {
    match value {
        FieldValue::Character(opt) => opt.clone().unwrap_or_default(),
        FieldValue::Currency(v) | FieldValue::Double(v) => format!("{v}"),
        FieldValue::Date(d) => {
            if let Some(date) = d {
                format!("{date}") // dbase::Date implements Display
            } else {
                String::new()
            }
        }
        FieldValue::DateTime(dt) => format!("{dt:?}"), // dbase::DateTime might not impl Display
        FieldValue::Float(v) => {
            if let Some(f) = v {
                format!("{f}")
            } else {
                String::new()
            }
        }
        FieldValue::Integer(v) => format!("{v}"),
        FieldValue::Logical(v) => {
            if let Some(b) = v {
                if *b {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            } else {
                String::new()
            }
        }
        FieldValue::Memo(s) => s.clone(),
        FieldValue::Numeric(v) => {
            if let Some(n) = v {
                format!("{n}")
            } else {
                String::new()
            }
        }
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
