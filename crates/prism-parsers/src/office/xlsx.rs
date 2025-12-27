// SPDX-License-Identifier: AGPL-3.0-only
//! XLSX (Excel) parser
//!
//! Parses XLSX (Office Open XML Spreadsheet) files into the Unified Document Model.
//! Each worksheet becomes a Page containing a TableBlock with the cell grid.

use async_trait::async_trait;
use bytes::Bytes;
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
use tracing::{debug, warn};
use zip::ZipArchive;

use crate::office::excel_styles::ExcelStyles;
use crate::office::theme::{parse_theme, Theme};

/// XLSX (Excel) parser
#[derive(Debug, Clone)]
pub struct XlsxParser;

impl XlsxParser {
    /// Create a new XLSX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data is an XLSX file by checking ZIP signature
    fn is_xlsx_zip(data: &[u8]) -> bool {
        if data.len() < 4 {
            return false;
        }
        data[0] == 0x50
            && data[1] == 0x4B
            && (data[2] == 0x03 || data[2] == 0x05)
            && data[3] == 0x04
    }

    /// Parse shared strings table
    fn parse_shared_strings(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<Vec<String>> {
        let mut strings = Vec::new();
        if let Ok(mut file) = archive.by_name("xl/sharedStrings.xml") {
            let mut xml = String::new();
            if file.read_to_string(&mut xml).is_ok() {
                let mut reader = quick_xml::Reader::from_str(&xml);
                reader.trim_text(true);
                let mut buf = Vec::new();
                let mut current_string = String::new();
                let mut in_si = false;

                loop {
                    match reader.read_event_into(&mut buf) {
                        Ok(quick_xml::events::Event::Start(e)) => {
                            if e.name().as_ref() == b"si" {
                                in_si = true;
                                current_string.clear();
                            } else if e.name().as_ref() == b"t" && in_si {
                                if let Ok(text) = reader.read_text(e.name()) {
                                    current_string.push_str(&text);
                                }
                            }
                        }
                        Ok(quick_xml::events::Event::End(e)) => {
                            if e.name().as_ref() == b"si" {
                                strings.push(current_string.clone());
                                in_si = false;
                            }
                        }
                        Ok(quick_xml::events::Event::Eof) => break,
                        _ => {}
                    }
                    buf.clear();
                }
            }
        }
        Ok(strings)
    }

    /// Parse workbook.xml to get sheets
    fn parse_workbook_sheets(
        archive: &mut ZipArchive<Cursor<&[u8]>>,
    ) -> Result<Vec<(String, String)>> {
        let mut sheets = Vec::new();
        if let Ok(mut file) = archive.by_name("xl/workbook.xml") {
            let mut xml = String::new();
            if file.read_to_string(&mut xml).is_ok() {
                let mut reader = quick_xml::Reader::from_str(&xml);
                reader.trim_text(true);
                let mut buf = Vec::new();

                loop {
                    match reader.read_event_into(&mut buf) {
                        Ok(quick_xml::events::Event::Empty(e))
                        | Ok(quick_xml::events::Event::Start(e)) => {
                            if e.name().as_ref() == b"sheet" {
                                let mut name = String::new();
                                let mut rid = String::new();
                                for attr in e.attributes().flatten() {
                                    match attr.key.as_ref() {
                                        b"name" => {
                                            name = crate::office::utils::attr_value(&attr.value)
                                        }
                                        b"r:id" => {
                                            rid = crate::office::utils::attr_value(&attr.value)
                                        }
                                        _ => {}
                                    }
                                }
                                if !name.is_empty() && !rid.is_empty() {
                                    sheets.push((name, rid));
                                }
                            }
                        }
                        Ok(quick_xml::events::Event::Eof) => break,
                        _ => {}
                    }
                    buf.clear();
                }
            }
        }
        Ok(sheets)
    }

    /// Parse workbook rels
    fn parse_workbook_rels(
        archive: &mut ZipArchive<Cursor<&[u8]>>,
    ) -> Result<std::collections::HashMap<String, String>> {
        let mut rels = std::collections::HashMap::new();
        if let Ok(mut file) = archive.by_name("xl/_rels/workbook.xml.rels") {
            let mut xml = String::new();
            if file.read_to_string(&mut xml).is_ok() {
                let mut reader = quick_xml::Reader::from_str(&xml);
                reader.trim_text(true);
                let mut buf = Vec::new();

                loop {
                    match reader.read_event_into(&mut buf) {
                        Ok(quick_xml::events::Event::Empty(e))
                        | Ok(quick_xml::events::Event::Start(e)) => {
                            if e.name().as_ref() == b"Relationship" {
                                let mut id = String::new();
                                let mut target = String::new();
                                for attr in e.attributes().flatten() {
                                    match attr.key.as_ref() {
                                        b"Id" => id = crate::office::utils::attr_value(&attr.value),
                                        b"Target" => {
                                            target = crate::office::utils::attr_value(&attr.value)
                                        }
                                        _ => {}
                                    }
                                }
                                if !id.is_empty() && !target.is_empty() {
                                    rels.insert(id, target);
                                }
                            }
                        }
                        Ok(quick_xml::events::Event::Eof) => break,
                        _ => {}
                    }
                    buf.clear();
                }
            }
        }
        Ok(rels)
    }

    /// Convert column letter to 0-based index (e.g. "A" -> 0, "Z" -> 25, "AA" -> 26)
    fn col_index_from_ref(r: &str) -> usize {
        let mut col_idx = 0;
        for c in r.chars() {
            if c.is_ascii_alphabetic() {
                col_idx = col_idx * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
            } else {
                break;
            }
        }
        if col_idx > 0 {
            col_idx - 1
        } else {
            0
        }
    }

    /// Parse a single sheet
    /// Parse a single sheet
    fn parse_sheet(
        xml: &str,
        shared_strings: &[String],
        styles: &Option<ExcelStyles>,
    ) -> Result<TableBlock> {
        Self::parse_sheet_clean(xml, shared_strings, styles)
    }

    fn parse_sheet_clean(
        xml: &str,
        shared_strings: &[String],
        styles: &Option<ExcelStyles>,
    ) -> Result<TableBlock> {
        let mut reader = quick_xml::Reader::from_str(xml);
        reader.trim_text(true);
        let mut buf = Vec::new();

        let mut rows: Vec<TableRow> = Vec::new();
        let mut max_col = 0;

        let mut in_row = false;
        let mut current_cells = Vec::new();

        // (r1, c1, r2, c2) - 0-based
        let mut merges: Vec<(usize, usize, usize, usize)> = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(e)) => {
                    match e.name().as_ref() {
                        b"row" => {
                            in_row = true;
                            current_cells = Vec::new();
                        }
                        b"c" if in_row => {
                            // Start of cell
                            let mut s_idx = 0;
                            let mut t_type = String::new();
                            let mut r_ref = String::new();

                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    b"s" => {
                                        s_idx = crate::office::utils::attr_value(&attr.value)
                                            .parse()
                                            .unwrap_or(0)
                                    }
                                    b"t" => t_type = crate::office::utils::attr_value(&attr.value),
                                    b"r" => r_ref = crate::office::utils::attr_value(&attr.value),
                                    _ => {}
                                }
                            }

                            // Fill gaps
                            if !r_ref.is_empty() {
                                let col_idx = Self::col_index_from_ref(&r_ref);
                                while current_cells.len() < col_idx {
                                    current_cells.push(TableCell {
                                        content: vec![],
                                        col_span: 1,
                                        row_span: 1,
                                        background_color: None,
                                    });
                                }
                            }

                            // Parse value
                            let mut val = String::new();
                            let mut cell_buf = Vec::new();
                            loop {
                                match reader.read_event_into(&mut cell_buf) {
                                    Ok(quick_xml::events::Event::Start(v)) => {
                                        if v.name().as_ref() == b"v" || v.name().as_ref() == b"is" {
                                            // v=value, is=inline string
                                            if let Ok(text) = reader.read_text(v.name()) {
                                                val = text.into_owned();
                                            }
                                        }
                                    }
                                    Ok(quick_xml::events::Event::End(ce))
                                        if ce.name().as_ref() == b"c" =>
                                    {
                                        break
                                    }
                                    _ => {}
                                }
                                cell_buf.clear();
                            }

                            // Process Content
                            // Resolve Value
                            let display_text = if t_type == "s" {
                                if let Ok(idx) = val.parse::<usize>() {
                                    shared_strings.get(idx).cloned().unwrap_or_default()
                                } else {
                                    val
                                }
                            } else if t_type == "str" {
                                val
                            } else {
                                val
                            };

                            // Resolve Style
                            let (text_style, bg_color) = if let Some(styles) = styles {
                                styles.get_style(s_idx)
                            } else {
                                (TextStyle::default(), None)
                            };

                            let mut run = TextRun::new(display_text);
                            run.style = text_style;

                            let content = if run.text.is_empty() {
                                vec![]
                            } else {
                                vec![ContentBlock::Text(TextBlock {
                                    bounds: prism_core::document::Rect::default(),
                                    runs: vec![run],
                                    paragraph_style: None,
                                    style: prism_core::document::ShapeStyle::default(),
                                    rotation: 0.0,
                                })]
                            };

                            current_cells.push(TableCell {
                                content,
                                col_span: 1,
                                row_span: 1,
                                background_color: bg_color,
                            });
                        }
                        _ => {}
                    }
                }
                Ok(quick_xml::events::Event::Empty(e)) => {
                    if e.name().as_ref() == b"mergeCell" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"ref" {
                                let ref_val = crate::office::utils::attr_value(&attr.value);
                                // Parse "A1:B2"
                                if let Some((p1, p2)) = ref_val.split_once(':') {
                                    let c1 = Self::col_index_from_ref(p1);
                                    let c2 = Self::col_index_from_ref(p2);

                                    // Row index extraction
                                    let parse_row = |s: &str| -> Option<usize> {
                                        let digits: String =
                                            s.chars().filter(|c| c.is_ascii_digit()).collect();
                                        digits.parse::<usize>().ok().map(|r| {
                                            if r > 0 {
                                                r - 1
                                            } else {
                                                0
                                            }
                                        })
                                    };

                                    if let (Some(r1), Some(r2)) = (parse_row(p1), parse_row(p2)) {
                                        merges.push((r1, c1, r2, c2));
                                    }
                                }
                            }
                        }
                    } else if in_row && e.name().as_ref() == b"c" {
                        // Empty Cell <c ... />
                        let mut s_idx = 0;
                        let mut r_ref = String::new();
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"s" => {
                                    s_idx = crate::office::utils::attr_value(&attr.value)
                                        .parse()
                                        .unwrap_or(0);
                                }
                                b"r" => r_ref = crate::office::utils::attr_value(&attr.value),
                                _ => {}
                            }
                        }

                        // Fill gaps
                        if !r_ref.is_empty() {
                            let col_idx = Self::col_index_from_ref(&r_ref);
                            while current_cells.len() < col_idx {
                                current_cells.push(TableCell {
                                    content: vec![],
                                    col_span: 1,
                                    row_span: 1,
                                    background_color: None,
                                });
                            }
                        }

                        let (_, bg_color) = if let Some(styles) = styles {
                            styles.get_style(s_idx)
                        } else {
                            (TextStyle::default(), None)
                        };

                        current_cells.push(TableCell {
                            content: vec![],
                            col_span: 1,
                            row_span: 1,
                            background_color: bg_color,
                        });
                    }
                }
                Ok(quick_xml::events::Event::End(e)) => {
                    if e.name().as_ref() == b"row" {
                        if current_cells.len() > max_col {
                            max_col = current_cells.len();
                        }
                        rows.push(TableRow {
                            cells: current_cells.clone(), // Clone needed because we reuse
                            height: None,
                        });
                        in_row = false;
                    }
                }
                Ok(quick_xml::events::Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }

        // Apply Merged Cells
        let mut cells_to_remove: Vec<(usize, usize)> = Vec::new();

        for (r1, c1, r2, c2) in merges {
            // Validate bounds
            if r1 >= rows.len() {
                continue;
            }

            // Set spans on master cell
            if c1 < rows[r1].cells.len() {
                let rowspan = r2 - r1 + 1;
                let colspan = c2 - c1 + 1;
                rows[r1].cells[c1].row_span = rowspan;
                rows[r1].cells[c1].col_span = colspan;
            }

            // Mark covered cells
            for r in r1..=r2 {
                for c in c1..=c2 {
                    if r == r1 && c == c1 {
                        continue;
                    }
                    cells_to_remove.push((r, c));
                }
            }
        }

        // Remove cells.
        // We have (r, c) where c is the original index.
        // Sorting removals: by row, then by col DESCENDING.
        cells_to_remove.sort_by(|a, b| {
            if a.0 != b.0 {
                a.0.cmp(&b.0)
            } else {
                b.1.cmp(&a.1) // Descending column
            }
        });
        cells_to_remove.dedup();

        for (r, c) in cells_to_remove {
            if r < rows.len() {
                if c < rows[r].cells.len() {
                    rows[r].cells.remove(c);
                }
            }
        }

        Ok(TableBlock {
            bounds: prism_core::document::Rect::default(),
            rows,
            column_count: max_col,
            style: prism_core::document::ShapeStyle::default(),
            rotation: 0.0,
        })
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
        true
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!(
            "Parsing XLSX file, size: {} bytes, filename: {:?}",
            context.size, context.filename
        );

        if !Self::is_xlsx_zip(&data) {
            return Err(Error::ParseError("Invalid XLSX signature".to_string()));
        }

        let reader = Cursor::new(data.as_ref());
        let mut archive =
            ZipArchive::new(reader).map_err(|e| Error::ParseError(format!("ZIP error: {}", e)))?;

        // 1. Parse Shared Strings
        let shared_strings = Self::parse_shared_strings(&mut archive)?;
        debug!("Parsed {} shared strings", shared_strings.len());

        // 2. Parse Theme
        let mut theme: Option<Theme> = None;
        if let Ok(mut file) = archive.by_name("xl/theme/theme1.xml") {
            let mut xml = Vec::new();
            if file.read_to_end(&mut xml).is_ok() {
                if let Ok(t) = parse_theme(&xml) {
                    theme = Some(t);
                    debug!("Parsed theme with color scheme");
                }
            }
        }

        // 3. Parse Styles
        let mut styles: Option<ExcelStyles> = None;
        if let Ok(mut styles_file) = archive.by_name("xl/styles.xml") {
            let mut xml = String::new();
            if styles_file.read_to_string(&mut xml).is_ok() {
                if let Ok(parsed_styles) = ExcelStyles::from_xml(&xml, theme.as_ref()) {
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

        // 3. Parse Workbook & Rels to find sheets
        let sheets = Self::parse_workbook_sheets(&mut archive)?;
        let rels = Self::parse_workbook_rels(&mut archive)?;

        let mut pages = Vec::new();

        for (i, (name, rid)) in sheets.iter().enumerate() {
            if let Some(target) = rels.get(rid) {
                // Target is relative to xl/ usually, like "worksheets/sheet1.xml"
                // but can be just "sheet1.xml" if target is relative to workbook.xml?
                // Usually relation target is from xl/ directory context if workbook is in xl/.
                // Let's assume standard structure: xl/worksheets/sheetN.xml
                // We should handle path joining properly if possible, but for now simple concat.
                let zip_path = if target.starts_with('/') {
                    target.trim_start_matches('/').to_string()
                } else {
                    format!("xl/{}", target)
                };

                debug!("Processing sheet '{}' at {}", name, zip_path);

                if let Ok(mut sheet_file) = archive.by_name(&zip_path) {
                    let mut xml = String::new();
                    if sheet_file.read_to_string(&mut xml).is_ok() {
                        match Self::parse_sheet(&xml, &shared_strings, &styles) {
                            Ok(table) => {
                                let mut page_meta = PageMetadata::default();
                                page_meta.label = Some(name.clone());

                                pages.push(Page {
                                    number: (i + 1) as u32,
                                    dimensions: Dimensions::LETTER,
                                    content: vec![ContentBlock::Table(table)],
                                    metadata: page_meta,
                                    annotations: vec![],
                                });
                            }
                            Err(e) => {
                                warn!("Failed to parse sheet {}: {}", name, e);
                            }
                        }
                    }
                } else {
                    warn!("Could not find sheet file in zip: {}", zip_path);
                }
            }
        }

        let mut metadata = Metadata::default();
        if let Some(ref f) = context.filename {
            metadata.title = Some(f.clone());
        }
        metadata.add_custom("excel_sheet_count", sheets.len() as i64);

        let mut doc = Document::builder().metadata(metadata).build();
        doc.pages = pages;

        Ok(doc)
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
        let valid_zip = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00];
        assert!(XlsxParser::is_xlsx_zip(&valid_zip));
        let invalid = b"Not a ZIP file";
        assert!(!XlsxParser::is_xlsx_zip(invalid));
    }

    #[test]
    fn test_parse_sheet_logic() {
        use crate::office::excel_styles::{CellXf, ExcelFill, ExcelFont};

        let xml = r#"
            <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
                <sheetData>
                    <row r="1">
                        <c r="A1" t="str"><v>Header</v></c>
                        <c r="B1" t="s"><v>0</v></c>
                    </row>
                    <row r="2">
                        <c r="A2" s="1"><v>123</v></c>
                        <c r="B2" s="2"><v>456</v></c>
                    </row>
                </sheetData>
            </worksheet>
        "#;

        let shared_strings = vec!["SharedString".to_string()];

        // Mock styles
        let mut styles = ExcelStyles::default();
        styles.cell_xfs.push(CellXf::default()); // Index 0 (Default)

        let mut style1 = CellXf::default();
        style1.font_id = 0;
        styles.cell_xfs.push(style1); // Index 1
        styles.fonts.push(ExcelFont {
            name: "Arial".to_string(),
            size: 12.0,
            color: Some("#FF0000".to_string()),
            ..Default::default()
        });

        let mut style2 = CellXf::default();
        style2.fill_id = 0;
        styles.cell_xfs.push(style2); // Index 2
        styles.fills.push(ExcelFill {
            pattern_type: "solid".to_string(),
            fg_color: Some("#00FF00".to_string()),
            ..Default::default()
        });

        let result = XlsxParser::parse_sheet_clean(xml, &shared_strings, &Some(styles));
        assert!(result.is_ok());

        let table = result.unwrap();
        assert_eq!(table.rows.len(), 2);

        // Check Row 1
        // A1: Inline String "Header"
        assert_eq!(table.rows[0].cells[0].content.len(), 1);
        if let ContentBlock::Text(ref tb) = table.rows[0].cells[0].content[0] {
            assert_eq!(tb.runs[0].text, "Header");
        } else {
            panic!("Expected TextBlock in A1");
        }

        // B1: Shared String "SharedString" (index 0)
        if let ContentBlock::Text(ref tb) = table.rows[0].cells[1].content[0] {
            assert_eq!(tb.runs[0].text, "SharedString");
        }

        // Check Row 2
        // A2: Number "123" with Style 1 (Font Red)
        if let ContentBlock::Text(ref tb) = table.rows[1].cells[0].content[0] {
            assert_eq!(tb.runs[0].text, "123");
            assert_eq!(tb.runs[0].style.font_family.as_deref(), Some("Arial"));
            assert_eq!(tb.runs[0].style.color.as_deref(), Some("#FF0000"));
        }

        // B2: Number "456" with Style 2 (Fill Green)
        if let ContentBlock::Text(ref tb) = table.rows[1].cells[1].content[0] {
            assert_eq!(tb.runs[0].text, "456");
        }
        assert_eq!(
            table.rows[1].cells[1].background_color.as_deref(),
            Some("#00FF00")
        );
    }

    #[test]
    fn test_parse_sheet_merged() {
        let xml = r#"
            <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
                <sheetData>
                    <row r="1">
                        <c r="A1" t="str"><v>Merged Title</v></c>
                        <c r="C1" t="str"><v>After Merge</v></c>
                    </row>
                    <row r="2">
                        <c r="A2"><v>1</v></c>
                        <c r="B2"><v>2</v></c>
                        <c r="C2"><v>3</v></c>
                    </row>
                </sheetData>
                <mergeCells>
                    <mergeCell ref="A1:B1"/>
                </mergeCells>
            </worksheet>
        "#;
        let shared_strings = vec![];
        let styles = None;

        let result = XlsxParser::parse_sheet_clean(xml, &shared_strings, &styles);
        assert!(result.is_ok());
        let table = result.unwrap();

        assert_eq!(table.rows.len(), 2);

        // Row 1: A1 spans 2. B1 should be removed. C1 should remain.
        assert_eq!(table.rows[0].cells.len(), 2);

        // A1
        assert_eq!(table.rows[0].cells[0].col_span, 2);
        if let ContentBlock::Text(ref tb) = table.rows[0].cells[0].content[0] {
            assert_eq!(tb.runs[0].text, "Merged Title");
        }

        // Next cell should be C1 (After Merge)
        if let ContentBlock::Text(ref tb) = table.rows[0].cells[1].content[0] {
            assert_eq!(tb.runs[0].text, "After Merge");
        }

        // Row 2: No merges. [A2, B2, C2].
        assert_eq!(table.rows[1].cells.len(), 3);
        assert_eq!(table.rows[1].cells[0].col_span, 1);
        assert_eq!(table.rows[1].cells[1].col_span, 1);
    }
}
