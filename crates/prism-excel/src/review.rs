// SPDX-License-Identifier: AGPL-3.0-only
//! Hidden-content and forensic findings provider for `Excel` workbooks (`.xlsx` and `.xls`).

use crate::model::{ConditionalRuleKind, SheetVisibility, Workbook, col_index_to_letters};
use prism_core::review::{Finding, FindingKind, HiddenContentReport, Location, Severity};

impl Workbook {
    /// Generate the cross-format [`HiddenContentReport`] for this workbook.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn hidden_content_report(&self, format_id: &str) -> HiddenContentReport {
        let mut report = HiddenContentReport::for_format(format_id);

        for (idx, sheet) in self.sheets.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let sheet_index = idx as u32;

            // 1. Hidden / veryHidden sheet
            match sheet.visibility {
                SheetVisibility::Visible => {}
                SheetVisibility::Hidden => {
                    report.push(Finding::new(
                        FindingKind::HiddenSheet,
                        Severity::Notable,
                        Location::SheetRange {
                            sheet_name: sheet.name.clone(),
                            sheet_index,
                            cell_range: None,
                        },
                        None,
                        Some(format!(
                            "Worksheet '{}' is hidden ({} populated cells)",
                            sheet.name,
                            sheet.cells.len()
                        )),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "visibility": "hidden",
                            "cell_count": sheet.cells.len(),
                        }),
                    ));
                }
                SheetVisibility::VeryHidden => {
                    report.push(Finding::new(
                        FindingKind::HiddenSheet,
                        Severity::High,
                        Location::SheetRange {
                            sheet_name: sheet.name.clone(),
                            sheet_index,
                            cell_range: None,
                        },
                        None,
                        Some(format!(
                            "Worksheet '{}' is veryHidden ({} populated cells)",
                            sheet.name,
                            sheet.cells.len()
                        )),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "visibility": "veryHidden",
                            "cell_count": sheet.cells.len(),
                        }),
                    ));
                }
            }

            // 2. Hidden rows and columns & collapsed outline groups
            let hidden_rows: Vec<u32> = sheet
                .row_props
                .iter()
                .filter_map(|(&r, rp)| rp.hidden.then_some(r + 1))
                .collect();
            if !hidden_rows.is_empty() {
                let range_desc = format!("Rows {:?}", &hidden_rows[..hidden_rows.len().min(10)]);
                report.push(Finding::new(
                    FindingKind::HiddenRowsOrColumns,
                    Severity::Notable,
                    Location::SheetRange {
                        sheet_name: sheet.name.clone(),
                        sheet_index,
                        cell_range: Some(format!("Row {}", hidden_rows[0])),
                    },
                    None,
                    Some(format!(
                        "{} hidden row(s) on '{}' ({range_desc})",
                        hidden_rows.len(),
                        sheet.name
                    )),
                    serde_json::json!({
                        "sheet_name": sheet.name,
                        "kind": "rows",
                        "count": hidden_rows.len(),
                        "rows_1based": &hidden_rows[..hidden_rows.len().min(32)],
                    }),
                ));
            }

            for span in &sheet.col_spans {
                if span.hidden {
                    let c1 = col_index_to_letters(span.min_col);
                    let c2 = col_index_to_letters(span.max_col);
                    let col_range = if span.min_col == span.max_col {
                        format!("{c1}:{c1}")
                    } else {
                        format!("{c1}:{c2}")
                    };
                    report.push(Finding::new(
                        FindingKind::HiddenRowsOrColumns,
                        Severity::Notable,
                        Location::SheetRange {
                            sheet_name: sheet.name.clone(),
                            sheet_index,
                            cell_range: Some(col_range.clone()),
                        },
                        None,
                        Some(format!(
                            "Hidden column span {col_range} on '{}'",
                            sheet.name
                        )),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "kind": "columns",
                            "range": col_range,
                        }),
                    ));
                }
                if span.outline_level > 0 && span.hidden {
                    let c1 = col_index_to_letters(span.min_col);
                    let c2 = col_index_to_letters(span.max_col);
                    report.push(Finding::new(
                        FindingKind::CollapsedOutlineGroup,
                        Severity::Notable,
                        Location::SheetRange {
                            sheet_name: sheet.name.clone(),
                            sheet_index,
                            cell_range: Some(format!("{c1}:{c2}")),
                        },
                        None,
                        Some(format!(
                            "Collapsed column outline group {c1}:{c2} (level {})",
                            span.outline_level
                        )),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "outline_level": span.outline_level,
                        }),
                    ));
                }
            }

            let collapsed_outline_rows: Vec<u32> = sheet
                .row_props
                .iter()
                .filter_map(|(&r, rp)| (rp.outline_level > 0 && rp.hidden).then_some(r + 1))
                .collect();
            if !collapsed_outline_rows.is_empty() {
                report.push(Finding::new(
                    FindingKind::CollapsedOutlineGroup,
                    Severity::Notable,
                    Location::SheetRange {
                        sheet_name: sheet.name.clone(),
                        sheet_index,
                        cell_range: Some(format!("Row {}", collapsed_outline_rows[0])),
                    },
                    None,
                    Some(format!(
                        "{} row(s) in collapsed outline group on '{}'",
                        collapsed_outline_rows.len(),
                        sheet.name
                    )),
                    serde_json::json!({
                        "sheet_name": sheet.name,
                        "rows_1based": &collapsed_outline_rows[..collapsed_outline_rows.len().min(32)],
                    }),
                ));
            }

            // 3. Comments
            for (&(r, c), comment) in &sheet.comments {
                let cell_ref = format!("{}{}", col_index_to_letters(c), r + 1);
                report.push(Finding::new(
                    FindingKind::Comment,
                    Severity::Notable,
                    Location::SheetRange {
                        sheet_name: sheet.name.clone(),
                        sheet_index,
                        cell_range: Some(cell_ref.clone()),
                    },
                    None,
                    Some(comment.text.clone()),
                    serde_json::json!({
                        "sheet_name": sheet.name,
                        "cell": cell_ref,
                        "author": comment.author,
                        "replies": comment.replies,
                    }),
                ));
            }

            // 4. Concealed cells (`;;;`, matching font/fill color, `< 2 pt`, zero-width chars)
            for (&(r, c), cell) in &sheet.cells {
                let raw = cell.value.raw_string();
                if raw.is_empty() && cell.formula.is_none() {
                    continue;
                }
                let cell_ref = format!("{}{}", col_index_to_letters(c), r + 1);
                let loc = Location::SheetRange {
                    sheet_name: sheet.name.clone(),
                    sheet_index,
                    cell_range: Some(cell_ref.clone()),
                };

                if cell.num_fmt_code.trim().starts_with(";;;") {
                    report.push(Finding::new(
                        FindingKind::CustomSuppressedCell,
                        Severity::High,
                        loc.clone(),
                        None,
                        Some(raw.clone()),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "cell": cell_ref,
                            "num_fmt_code": cell.num_fmt_code,
                            "raw_value": raw,
                        }),
                    ));
                } else if cell.is_concealed {
                    report.push(Finding::new(
                        FindingKind::WhiteOrLowContrastText,
                        Severity::High,
                        loc.clone(),
                        None,
                        Some(raw.clone()),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "cell": cell_ref,
                            "raw_value": raw,
                        }),
                    ));
                }

                if let Some(font) = self.styles.font_for_style(cell.style_idx)
                    && font.size_pt < 2.0
                    && !raw.trim().is_empty()
                {
                    report.push(Finding::new(
                        FindingKind::TinyText,
                        Severity::High,
                        loc.clone(),
                        None,
                        Some(raw.clone()),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "cell": cell_ref,
                            "font_size_pt": font.size_pt,
                        }),
                    ));
                }

                if raw.chars().any(|ch| {
                    matches!(
                        ch,
                        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}'
                    )
                }) {
                    report.push(Finding::new(
                        FindingKind::ZeroWidthCharacters,
                        Severity::Notable,
                        loc,
                        None,
                        Some(raw),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "cell": cell_ref,
                        }),
                    ));
                }
            }

            // 5. Unevaluated expression conditional formatting rules
            for rule in &sheet.conditional_rules {
                if let ConditionalRuleKind::Expression { ref formula } = rule.kind {
                    report.push(Finding::new(
                        FindingKind::DynamicField,
                        Severity::Notable,
                        Location::SheetRange {
                            sheet_name: sheet.name.clone(),
                            sheet_index,
                            cell_range: None,
                        },
                        None,
                        Some(format!("Conditional formatting expression: {formula}")),
                        serde_json::json!({
                            "sheet_name": sheet.name,
                            "formula": formula,
                        }),
                    ));
                }
            }
        }

        // 6. Hidden defined names
        for dn in &self.defined_names {
            if dn.hidden {
                report.push(Finding::new(
                    FindingKind::HiddenDefinedName,
                    Severity::Notable,
                    Location::Document,
                    None,
                    Some(format!("{} = {}", dn.name, dn.formula)),
                    serde_json::json!({
                        "name": dn.name,
                        "formula": dn.formula,
                        "local_sheet_id": dn.local_sheet_id,
                    }),
                ));
            }
        }

        // 7. Macros & external links
        if self.forensic.vba_project_present {
            report.push(Finding::new(
                FindingKind::MacroCode,
                Severity::High,
                Location::PartPath {
                    path: "xl/vbaProject.bin".to_string(),
                },
                None,
                Some("VBA macro project present".to_string()),
                serde_json::json!({ "vba_project_present": true }),
            ));
        }

        for link in &self.forensic.external_links {
            report.push(Finding::new(
                FindingKind::ExternalLinkOrConnection,
                Severity::Notable,
                Location::Document,
                None,
                Some(link.clone()),
                serde_json::json!({ "target": link }),
            ));
        }

        if self.author.is_some()
            || !self.forensic.comment_authors.is_empty()
            || self.forensic.workbook_protected
            || !self.forensic.protected_sheet_names.is_empty()
        {
            report.push(Finding::new(
                FindingKind::AuthorAndModificationHistory,
                Severity::Info,
                Location::Document,
                None,
                self.author.clone(),
                serde_json::json!({
                    "author": self.author,
                    "title": self.title,
                    "comment_authors": self.forensic.comment_authors,
                    "workbook_protected": self.forensic.workbook_protected,
                    "protected_sheet_names": self.forensic.protected_sheet_names,
                    "calc_id": self.forensic.calc_id,
                }),
            ));
        }

        report
    }
}
