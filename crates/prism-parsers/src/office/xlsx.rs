// SPDX-License-Identifier: AGPL-3.0-only
//! XLSX (Microsoft Excel) parser
//!
//! Parses XLSX files into the Unified Document Model via [`prism_excel`].

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::Document,
    error::Result,
    format::Format,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use prism_excel::{is_xlsx_zip, parse_xlsx, workbook_to_udm};
use tracing::debug;

/// XLSX parser
#[derive(Debug, Clone)]
pub struct XlsxParser;

impl XlsxParser {
    /// Create a new XLSX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Parse an XLSX byte slice directly into a [`prism_excel::Workbook`].
    ///
    /// # Errors
    /// Returns an error if the ZIP archive is malformed or exceeds memory limits.
    pub fn parse_workbook(
        &self,
        data: &[u8],
        context: &ParseContext,
    ) -> Result<prism_excel::Workbook> {
        let limit = crate::limits::max_decompressed_bytes(context);
        parse_xlsx(data, limit)
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
        is_xlsx_zip(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing XLSX file: {:?}", context.filename);
        let wb = self.parse_workbook(data.as_ref(), &context)?;
        Ok(workbook_to_udm(&wb, "XLSX"))
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "XLSX Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
                ParserFeature::TableExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::document::ContentBlock;
    use prism_core::parser::ParseOptions;
    use std::io::{Cursor, Write};
    use zip::write::FileOptions;

    /// Workbook relationships pointing `rId1` at the first worksheet part.
    const WORKBOOK_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

    fn build_xlsx_zip(files: &[(&str, &[u8])]) -> Bytes {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = FileOptions::default();
            for (name, content) in files {
                zip.start_file(*name, options).unwrap();
                zip.write_all(content).unwrap();
            }
            zip.finish().unwrap();
        }
        Bytes::from(buf.into_inner())
    }

    #[tokio::test]
    async fn test_parse_xlsx_basic_table() {
        let workbook_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Sales" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#;

        let shared_strings_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="3" uniqueCount="3">
  <si><t>Product</t></si>
  <si><t>Revenue</t></si>
  <si><t>Widget</t></si>
</sst>"#;

        let sheet1_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1" t="s"><v>1</v></c>
    </row>
    <row r="2">
      <c r="A2" t="s"><v>2</v></c>
      <c r="B2"><v>1250</v></c>
    </row>
  </sheetData>
</worksheet>"#;

        let zip_bytes = build_xlsx_zip(&[
            ("xl/workbook.xml", workbook_xml),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/sharedStrings.xml", shared_strings_xml),
            ("xl/worksheets/sheet1.xml", sheet1_xml),
        ]);

        let parser = XlsxParser::new();
        assert!(parser.can_parse(&zip_bytes));

        let context = ParseContext {
            format: Format::xlsx(),
            filename: Some("sales.xlsx".to_string()),
            size: zip_bytes.len(),
            options: ParseOptions::default(),
        };

        let doc = parser.parse(zip_bytes, context).await.unwrap();
        assert_eq!(doc.page_count(), 1);
        assert_eq!(doc.pages[0].metadata.label.as_deref(), Some("Sales"));

        let ContentBlock::Table(table) = &doc.pages[0].content[0] else {
            panic!("Expected TableBlock on sheet page");
        };
        assert_eq!(table.column_count, 2);
        assert_eq!(table.rows.len(), 2);

        let extracted = doc.extract_text();
        assert!(extracted.contains("Product"));
        assert!(extracted.contains("Revenue"));
        assert!(extracted.contains("Widget"));
        assert!(extracted.contains("1250"));
    }

    #[tokio::test]
    async fn test_parse_xlsx_styles_and_merges() {
        let workbook_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Report" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#;

        let styles_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <fonts count="2">
    <font><sz val="11"/><name val="Calibri"/></font>
    <font><b/><sz val="14"/><color rgb="FFFFFFFF"/><name val="Arial"/></font>
  </fonts>
  <fills count="3">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
    <fill><patternFill patternType="solid"><fgColor rgb="FF1F4E78"/></patternFill></fill>
  </fills>
  <borders count="1">
    <border><left/><right/><top/><bottom/></border>
  </borders>
  <cellXfs count="2">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="0" fontId="1" fillId="2" borderId="0"/>
  </cellXfs>
</styleSheet>"#;

        let sheet1_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <cols>
    <col min="1" max="2" width="20" customWidth="1"/>
  </cols>
  <sheetData>
    <row r="1" ht="24">
      <c r="A1" s="1" t="inlineStr"><is><t>Merged Header</t></is></c>
      <c r="B1" s="1"/>
    </row>
  </sheetData>
  <mergeCells count="1">
    <mergeCell ref="A1:B1"/>
  </mergeCells>
</worksheet>"#;

        let zip_bytes = build_xlsx_zip(&[
            ("xl/workbook.xml", workbook_xml),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/styles.xml", styles_xml),
            ("xl/worksheets/sheet1.xml", sheet1_xml),
        ]);

        let parser = XlsxParser::new();
        let context = ParseContext {
            format: Format::xlsx(),
            filename: Some("report.xlsx".to_string()),
            size: zip_bytes.len(),
            options: ParseOptions::default(),
        };

        let doc = parser.parse(zip_bytes, context).await.unwrap();
        let ContentBlock::Table(table) = &doc.pages[0].content[0] else {
            panic!("Expected TableBlock");
        };

        let cell_a1 = &table.rows[0].cells[0];
        assert_eq!(cell_a1.col_span, 2);
        assert_eq!(cell_a1.row_span, 1);
        // B1 is covered by the merge, so the row holds only the spanning cell.
        assert_eq!(table.rows[0].cells.len(), 1);
        assert!(cell_a1.background_color.is_some());

        let ContentBlock::Text(tb) = &cell_a1.content[0] else {
            panic!("Expected TextBlock in cell");
        };
        assert!(tb.runs[0].style.bold);
        assert_eq!(tb.runs[0].style.font_family.as_deref(), Some("Arial"));
    }

    #[tokio::test]
    async fn test_parse_xlsx_number_formatting() {
        let workbook_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;

        let styles_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <numFmts count="1">
    <numFmt numFmtId="164" formatCode="yyyy-mm-dd"/>
  </numFmts>
  <fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
  <fills count="1"><fill><patternFill patternType="none"/></fill></fills>
  <borders count="1"><border><left/><right/><top/><bottom/></border></borders>
  <cellXfs count="3">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="164" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="9" fontId="0" fillId="0" borderId="0"/>
  </cellXfs>
</styleSheet>"#;

        let sheet1_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" s="1"><v>43831</v></c>
      <c r="B1" s="2"><v>0.5</v></c>
    </row>
  </sheetData>
</worksheet>"#;

        let zip_bytes = build_xlsx_zip(&[
            ("xl/workbook.xml", workbook_xml),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/styles.xml", styles_xml),
            ("xl/worksheets/sheet1.xml", sheet1_xml),
        ]);

        let parser = XlsxParser::new();
        let context = ParseContext {
            format: Format::xlsx(),
            filename: Some("fmt.xlsx".to_string()),
            size: zip_bytes.len(),
            options: ParseOptions::default(),
        };

        let doc = parser.parse(zip_bytes, context).await.unwrap();
        let text = doc.extract_text();
        assert!(
            text.contains("2020-01-01"),
            "Expected formatted date 2020-01-01, got: {text}"
        );
        assert!(
            text.contains("50%"),
            "Expected formatted percent 50%, got: {text}"
        );
    }
}
