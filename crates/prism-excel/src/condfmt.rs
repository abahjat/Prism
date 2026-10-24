// SPDX-License-Identifier: AGPL-3.0-only
//! Conditional formatting evaluator operating strictly on stored/cached cell values.
//!
//! Per ADR-0009:
//! - Rules (`cellIs`, `containsText`, `top10`, `aboveAverage`, `duplicateValues`, `colorScale`,
//!   `dataBar`, `iconSet`, etc.) are evaluated against the stored [`CellValue`]s in the [`Sheet`].
//! - Formula rules (`type="expression"`) are **never** evaluated; they are recorded in
//!   [`crate::model::WorkbookForensicFacts::unevaluated_conditional_rules`].

use crate::model::{
    Cell, CellRange, CellValue, CondOperator, ConditionalFormatRule, ConditionalRuleKind, DxfStyle,
    Sheet, WorkbookStyles, parse_a1_cell_ref,
};
use prism_layout::model::Rgba;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Resolved conditional formatting overrides and visual adornments for a single `(row, col)`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolvedCellConditional {
    /// Background fill color override (from `dxf` or `colorScale`).
    pub fill_color: Option<Rgba>,
    /// Font color override (from `dxf`).
    pub font_color: Option<Rgba>,
    /// Bold override (from `dxf`).
    pub bold: Option<bool>,
    /// Italic override (from `dxf`).
    pub italic: Option<bool>,
    /// Number format code override (from `dxf`).
    pub num_fmt_code: Option<String>,
    /// Data bar `(fraction_0_to_1, color, show_value)`.
    pub data_bar: Option<(f32, Rgba, bool)>,
    /// Icon set indicator `(glyph_str, color)`.
    pub icon: Option<(String, Rgba)>,
}

/// Evaluate all conditional formatting rules on `sheet` and return a sparse map of
/// `(row, col) -> ResolvedCellConditional` for cells that match one or more rules.
#[must_use]
pub fn evaluate_sheet_conditionals(
    sheet: &Sheet,
    styles: &WorkbookStyles,
) -> BTreeMap<(u32, u32), ResolvedCellConditional> {
    let mut out: BTreeMap<(u32, u32), ResolvedCellConditional> = BTreeMap::new();
    if sheet.conditional_rules.is_empty() {
        return out;
    }

    // Sort rules by ascending priority (1 = highest priority)
    let mut rules: Vec<&ConditionalFormatRule> = sheet.conditional_rules.iter().collect();
    rules.sort_by_key(|r| r.priority);

    let mut stopped_cells: std::collections::BTreeSet<(u32, u32)> =
        std::collections::BTreeSet::new();

    for rule in rules {
        if matches!(rule.kind, ConditionalRuleKind::Expression { .. }) {
            // Never evaluate arbitrary expression formulas
            continue;
        }

        // Gather populated cells inside `rule.ranges`
        let target_coords: Vec<(u32, u32)> = sheet
            .cells
            .keys()
            .copied()
            .filter(|&(r, c)| rule.ranges.iter().any(|rng| rng.contains(r, c)))
            .collect();

        if target_coords.is_empty() {
            continue;
        }

        // Precompute range statistics for aggregate rules (Top10, AboveAverage, ColorScale, DataBar, IconSet, DuplicateUnique)
        let stats = RangeStats::compute(sheet, &rule.ranges);
        let dxf = rule.dxf_id.and_then(|id| styles.dxfs.get(id as usize));

        for (r, c) in target_coords {
            if stopped_cells.contains(&(r, c)) {
                continue;
            }
            let Some(cell) = sheet.cells.get(&(r, c)) else {
                continue;
            };

            match &rule.kind {
                ConditionalRuleKind::ColorScale {
                    min_color,
                    mid_color,
                    max_color,
                } => {
                    if let Some(val) = cell.value.as_f64()
                        && let Some(t) = stats.normalized_position(val)
                    {
                        let color = if let Some(mid) = mid_color {
                            if t <= 0.5 {
                                lerp_rgba(*min_color, *mid, t * 2.0)
                            } else {
                                lerp_rgba(*mid, *max_color, (t - 0.5) * 2.0)
                            }
                        } else {
                            lerp_rgba(*min_color, *max_color, t)
                        };
                        let entry = out.entry((r, c)).or_default();
                        if entry.fill_color.is_none() {
                            entry.fill_color = Some(color);
                        }
                    }
                }
                ConditionalRuleKind::DataBar { color, show_value } => {
                    if let Some(val) = cell.value.as_f64()
                        && let Some(t) = stats.normalized_position(val)
                    {
                        let entry = out.entry((r, c)).or_default();
                        if entry.data_bar.is_none() {
                            entry.data_bar = Some((t.clamp(0.0, 1.0), *color, *show_value));
                        }
                    }
                }
                ConditionalRuleKind::IconSet {
                    icon_set, reverse, ..
                } => {
                    if let Some(val) = cell.value.as_f64()
                        && let Some(t) = stats.normalized_position(val)
                    {
                        let eff_t = if *reverse { 1.0 - t } else { t };
                        let icon = resolve_icon_for_fraction(icon_set, eff_t);
                        let entry = out.entry((r, c)).or_default();
                        if entry.icon.is_none() {
                            entry.icon = Some(icon);
                        }
                    }
                }
                other_kind => {
                    if matches_dxf_rule(other_kind, cell, sheet, &stats) {
                        if let Some(dxf_style) = dxf {
                            let entry = out.entry((r, c)).or_default();
                            apply_dxf(entry, dxf_style);
                        }
                        if rule.stop_if_true {
                            stopped_cells.insert((r, c));
                        }
                    }
                }
            }
        }
    }

    out
}

fn apply_dxf(target: &mut ResolvedCellConditional, dxf: &DxfStyle) {
    if target.fill_color.is_none() {
        target.fill_color = dxf.fill_color;
    }
    if target.font_color.is_none() {
        target.font_color = dxf.font_color;
    }
    if target.bold.is_none() {
        target.bold = dxf.bold;
    }
    if target.italic.is_none() {
        target.italic = dxf.italic;
    }
    if target.num_fmt_code.is_none() {
        target.num_fmt_code.clone_from(&dxf.num_fmt_code);
    }
}

struct RangeStats {
    numeric_sorted: Vec<f64>,
    min: Option<f64>,
    max: Option<f64>,
    mean: Option<f64>,
    value_counts: BTreeMap<String, usize>,
}

impl RangeStats {
    #[allow(clippy::cast_precision_loss)]
    fn compute(sheet: &Sheet, ranges: &[CellRange]) -> Self {
        let mut nums = Vec::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();

        for (&(r, c), cell) in &sheet.cells {
            if !ranges.iter().any(|rng| rng.contains(r, c)) {
                continue;
            }
            if let Some(n) = cell.value.as_f64()
                && n.is_finite()
            {
                nums.push(n);
            }
            let key = canonical_cell_key(&cell.value);
            if !key.is_empty() {
                *counts.entry(key).or_insert(0) += 1;
            }
        }

        nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let min = nums.first().copied();
        let max = nums.last().copied();
        let mean = if nums.is_empty() {
            None
        } else {
            Some(nums.iter().sum::<f64>() / (nums.len() as f64))
        };

        Self {
            numeric_sorted: nums,
            min,
            max,
            mean,
            value_counts: counts,
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn normalized_position(&self, val: f64) -> Option<f32> {
        let min = self.min?;
        let max = self.max?;
        let span = max - min;
        if span.abs() < 1e-12 {
            Some(0.5)
        } else {
            Some(((val - min) / span).clamp(0.0, 1.0) as f32)
        }
    }
}

fn canonical_cell_key(val: &CellValue) -> String {
    match val {
        CellValue::Empty => String::new(),
        CellValue::Number(n) => format!("N:{n:.9}"),
        CellValue::Text(s) | CellValue::IsoDate(s) => format!("S:{}", s.to_lowercase()),
        CellValue::RichText(runs) => {
            let s: String = runs.iter().map(|r| r.text.as_str()).collect();
            format!("S:{}", s.to_lowercase())
        }
        CellValue::Bool(b) => format!("B:{b}"),
        CellValue::Error(e) => format!("E:{e}"),
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn matches_dxf_rule(
    kind: &ConditionalRuleKind,
    cell: &Cell,
    sheet: &Sheet,
    stats: &RangeStats,
) -> bool {
    match kind {
        ConditionalRuleKind::CellIs {
            operator,
            formula1,
            formula2,
        } => eval_cell_is(*operator, &cell.value, formula1, formula2.as_deref(), sheet),
        ConditionalRuleKind::TextMatch { mode, text } => {
            let cell_str = cell.display_text.to_lowercase();
            let needle = text.trim_matches('"').to_lowercase();
            match mode.as_str() {
                "notContainsText" => !cell_str.contains(&needle),
                "beginsWith" => cell_str.starts_with(&needle),
                "endsWith" => cell_str.ends_with(&needle),
                _ => cell_str.contains(&needle),
            }
        }
        ConditionalRuleKind::BlankCheck { is_blank } => {
            let blank = matches!(cell.value, CellValue::Empty)
                || (matches!(&cell.value, CellValue::Text(s) if s.trim().is_empty()));
            if *is_blank { blank } else { !blank }
        }
        ConditionalRuleKind::ErrorCheck { is_error } => {
            let err = matches!(cell.value, CellValue::Error(_));
            if *is_error { err } else { !err }
        }
        ConditionalRuleKind::DuplicateUnique { duplicate } => {
            let key = canonical_cell_key(&cell.value);
            if key.is_empty() {
                return false;
            }
            let count = stats.value_counts.get(&key).copied().unwrap_or(0);
            if *duplicate { count > 1 } else { count == 1 }
        }
        ConditionalRuleKind::Top10 {
            rank,
            percent,
            bottom,
        } => {
            let Some(val) = cell.value.as_f64() else {
                return false;
            };
            let n_total = stats.numeric_sorted.len();
            if n_total == 0 || *rank == 0 {
                return false;
            }
            let count = if *percent {
                (((n_total as f64) * (f64::from(*rank) / 100.0)).ceil() as usize).clamp(1, n_total)
            } else {
                (*rank as usize).clamp(1, n_total)
            };
            if *bottom {
                let cutoff = stats.numeric_sorted[count - 1];
                val <= cutoff
            } else {
                let cutoff = stats.numeric_sorted[n_total - count];
                val >= cutoff
            }
        }
        ConditionalRuleKind::AboveAverage {
            above,
            equal_average,
        } => {
            let (Some(val), Some(mean)) = (cell.value.as_f64(), stats.mean) else {
                return false;
            };
            match (*above, *equal_average) {
                (true, true) => val >= mean,
                (true, false) => val > mean,
                (false, true) => val <= mean,
                (false, false) => val < mean,
            }
        }
        _ => false,
    }
}

fn eval_cell_is(
    op: CondOperator,
    cell_val: &CellValue,
    f1: &str,
    f2: Option<&str>,
    sheet: &Sheet,
) -> bool {
    if let Some(num) = cell_val.as_f64()
        && let Some(rhs1) = resolve_numeric_operand(f1, sheet)
    {
        return match op {
            CondOperator::LessThan => num < rhs1,
            CondOperator::LessThanOrEqual => num <= rhs1 + 1e-12,
            CondOperator::Equal => (num - rhs1).abs() <= 1e-9,
            CondOperator::NotEqual => (num - rhs1).abs() > 1e-9,
            CondOperator::GreaterThanOrEqual => num >= rhs1 - 1e-12,
            CondOperator::GreaterThan => num > rhs1,
            CondOperator::Between => {
                if let Some(rhs2) = f2.and_then(|s| resolve_numeric_operand(s, sheet)) {
                    let lo = rhs1.min(rhs2);
                    let hi = rhs1.max(rhs2);
                    num >= lo - 1e-12 && num <= hi + 1e-12
                } else {
                    false
                }
            }
            CondOperator::NotBetween => {
                if let Some(rhs2) = f2.and_then(|s| resolve_numeric_operand(s, sheet)) {
                    let lo = rhs1.min(rhs2);
                    let hi = rhs1.max(rhs2);
                    num < lo - 1e-12 || num > hi + 1e-12
                } else {
                    false
                }
            }
        };
    }

    // Fallback to case-insensitive string equality
    let lhs = cell_val.raw_string().to_lowercase();
    let rhs = resolve_string_operand(f1, sheet).to_lowercase();
    match op {
        CondOperator::Equal => lhs == rhs,
        CondOperator::NotEqual => lhs != rhs,
        CondOperator::LessThan => lhs < rhs,
        CondOperator::LessThanOrEqual => lhs <= rhs,
        CondOperator::GreaterThanOrEqual => lhs >= rhs,
        CondOperator::GreaterThan => lhs > rhs,
        _ => false,
    }
}

fn resolve_numeric_operand(expr: &str, sheet: &Sheet) -> Option<f64> {
    let trimmed = expr.trim().trim_start_matches('=');
    if let Ok(n) = trimmed.parse::<f64>() {
        return Some(n);
    }
    if let Some((r, c)) = parse_a1_cell_ref(trimmed)
        && let Some(cell) = sheet.cells.get(&(r, c))
    {
        return cell.value.as_f64();
    }
    None
}

fn resolve_string_operand(expr: &str, sheet: &Sheet) -> String {
    let trimmed = expr.trim().trim_start_matches('=');
    if let Some(stripped) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return stripped.to_string();
    }
    if let Some((r, c)) = parse_a1_cell_ref(trimmed)
        && let Some(cell) = sheet.cells.get(&(r, c))
    {
        return cell.value.raw_string();
    }
    trimmed.to_string()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
fn lerp_rgba(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let r = (f32::from(a.r) + (f32::from(b.r) - f32::from(a.r)) * t).round() as u8;
    let g = (f32::from(a.g) + (f32::from(b.g) - f32::from(a.g)) * t).round() as u8;
    let bl = (f32::from(a.b) + (f32::from(b.b) - f32::from(a.b)) * t).round() as u8;
    Rgba::rgb(r, g, bl)
}

fn resolve_icon_for_fraction(icon_set: &str, t: f32) -> (String, Rgba) {
    let green = Rgba::rgb(0x10, 0x7C, 0x41);
    let amber = Rgba::rgb(0xD9, 0x77, 0x06);
    let red = Rgba::rgb(0xDC, 0x26, 0x26);

    if icon_set.contains("TrafficLights") || icon_set.contains("Signs") {
        if t >= 0.67 {
            ("●".to_string(), green)
        } else if t >= 0.33 {
            ("●".to_string(), amber)
        } else {
            ("●".to_string(), red)
        }
    } else if icon_set.contains("Symbols") || icon_set.contains("Indicators") {
        if t >= 0.67 {
            ("✓".to_string(), green)
        } else if t >= 0.33 {
            ("!".to_string(), amber)
        } else {
            ("✕".to_string(), red)
        }
    } else {
        // Default 3Arrows / 4Arrows / 5Arrows
        if t >= 0.67 {
            ("▲".to_string(), green)
        } else if t >= 0.33 {
            ("▶".to_string(), amber)
        } else {
            ("▼".to_string(), red)
        }
    }
}
