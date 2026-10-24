// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic ECMA-376 / Excel number-format engine (`prism-numfmt`).
//!
//! Formats stored cell values into the exact strings Excel displays without recalculating
//! formulas or consulting the host operating system's locale settings.

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

use serde::{Deserialize, Serialize};
use std::fmt::Write;

/// Workbook date system (`1900` vs `1904`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DateSystem {
    /// 1900 date system (default on Windows Excel), including the Lotus 1-2-3 leap-year bug
    /// where serial `60` is `1900-02-29`.
    #[default]
    Date1900,
    /// 1904 date system (historical Macintosh default; serial `0` is `1904-01-01`).
    Date1904,
}

/// Options controlling number and date formatting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatOptions {
    /// Date system used by the workbook.
    pub date_system: DateSystem,
    /// Decimal separator character (default `'.'`).
    pub decimal_sep: char,
    /// Thousands grouping separator character (default `','`).
    pub group_sep: char,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            date_system: DateSystem::Date1900,
            decimal_sep: '.',
            group_sep: ',',
        }
    }
}

/// Input value to be formatted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CellInputValue {
    /// Empty cell.
    Empty,
    /// IEEE-754 floating-point number (or date serial).
    Number(f64),
    /// String value.
    Text(String),
    /// Boolean value (`TRUE` or `FALSE`).
    Bool(bool),
    /// Error token (`#DIV/0!`, `#N/A`, `#VALUE!`, etc.).
    Error(String),
}

/// Result of formatting a cell value with a number format string.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormattedValue {
    /// Displayed text string.
    pub text: String,
    /// Optional hex color override from a format token such as `[Red]` or `[Color10]`.
    pub color: Option<String>,
    /// True if the format intentionally conceals the value (e.g. `;;;`).
    pub is_concealed: bool,
    /// Optional `*` repeat-fill character from the format string.
    pub fill_char: Option<char>,
}

/// Look up the built-in ECMA-376 §18.8.30 format code for `num_fmt_id` (`0..=49`).
#[must_use]
pub fn builtin_format_code(num_fmt_id: u32) -> Option<&'static str> {
    match num_fmt_id {
        0 => Some("General"),
        1 => Some("0"),
        2 => Some("0.00"),
        3 => Some("#,##0"),
        4 => Some("#,##0.00"),
        5 => Some("$#,##0_);($#,##0)"),
        6 => Some("$#,##0_);[Red]($#,##0)"),
        7 => Some("$#,##0.00_);($#,##0.00)"),
        8 => Some("$#,##0.00_);[Red]($#,##0.00)"),
        9 => Some("0%"),
        10 => Some("0.00%"),
        11 => Some("0.00E+00"),
        12 => Some("# ?/?"),
        13 => Some("# ??/??"),
        14 | 27..=31 | 36 | 50..=58 => Some("m/d/yyyy"),
        15 => Some("d-mmm-yy"),
        16 => Some("d-mmm"),
        17 => Some("mmm-yy"),
        18 => Some("h:mm AM/PM"),
        19 => Some("h:mm:ss AM/PM"),
        20 => Some("h:mm"),
        21 | 32..=35 => Some("h:mm:ss"),
        22 => Some("m/d/yyyy h:mm"),
        37 => Some("#,##0 ;(#,##0)"),
        38 => Some("#,##0 ;[Red](#,##0)"),
        39 => Some("#,##0.00;(#,##0.00)"),
        40 => Some("#,##0.00;[Red](#,##0.00)"),
        41 => Some("_(* #,##0_);_(* (#,##0);_(* \"-\"_);_(@_)"),
        42 => Some("_($* #,##0_);_($* (#,##0);_($* \"-\"_);_(@_)"),
        43 => Some("_(* #,##0.00_);_(* (#,##0.00);_(* \"-\"_);_(@_)"),
        44 => Some("_($* #,##0.00_);_($* (#,##0.00);_($* \"-\"_);_(@_)"),
        45 => Some("mm:ss"),
        46 => Some("[h]:mm:ss"),
        47 => Some("mmss.0"),
        48 => Some("##0.0E+0"),
        49 => Some("@"),
        _ => None,
    }
}

/// Returns `true` if `format_code` conceals cell values (such as `;;;` or `;;`).
#[must_use]
pub fn is_concealed_format(format_code: &str) -> bool {
    let trimmed = format_code.trim();
    if trimmed.is_empty() {
        return false;
    }
    let sections = split_sections(trimmed);
    sections.len() >= 3
        && sections
            .iter()
            .all(|s| strip_brackets_and_escapes(s).trim().is_empty())
}

/// Returns `true` if `format_code` (or `num_fmt_id`) represents a date or time format.
#[must_use]
pub fn is_date_format(num_fmt_id: Option<u32>, format_code: Option<&str>) -> bool {
    if let Some(id) = num_fmt_id
        && matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58)
    {
        return true;
    }
    if let Some(code) = format_code {
        let first = split_sections(code).into_iter().next().unwrap_or(code);
        return section_is_datetime(first);
    }
    false
}

/// Format a [`CellInputValue`] using a built-in `num_fmt_id` and/or custom `format_code`.
#[must_use]
pub fn format_cell_value(
    value: &CellInputValue,
    num_fmt_id: Option<u32>,
    format_code: Option<&str>,
    options: &FormatOptions,
) -> FormattedValue {
    let resolved_code = format_code
        .filter(|s| !s.trim().is_empty())
        .or_else(|| num_fmt_id.and_then(builtin_format_code))
        .unwrap_or("General");
    format_with_code(value, resolved_code, options)
}

/// Format a [`CellInputValue`] with an explicit ECMA-376 format code string.
#[must_use]
pub fn format_with_code(
    value: &CellInputValue,
    format_code: &str,
    options: &FormatOptions,
) -> FormattedValue {
    match value {
        CellInputValue::Empty => FormattedValue::default(),
        CellInputValue::Bool(b) => FormattedValue {
            text: if *b {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            },
            color: None,
            is_concealed: false,
            fill_char: None,
        },
        CellInputValue::Error(err) => FormattedValue {
            text: err.clone(),
            color: None,
            is_concealed: false,
            fill_char: None,
        },
        CellInputValue::Text(s) => format_text_section(s, format_code),
        CellInputValue::Number(num) => {
            if !num.is_finite() {
                return FormattedValue {
                    text: "#NUM!".to_string(),
                    color: None,
                    is_concealed: false,
                    fill_char: None,
                };
            }
            format_number_sections(*num, format_code, options)
        }
    }
}

/// Format a floating-point number using Excel's `General` rule (up to 11 significant digits).
#[must_use]
pub fn format_general_number(num: f64, options: &FormatOptions) -> String {
    if !num.is_finite() {
        return "#NUM!".to_string();
    }
    let clean = if num == 0.0 { 0.0 } else { num };
    let abs = clean.abs();
    if abs == 0.0 {
        return "0".to_string();
    }
    if abs >= 1e11 || abs < 1e-4 {
        // Scientific notation in General: up to 5 decimal places, trimmed
        #[allow(clippy::cast_possible_truncation)]
        let exp = abs.log10().floor() as i32;
        #[allow(clippy::cast_precision_loss)]
        let mantissa = clean / 10_f64.powi(exp);
        let mut mant_str = format!("{mantissa:.5}");
        if mant_str.contains('.') {
            mant_str = mant_str
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string();
        }
        if options.decimal_sep != '.' {
            mant_str = mant_str.replace('.', &options.decimal_sep.to_string());
        }
        let sign = if exp >= 0 { '+' } else { '-' };
        return format!("{mant_str}E{sign}{:02}", exp.unsigned_abs());
    }

    // Integer exact check
    if (clean - clean.round()).abs() < 1e-9 && abs < 1e11 {
        #[allow(clippy::cast_possible_truncation)]
        let int_val = clean.round() as i64;
        return int_val.to_string();
    }

    // Up to 11 total significant digits
    let int_digits = if abs >= 1.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            (abs.log10().floor() as usize) + 1
        }
    } else {
        0
    };
    let max_frac = 10usize.saturating_sub(int_digits).min(9);
    let mut s = format!("{clean:.max_frac$}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if options.decimal_sep != '.' {
        s = s.replace('.', &options.decimal_sep.to_string());
    }
    s
}

fn format_text_section(text: &str, format_code: &str) -> FormattedValue {
    let sections = split_sections(format_code);
    // In Excel, the 4th section formats text; if < 4 sections exist and a single section has `@`, use it.
    let target_sec = if sections.len() >= 4 {
        Some(sections[3])
    } else if sections.len() == 1 && sections[0].contains('@') {
        Some(sections[0])
    } else if sections.len() >= 3 && is_concealed_format(format_code) {
        return FormattedValue {
            text: String::new(),
            color: None,
            is_concealed: true,
            fill_char: None,
        };
    } else {
        None
    };

    let Some(sec) = target_sec else {
        return FormattedValue {
            text: text.to_string(),
            color: None,
            is_concealed: false,
            fill_char: None,
        };
    };

    let meta = parse_section_metadata(sec);
    if meta.cleaned.trim().is_empty() {
        return FormattedValue {
            text: String::new(),
            color: meta.color,
            is_concealed: true,
            fill_char: meta.fill_char,
        };
    }

    let mut out = String::new();
    let mut chars = meta.cleaned.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    out.push(c);
                }
            }
            '\\' => {
                if let Some(c) = chars.next() {
                    out.push(c);
                }
            }
            '_' => {
                if chars.next().is_some() {
                    out.push(' ');
                }
            }
            '*' => {
                let _ = chars.next();
            }
            '@' => out.push_str(text),
            other => out.push(other),
        }
    }

    FormattedValue {
        text: out,
        color: meta.color,
        is_concealed: false,
        fill_char: meta.fill_char,
    }
}

/// Whether `section` has an `@` text placeholder outside quotes and escapes.
fn has_text_placeholder(section: &str) -> bool {
    let mut chars = section.chars();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '\\' if !in_quotes => {
                let _ = chars.next();
            }
            '@' if !in_quotes => return true,
            _ => {}
        }
    }
    false
}

fn format_number_sections(num: f64, format_code: &str, options: &FormatOptions) -> FormattedValue {
    let sections = split_sections(format_code);
    if sections.is_empty() {
        return FormattedValue {
            text: format_general_number(num, options),
            color: None,
            is_concealed: false,
            fill_char: None,
        };
    }

    // Select section by explicit condition `[>=100]` or standard `(pos;neg;zero)` rules
    let (chosen_sec, auto_neg_sign, use_abs_value) = select_numeric_section(&sections, num);
    let meta = parse_section_metadata(chosen_sec);

    if meta.cleaned.trim().is_empty() {
        return FormattedValue {
            text: String::new(),
            color: meta.color,
            is_concealed: true,
            fill_char: meta.fill_char,
        };
    }

    let trimmed_upper = meta.cleaned.trim().to_ascii_uppercase();
    // A text section (`@`, the "Text" number format) shows a number as General does.
    if trimmed_upper == "GENERAL" || has_text_placeholder(&meta.cleaned) {
        let mut t = format_general_number(if use_abs_value { num.abs() } else { num }, options);
        if auto_neg_sign && num < 0.0 && !t.starts_with('-') {
            t.insert(0, '-');
        }
        return FormattedValue {
            text: t,
            color: meta.color,
            is_concealed: false,
            fill_char: meta.fill_char,
        };
    }

    if section_is_datetime(&meta.cleaned) {
        let text = if date_serial_in_range(num, options.date_system) {
            format_datetime_section(num, &meta.cleaned, options.date_system)
        } else {
            // Excel fills the cell with `#` when a date/time format gets a serial it cannot
            // show (negative in the 1900 system, or past 9999-12-31).
            "#".repeat(8)
        };
        return FormattedValue {
            text,
            color: meta.color,
            is_concealed: false,
            fill_char: meta.fill_char,
        };
    }

    let val_to_format = if use_abs_value { num.abs() } else { num };
    let mut text = format_numeric_pattern(val_to_format, &meta.cleaned, options);
    if auto_neg_sign && num < 0.0 && !text.starts_with('-') {
        text.insert(0, '-');
    }

    FormattedValue {
        text,
        color: meta.color,
        is_concealed: false,
        fill_char: meta.fill_char,
    }
}

fn select_numeric_section<'a>(sections: &[&'a str], num: f64) -> (&'a str, bool, bool) {
    // Check if any of the first 3 sections has an explicit condition
    let has_explicit_cond = sections
        .iter()
        .take(3)
        .any(|s| extract_condition(s).is_some());

    if has_explicit_cond {
        for (idx, &sec) in sections.iter().take(3).enumerate() {
            if let Some(cond) = extract_condition(sec) {
                if eval_condition(&cond, num) {
                    return (sec, num < 0.0 && idx == 0, idx > 0);
                }
            } else {
                // Fallback unconditional section
                return (sec, false, idx == 1);
            }
        }
        return (sections[0], num < 0.0, false);
    }

    match sections.len() {
        1 => (sections[0], num < 0.0, true),
        2 => {
            if num < 0.0 {
                (sections[1], false, true)
            } else {
                (sections[0], false, true)
            }
        }
        _ => {
            if num > 0.0 {
                (sections[0], false, true)
            } else if num < 0.0 {
                (sections[1], false, true)
            } else {
                (sections[2], false, true)
            }
        }
    }
}

#[derive(Debug, Clone)]
enum ConditionOp {
    Lt(f64),
    Le(f64),
    Gt(f64),
    Ge(f64),
    Eq(f64),
    Ne(f64),
}

fn extract_condition(section: &str) -> Option<ConditionOp> {
    let mut in_quotes = false;
    let mut chars = section.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let _ = chars.next();
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if !in_quotes && ch == '[' {
            let mut inner = String::new();
            for c in chars.by_ref() {
                if c == ']' {
                    break;
                }
                inner.push(c);
            }
            let t = inner.trim();
            if let Some(rest) = t.strip_prefix(">=") {
                if let Ok(v) = rest.trim().parse::<f64>() {
                    return Some(ConditionOp::Ge(v));
                }
            } else if let Some(rest) = t.strip_prefix("<=") {
                if let Ok(v) = rest.trim().parse::<f64>() {
                    return Some(ConditionOp::Le(v));
                }
            } else if let Some(rest) = t.strip_prefix("<>") {
                if let Ok(v) = rest.trim().parse::<f64>() {
                    return Some(ConditionOp::Ne(v));
                }
            } else if let Some(rest) = t.strip_prefix('>') {
                if let Ok(v) = rest.trim().parse::<f64>() {
                    return Some(ConditionOp::Gt(v));
                }
            } else if let Some(rest) = t.strip_prefix('<') {
                if let Ok(v) = rest.trim().parse::<f64>() {
                    return Some(ConditionOp::Lt(v));
                }
            } else if let Some(rest) = t.strip_prefix('=')
                && let Ok(v) = rest.trim().parse::<f64>()
            {
                return Some(ConditionOp::Eq(v));
            }
        }
    }
    None
}

fn eval_condition(op: &ConditionOp, num: f64) -> bool {
    match *op {
        ConditionOp::Lt(v) => num < v,
        ConditionOp::Le(v) => num <= v,
        ConditionOp::Gt(v) => num > v,
        ConditionOp::Ge(v) => num >= v,
        ConditionOp::Eq(v) => (num - v).abs() <= f64::EPSILON,
        ConditionOp::Ne(v) => (num - v).abs() > f64::EPSILON,
    }
}

struct SectionMetadata {
    cleaned: String,
    color: Option<String>,
    fill_char: Option<char>,
}

fn parse_section_metadata(section: &str) -> SectionMetadata {
    let mut cleaned = String::new();
    let mut color = None;
    let mut fill_char = None;
    let mut chars = section.chars().peekable();
    let mut in_quotes = false;

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            cleaned.push('\\');
            if let Some(next) = chars.next() {
                cleaned.push(next);
            }
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            cleaned.push('"');
            continue;
        }
        if !in_quotes && ch == '*' {
            if let Some(fc) = chars.next() {
                fill_char = Some(fc);
            }
            continue;
        }
        if !in_quotes && ch == '[' {
            let mut bracket = String::new();
            for c in chars.by_ref() {
                if c == ']' {
                    break;
                }
                bracket.push(c);
            }
            let b_trim = bracket.trim();
            let b_lower = b_trim.to_ascii_lowercase();
            // Elapsed time tokens `[h]`, `[m]`, `[s]` stay in `cleaned`
            if matches!(b_lower.as_str(), "h" | "hh" | "m" | "mm" | "s" | "ss") {
                cleaned.push('[');
                cleaned.push_str(&b_lower);
                cleaned.push(']');
            } else if let Some(rest) = b_trim.strip_prefix('$') {
                // Currency/locale tag `[$€-407]` -> emit the currency symbol before `-`
                let sym = rest.split('-').next().unwrap_or("");
                if !sym.is_empty() {
                    cleaned.push('"');
                    cleaned.push_str(sym);
                    cleaned.push('"');
                }
            } else if let Some(hex) = parse_color_token(&b_lower) {
                color = Some(hex.to_string());
            }
            continue;
        }
        cleaned.push(ch);
    }

    SectionMetadata {
        cleaned,
        color,
        fill_char,
    }
}

fn parse_color_token(token_lower: &str) -> Option<&'static str> {
    match token_lower {
        "black" | "color1" => Some("#000000"),
        "white" | "color2" => Some("#FFFFFF"),
        "red" | "color3" => Some("#FF0000"),
        "green" | "color4" | "color10" => Some("#008000"),
        "blue" | "color5" => Some("#0000FF"),
        "yellow" | "color6" => Some("#FFFF00"),
        "magenta" | "color7" => Some("#FF00FF"),
        "cyan" | "color8" => Some("#00FFFF"),
        "color9" => Some("#800000"),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
fn format_numeric_pattern(val: f64, pattern: &str, options: &FormatOptions) -> String {
    // Check for fraction pattern (`# ?/?`, `# ??/16`, `?/?`)
    if is_fraction_pattern(pattern) {
        return format_fraction_pattern(val, pattern);
    }

    // Check for scientific notation (`0.00E+00`, `##0.0E+0`)
    if let Some((mant_pat, exp_pat, exp_plus)) = split_scientific_pattern(pattern) {
        return format_scientific_pattern(val, &mant_pat, &exp_pat, exp_plus, options);
    }

    // Count unquoted `%` and trailing scaling `,`
    let mut percent_count = 0u32;
    let mut chars = pattern.chars().peekable();
    let mut in_quotes = false;
    let mut unquoted_tokens = String::new();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let _ = chars.next();
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if ch == '_' {
            let _ = chars.next();
            continue;
        }
        if !in_quotes {
            if ch == '%' {
                percent_count += 1;
            }
            unquoted_tokens.push(ch);
        }
    }

    let mut scaled = val.abs();
    for _ in 0..percent_count {
        scaled *= 100.0;
    }

    // Analyze digit placeholders in unquoted_tokens
    let (int_placeholders, frac_placeholders, use_thousands, trailing_commas) =
        analyze_digit_placeholders(&unquoted_tokens);

    for _ in 0..trailing_commas {
        scaled /= 1000.0;
    }

    let frac_len = frac_placeholders.len();
    let formatted_fixed = excel_fixed_decimal(scaled, frac_len);
    let (int_digits_raw, frac_digits_raw) = formatted_fixed
        .split_once('.')
        .unwrap_or((&formatted_fixed, ""));

    let formatted_int = format_int_with_placeholders(
        int_digits_raw,
        &int_placeholders,
        use_thousands,
        options.group_sep,
    );
    let formatted_frac = format_frac_with_placeholders(frac_digits_raw, &frac_placeholders);

    // Substitute into the full pattern preserving prefix/suffix literals
    render_with_literals(
        pattern,
        &formatted_int,
        &formatted_frac,
        options.decimal_sep,
    )
}

/// Formats `val` (`>= 0`, finite) with `frac_len` decimals the way Excel does: the value is
/// first reduced to 15 significant digits, then rounded half away from zero in decimal. So
/// `1.005` with `0.00` gives `1.01` (its binary value is 1.00499…), and `1e20` prints as
/// `100000000000000000000` rather than the binary expansion's trailing digits.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn excel_fixed_decimal(val: f64, frac_len: usize) -> String {
    let sci = format!("{val:.14e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    // The value is 0.d1d2d3… × 10^point.
    let mut point = exp + 1;
    let keep = point.saturating_add(i64::try_from(frac_len).unwrap_or(i64::MAX));
    if keep < 0 {
        digits.clear();
    } else if (keep as usize) < digits.len() {
        let keep = keep as usize;
        let round_up = digits[keep] >= 5;
        digits.truncate(keep);
        if round_up {
            let mut i = keep;
            loop {
                if i == 0 {
                    digits.insert(0, 1);
                    point += 1;
                    break;
                }
                i -= 1;
                if digits[i] == 9 {
                    digits[i] = 0;
                } else {
                    digits[i] += 1;
                    break;
                }
            }
        }
    }
    let digit_at = |idx: i64| -> char {
        usize::try_from(idx)
            .ok()
            .and_then(|i| digits.get(i))
            .map_or('0', |&d| char::from(b'0' + d))
    };
    let mut out: String = if point <= 0 {
        "0".to_string()
    } else {
        (0..point).map(digit_at).collect()
    };
    if frac_len > 0 {
        out.push('.');
        out.extend((0..frac_len as i64).map(|j| digit_at(point + j)));
    }
    out
}

fn analyze_digit_placeholders(unquoted: &str) -> (String, String, bool, u32) {
    let (before_dot, after_dot) = unquoted.split_once('.').unwrap_or((unquoted, ""));

    // Count trailing commas at the end of the last digit group
    let active_part = if after_dot.is_empty() {
        before_dot
    } else {
        after_dot
    };
    let mut trailing_commas = 0u32;
    let mut seen_digit = false;
    let mut comma_between_digits = false;

    let mut last_digit_idx = None;
    for (idx, ch) in active_part.char_indices() {
        if matches!(ch, '0' | '#' | '?') {
            last_digit_idx = Some(idx);
        }
    }
    if let Some(last_idx) = last_digit_idx {
        for ch in active_part[last_idx + 1..].chars() {
            if ch == ',' {
                trailing_commas += 1;
            } else {
                break;
            }
        }
    }

    for (idx, ch) in before_dot.char_indices() {
        if matches!(ch, '0' | '#' | '?') {
            seen_digit = true;
        } else if ch == ',' && seen_digit {
            let has_digit_after = before_dot[idx + 1..]
                .chars()
                .any(|c| matches!(c, '0' | '#' | '?'));
            if has_digit_after {
                comma_between_digits = true;
            }
        }
    }

    let int_ph: String = before_dot
        .chars()
        .filter(|c| matches!(c, '0' | '#' | '?'))
        .collect();
    let frac_ph: String = after_dot
        .chars()
        .filter(|c| matches!(c, '0' | '#' | '?'))
        .collect();

    (int_ph, frac_ph, comma_between_digits, trailing_commas)
}

fn format_int_with_placeholders(
    raw_digits: &str,
    placeholders: &str,
    use_thousands: bool,
    group_sep: char,
) -> String {
    let digits = if raw_digits == "0" { "" } else { raw_digits };
    let ph_len = placeholders.len();
    let dig_len = digits.len();

    let mut core = String::new();
    let mut space_prefix = String::new();

    if dig_len > ph_len {
        core.push_str(digits);
    } else {
        let pad_len = ph_len - dig_len;
        for ch in placeholders[..pad_len].chars() {
            match ch {
                '0' => core.push('0'),
                '?' => space_prefix.push(' '),
                _ => {}
            }
        }
        core.push_str(digits);
    }

    if use_thousands && core.len() > 3 {
        let mut grouped = String::new();
        let len = core.len();
        for (i, ch) in core.chars().enumerate() {
            if i > 0 && (len - i).is_multiple_of(3) {
                grouped.push(group_sep);
            }
            grouped.push(ch);
        }
        core = grouped;
    }

    format!("{space_prefix}{core}")
}

fn format_frac_with_placeholders(raw_frac: &str, placeholders: &str) -> String {
    let mut out = String::new();
    let raw_bytes = raw_frac.as_bytes();
    for (i, ph) in placeholders.chars().enumerate() {
        let d = raw_bytes.get(i).copied().unwrap_or(b'0') as char;
        let remaining_all_zero = raw_bytes
            .get(i..)
            .is_none_or(|rem| rem.iter().all(|&b| b == b'0'));
        match ph {
            '0' => out.push(d),
            '#' => {
                if !remaining_all_zero {
                    out.push(d);
                }
            }
            '?' => {
                if remaining_all_zero {
                    out.push(' ');
                } else {
                    out.push(d);
                }
            }
            _ => {}
        }
    }
    out
}

fn render_with_literals(
    pattern: &str,
    formatted_int: &str,
    formatted_frac: &str,
    decimal_sep: char,
) -> String {
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    let mut emitted_int = false;
    let mut in_frac = false;

    let has_any_digit_ph = {
        let mut c_iter = pattern.chars().peekable();
        let mut found = false;
        let mut q = false;
        while let Some(c) = c_iter.next() {
            if c == '\\' {
                let _ = c_iter.next();
                continue;
            }
            if c == '"' {
                q = !q;
                continue;
            }
            if !q && matches!(c, '0' | '#' | '?') {
                found = true;
                break;
            }
        }
        found
    };

    if !has_any_digit_ph {
        // Pure literal format
        while let Some(ch) = chars.next() {
            match ch {
                '"' => {
                    for c in chars.by_ref() {
                        if c == '"' {
                            break;
                        }
                        out.push(c);
                    }
                }
                '\\' => {
                    if let Some(c) = chars.next() {
                        out.push(c);
                    }
                }
                '_' => {
                    if chars.next().is_some() {
                        out.push(' ');
                    }
                }
                other => out.push(other),
            }
        }
        return out;
    }

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    out.push(c);
                }
            }
            '\\' => {
                if let Some(c) = chars.next() {
                    out.push(c);
                }
            }
            '_' => {
                if chars.next().is_some() {
                    out.push(' ');
                }
            }
            '0' | '#' | '?' => {
                if !in_frac && !emitted_int {
                    out.push_str(formatted_int);
                    emitted_int = true;
                }
            }
            ',' => {
                // Skip thousands / scaling commas inside number body
            }
            '.' if !in_frac => {
                if !emitted_int {
                    out.push_str(formatted_int);
                    emitted_int = true;
                }
                in_frac = true;
                // Excel keeps the point even when every `#` decimal is blank (`0.##` → `2.`).
                out.push(decimal_sep);
                out.push_str(formatted_frac);
            }
            other => out.push(other),
        }
    }

    out
}

fn is_fraction_pattern(pattern: &str) -> bool {
    let mut in_quotes = false;
    let mut seen_digit_ph = false;
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let _ = chars.next();
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if !in_quotes {
            if matches!(ch, '0' | '#' | '?') {
                seen_digit_ph = true;
            } else if ch == '/' && seen_digit_ph {
                return true;
            }
        }
    }
    false
}

fn format_fraction_pattern(val: f64, pattern: &str) -> String {
    let abs = val.abs();
    let has_whole_part = pattern
        .split_once('/')
        .is_some_and(|(left, _)| left.trim().contains(' '));

    let (_, denom_part) = pattern.split_once('/').unwrap_or(("", "?"));
    let denom_clean: String = denom_part
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '?' | '#' | '0'))
        .collect();

    let fixed_denom = denom_clean.parse::<u64>().ok();
    let max_denom = if let Some(d) = fixed_denom {
        d.max(1)
    } else {
        10_u64.pow(u32::try_from(denom_clean.len().clamp(1, 5)).unwrap_or(1)) - 1
    };

    let (whole, frac) = if has_whole_part {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            (abs.trunc() as u64, abs.fract())
        }
    } else {
        (0, abs)
    };

    let (num, den) = if let Some(d) = fixed_denom {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let n = (frac * (d as f64)).round() as u64;
        (n, d)
    } else {
        best_rational_approx(frac, max_denom)
    };

    if has_whole_part {
        let mut total_whole = whole;
        let mut rem_num = num;
        if rem_num >= den && den > 0 {
            total_whole += rem_num / den;
            rem_num %= den;
        }
        if rem_num == 0 {
            let pad = " ".repeat(denom_clean.len() * 2 + 2);
            return format!("{total_whole}{pad}");
        }
        if total_whole == 0 {
            return format!("{rem_num}/{den}");
        }
        format!("{total_whole} {rem_num}/{den}")
    } else {
        let total_num = whole * den + num;
        format!("{total_num}/{den}")
    }
}

fn best_rational_approx(x: f64, max_denom: u64) -> (u64, u64) {
    if x <= 1e-9 {
        return (0, 1);
    }
    let mut p0 = 0u64;
    let mut q0 = 1u64;
    let mut p1 = 1u64;
    let mut q1 = 0u64;
    let mut rem = x;

    for _ in 0..16 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let a = rem.floor() as u64;
        let p2 = a.saturating_mul(p1).saturating_add(p0);
        let q2 = a.saturating_mul(q1).saturating_add(q0);
        if q2 > max_denom {
            let k = (max_denom.saturating_sub(q0)) / q1.max(1);
            let pk = k.saturating_mul(p1).saturating_add(p0);
            let qk = k.saturating_mul(q1).saturating_add(q0);
            #[allow(clippy::cast_precision_loss)]
            let prev_err = (x - (p1 as f64) / (q1.max(1) as f64)).abs();
            #[allow(clippy::cast_precision_loss)]
            let cand_err = (x - (pk as f64) / (qk.max(1) as f64)).abs();
            return if cand_err < prev_err && qk > 0 {
                (pk, qk)
            } else {
                (p1, q1.max(1))
            };
        }
        p0 = p1;
        q0 = q1;
        p1 = p2;
        q1 = q2;
        #[allow(clippy::cast_precision_loss)]
        let diff = rem - (a as f64);
        if diff.abs() < 1e-9 {
            break;
        }
        rem = 1.0 / diff;
    }
    (p1, q1.max(1))
}

fn split_scientific_pattern(pattern: &str) -> Option<(String, String, bool)> {
    let bytes = pattern.as_bytes();
    let mut in_quotes = false;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            in_quotes = !in_quotes;
            i += 1;
            continue;
        }
        if !in_quotes
            && (bytes[i] == b'E' || bytes[i] == b'e')
            && i + 1 < bytes.len()
            && (bytes[i + 1] == b'+' || bytes[i + 1] == b'-')
        {
            let exp_plus = bytes[i + 1] == b'+';
            let mant = pattern[..i].to_string();
            let exp = pattern[i + 2..].to_string();
            return Some((mant, exp, exp_plus));
        }
        i += 1;
    }
    None
}

fn format_scientific_pattern(
    val: f64,
    mant_pat: &str,
    exp_pat: &str,
    exp_plus: bool,
    options: &FormatOptions,
) -> String {
    let abs = val.abs();
    let (int_ph, frac_ph, _, _) = analyze_digit_placeholders(mant_pat);
    let exp_digits = exp_pat
        .chars()
        .filter(|c| matches!(c, '0' | '#' | '?'))
        .count()
        .max(1);

    if abs == 0.0 {
        let mant = format_numeric_pattern(0.0, mant_pat, options);
        let sign = if exp_plus { "+" } else { "" };
        return format!("{mant}E{sign}{:0exp_digits$}", 0);
    }

    let group_step = int_ph.len().max(1);
    #[allow(clippy::cast_possible_truncation)]
    let raw_exp = abs.log10().floor() as i32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let step_i32 = group_step as i32;
    let adj_exp = if step_i32 > 1 {
        raw_exp.div_euclid(step_i32) * step_i32
    } else {
        raw_exp
    };
    let mant_val = abs / 10_f64.powi(adj_exp);
    let frac_len = frac_ph.len();
    let mut mant_str = format!("{mant_val:.frac_len$}");
    if options.decimal_sep != '.' {
        mant_str = mant_str.replace('.', &options.decimal_sep.to_string());
    }
    let sign = if adj_exp < 0 {
        "-"
    } else if exp_plus {
        "+"
    } else {
        ""
    };
    let exp_abs = adj_exp.unsigned_abs();
    format!("{mant_str}E{sign}{exp_abs:0exp_digits$}")
}

fn section_is_datetime(cleaned: &str) -> bool {
    let mut in_quotes = false;
    let mut chars = cleaned.chars().peekable();
    let mut unquoted = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let _ = chars.next();
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if ch == '_' {
            let _ = chars.next();
            continue;
        }
        if !in_quotes {
            unquoted.push(ch.to_ascii_lowercase());
        }
    }
    // If digit placeholders `0`, `#`, `?` appear outside `.0` fractional seconds, it's numeric
    if unquoted.contains('#') || unquoted.contains('?') {
        return false;
    }
    unquoted.contains('y')
        || unquoted.contains('d')
        || unquoted.contains('h')
        || unquoted.contains('s')
        || unquoted.contains("mm")
        || unquoted.contains("[m]")
        || unquoted.contains("am/pm")
        || unquoted.contains("a/p")
}

#[derive(Debug, Clone, Copy)]
struct CivilDateTime {
    year: i32,
    month: u32,
    day: u32,
    day_of_week: u32, // 0 = Sunday .. 6 = Saturday
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
}

/// Serial of 9999-12-31 in the 1900 system, the last date Excel can display.
const MAX_DATE_SERIAL_1900: f64 = 2_958_465.0;

/// Whether `serial` is a date/time Excel can display in `system`. Values outside the range
/// are shown as `#` fill; checking first also keeps the integer conversions below in range.
fn date_serial_in_range(serial: f64, system: DateSystem) -> bool {
    match system {
        DateSystem::Date1900 => (0.0..MAX_DATE_SERIAL_1900 + 1.0).contains(&serial),
        // Negative 1904 serials keep their existing clamp-to-1904-01-01 behaviour.
        DateSystem::Date1904 => serial.is_finite() && serial + 1462.0 < MAX_DATE_SERIAL_1900 + 1.0,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn serial_to_civil(serial: f64, system: DateSystem) -> CivilDateTime {
    let adjusted_serial = match system {
        DateSystem::Date1900 => serial.max(0.0),
        DateSystem::Date1904 => (serial + 1462.0).max(0.0),
    };

    // Total milliseconds in the fractional day (round to nearest millisecond to avoid FP drift)
    let total_day_ms = ((adjusted_serial.fract() * 86_400_000.0).round() as u64).min(86_400_000);
    let extra_day = u32::from(total_day_ms == 86_400_000);
    let day_ms = total_day_ms % 86_400_000;

    let hour = (day_ms / 3_600_000) as u32;
    let minute = ((day_ms % 3_600_000) / 60_000) as u32;
    let second = ((day_ms % 60_000) / 1_000) as u32;
    let millis = (day_ms % 1_000) as u32;

    let whole_days = (adjusted_serial.floor() as i64) + i64::from(extra_day);

    // Day of week (0 = Sunday): Excel counts serial 1 (1900-01-01) as a Sunday, so the
    // weekday is `(serial - 1) mod 7`. The fictitious 1900-02-29 keeps later dates correct.
    let dow = (whole_days + 6).rem_euclid(7) as u32;

    if system == DateSystem::Date1900 && whole_days == 60 {
        // Lotus 1-2-3 leap-year bug: 1900-02-29
        return CivilDateTime {
            year: 1900,
            month: 2,
            day: 29,
            day_of_week: 3,
            hour,
            minute,
            second,
            millis,
        };
    }

    if system == DateSystem::Date1900 && whole_days == 0 {
        return CivilDateTime {
            year: 1900,
            month: 1,
            day: 0,
            day_of_week: 6,
            hour,
            minute,
            second,
            millis,
        };
    }

    // Convert Excel serial day to proleptic Gregorian days since 1970-01-01
    // Serial 25569 = 1970-01-01 (because serial >= 61 includes the fake 1900-02-29 day)
    let corrected_days = if system == DateSystem::Date1900 && whole_days < 60 {
        whole_days + 1
    } else {
        whole_days
    };
    let unix_days = corrected_days - 25_569;
    let (year, month, day) = civil_from_unix_days(unix_days);

    CivilDateTime {
        year,
        month,
        day,
        day_of_week: dow,
        hour,
        minute,
        second,
        millis,
    }
}

/// Howard Hinnant's `civil_from_days` algorithm (exact across all Gregorian dates).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn civil_from_unix_days(z_in: i64) -> (i32, u32, u32) {
    let z = z_in + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + i64::from(m <= 2)) as i32;
    (year, m, d)
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn format_datetime_section(serial: f64, pattern: &str, system: DateSystem) -> String {
    let lower_pat = pattern.to_ascii_lowercase();
    let has_ampm = lower_pat.contains("am/pm") || lower_pat.contains("a/p");

    // Excel rounds a time to the precision it displays (whole seconds, or `ss.0`..`ss.000`)
    // before splitting it into fields, so 23:59:59.6 shown as `hh:mm:ss` is 00:00:00.
    let frac_sec_digits = lower_pat.find("s.0").map_or(0, |p| {
        lower_pat[p + 2..]
            .bytes()
            .take_while(|&b| b == b'0')
            .count()
            .min(3)
    });
    let serial = if lower_pat.contains('h') || lower_pat.contains('s') || has_ampm {
        let unit = 86_400.0 * 10_f64.powi(i32::try_from(frac_sec_digits).unwrap_or(0));
        (serial * unit).round() / unit
    } else {
        serial
    };
    let dt = serial_to_civil(serial, system);

    let total_seconds = (serial.abs() * 86_400.0).round() as u64;
    let elapsed_hours = total_seconds / 3600;
    let elapsed_minutes = total_seconds / 60;

    let mut out = String::new();
    let bytes = pattern.as_bytes();
    let mut i = 0usize;
    let mut last_was_hour = false;

    while i < bytes.len() {
        match bytes[i] {
            // Literals are copied as whole UTF-8 characters (e.g. `yyyy"年"m"月"d"日"`).
            b'"' => {
                let start = i + 1;
                let end = pattern[start..]
                    .find('"')
                    .map_or(pattern.len(), |p| start + p);
                out.push_str(&pattern[start..end]);
                i = (end + 1).min(pattern.len());
            }
            b'\\' => {
                i += 1;
                if let Some(ch) = pattern[i..].chars().next() {
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            b'_' => {
                i += 1;
                if let Some(ch) = pattern[i..].chars().next() {
                    out.push(' ');
                    i += ch.len_utf8();
                }
            }
            b'[' => {
                if let Some(end) = pattern[i + 1..].find(']') {
                    let inner = pattern[i + 1..i + 1 + end].to_ascii_lowercase();
                    match inner.as_str() {
                        "h" => {
                            let _ = write!(out, "{elapsed_hours}");
                            last_was_hour = true;
                        }
                        "hh" => {
                            let _ = write!(out, "{elapsed_hours:02}");
                            last_was_hour = true;
                        }
                        "m" => {
                            let _ = write!(out, "{elapsed_minutes}");
                            last_was_hour = false;
                        }
                        "mm" => {
                            let _ = write!(out, "{elapsed_minutes:02}");
                            last_was_hour = false;
                        }
                        "s" => {
                            let _ = write!(out, "{total_seconds}");
                            last_was_hour = false;
                        }
                        "ss" => {
                            let _ = write!(out, "{total_seconds:02}");
                            last_was_hour = false;
                        }
                        _ => {}
                    }
                    i += end + 2;
                } else {
                    i += 1;
                }
            }
            b'y' | b'Y' => {
                let len = count_run_ci(bytes, i, b'y');
                i += len;
                last_was_hour = false;
                if len >= 3 {
                    let _ = write!(out, "{:04}", dt.year);
                } else {
                    let _ = write!(out, "{:02}", dt.year.rem_euclid(100));
                }
            }
            b'd' | b'D' => {
                let len = count_run_ci(bytes, i, b'd');
                i += len;
                last_was_hour = false;
                match len {
                    1 => {
                        let _ = write!(out, "{}", dt.day);
                    }
                    2 => {
                        let _ = write!(out, "{:02}", dt.day);
                    }
                    3 => out.push_str(short_weekday_name(dt.day_of_week)),
                    _ => out.push_str(full_weekday_name(dt.day_of_week)),
                }
            }
            b'h' | b'H' => {
                let len = count_run_ci(bytes, i, b'h');
                i += len;
                last_was_hour = true;
                let h = if has_ampm {
                    match dt.hour % 12 {
                        0 => 12,
                        other => other,
                    }
                } else {
                    dt.hour
                };
                if len >= 2 {
                    let _ = write!(out, "{h:02}");
                } else {
                    let _ = write!(out, "{h}");
                }
            }
            b's' | b'S' => {
                let len = count_run_ci(bytes, i, b's');
                i += len;
                last_was_hour = false;
                if len >= 2 {
                    let _ = write!(out, "{:02}", dt.second);
                } else {
                    let _ = write!(out, "{}", dt.second);
                }
            }
            b'm' | b'M' => {
                let len = count_run_ci(bytes, i, b'm');
                let next_is_second = next_datetime_token_is_second(bytes, i + len);
                let is_minute = (len <= 2) && (last_was_hour || next_is_second);
                i += len;
                last_was_hour = false;
                if is_minute {
                    if len == 2 {
                        let _ = write!(out, "{:02}", dt.minute);
                    } else {
                        let _ = write!(out, "{}", dt.minute);
                    }
                } else {
                    match len {
                        1 => {
                            let _ = write!(out, "{}", dt.month);
                        }
                        2 => {
                            let _ = write!(out, "{:02}", dt.month);
                        }
                        3 => out.push_str(short_month_name(dt.month)),
                        4 => out.push_str(full_month_name(dt.month)),
                        _ => out.push(short_month_name(dt.month).chars().next().unwrap_or('J')),
                    }
                }
            }
            b'.' if i + 1 < bytes.len() && bytes[i + 1] == b'0' => {
                i += 1;
                let zero_len = count_run_ci(bytes, i, b'0').min(3);
                i += zero_len;
                out.push('.');
                // `serial` was already rounded to this precision above.
                match zero_len {
                    1 => {
                        let _ = write!(out, "{}", dt.millis / 100);
                    }
                    2 => {
                        let _ = write!(out, "{:02}", dt.millis / 10);
                    }
                    _ => {
                        let _ = write!(out, "{:03}", dt.millis);
                    }
                }
            }
            b'a' | b'A' => {
                let rem_lower = pattern[i..].to_ascii_lowercase();
                if rem_lower.starts_with("am/pm") {
                    let is_upper = bytes[i] == b'A';
                    if dt.hour < 12 {
                        out.push_str(if is_upper { "AM" } else { "am" });
                    } else {
                        out.push_str(if is_upper { "PM" } else { "pm" });
                    }
                    i += 5;
                } else if rem_lower.starts_with("a/p") {
                    let is_upper = bytes[i] == b'A';
                    if dt.hour < 12 {
                        out.push(if is_upper { 'A' } else { 'a' });
                    } else {
                        out.push(if is_upper { 'P' } else { 'p' });
                    }
                    i += 3;
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            _ => {
                let ch = pattern[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    out
}

fn count_run_ci(bytes: &[u8], start: usize, target_lower: u8) -> usize {
    let mut len = 0usize;
    while start + len < bytes.len() && bytes[start + len].to_ascii_lowercase() == target_lower {
        len += 1;
    }
    len
}

fn next_datetime_token_is_second(bytes: &[u8], mut idx: usize) -> bool {
    while idx < bytes.len() {
        let b = bytes[idx].to_ascii_lowercase();
        if b == b's' {
            return true;
        }
        if matches!(b, b'y' | b'd' | b'h' | b'm') {
            return false;
        }
        idx += 1;
    }
    false
}

fn short_month_name(month: u32) -> &'static str {
    match month {
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "Jan",
    }
}

fn full_month_name(month: u32) -> &'static str {
    match month {
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "January",
    }
}

fn short_weekday_name(dow: u32) -> &'static str {
    match dow % 7 {
        0 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        _ => "Sat",
    }
}

fn full_weekday_name(dow: u32) -> &'static str {
    match dow % 7 {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        _ => "Saturday",
    }
}

fn split_sections(format_code: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    let mut in_brackets = false;
    let bytes = format_code.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            in_quotes = !in_quotes;
        } else if !in_quotes && bytes[i] == b'[' {
            in_brackets = true;
        } else if !in_quotes && bytes[i] == b']' {
            in_brackets = false;
        } else if !in_quotes && !in_brackets && bytes[i] == b';' {
            sections.push(&format_code[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    sections.push(&format_code[start..]);
    sections
}

fn strip_brackets_and_escapes(section: &str) -> String {
    let meta = parse_section_metadata(section);
    meta.cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_builtin_numeric_and_currency_codes() {
        let opts = FormatOptions::default();
        assert_eq!(
            format_cell_value(&CellInputValue::Number(1234.567), Some(0), None, &opts).text,
            "1234.567"
        );
        assert_eq!(
            format_cell_value(&CellInputValue::Number(1234.567), Some(1), None, &opts).text,
            "1235"
        );
        assert_eq!(
            format_cell_value(&CellInputValue::Number(1234.567), Some(2), None, &opts).text,
            "1234.57"
        );
        assert_eq!(
            format_cell_value(&CellInputValue::Number(1_234_567.89), Some(4), None, &opts).text,
            "1,234,567.89"
        );
        let neg_curr = format_cell_value(&CellInputValue::Number(-1250.5), Some(8), None, &opts);
        assert_eq!(neg_curr.text, "($1,250.50)");
        assert_eq!(neg_curr.color.as_deref(), Some("#FF0000"));
    }

    #[test]
    fn formats_percentages_scientific_and_fractions() {
        let opts = FormatOptions::default();
        assert_eq!(
            format_cell_value(&CellInputValue::Number(0.1234), Some(10), None, &opts).text,
            "12.34%"
        );
        assert_eq!(
            format_cell_value(&CellInputValue::Number(12345.0), Some(11), None, &opts).text,
            "1.23E+04"
        );
        assert_eq!(
            format_cell_value(&CellInputValue::Number(3.25), Some(12), None, &opts).text,
            "3 1/4"
        );
        assert_eq!(
            format_with_code(&CellInputValue::Number(2_500_000.0), "#,##0,,\"M\"", &opts).text,
            "3M"
        );
    }

    #[test]
    fn formats_1900_leap_year_bug_and_1904_dates_and_elapsed_time() {
        let opts_1900 = FormatOptions::default();
        // Serial 60 in 1900 system is the Lotus 1-2-3 fake leap day 1900-02-29
        assert_eq!(
            format_with_code(&CellInputValue::Number(60.0), "yyyy-mm-dd", &opts_1900).text,
            "1900-02-29"
        );
        // Serial 1 is 1900-01-01
        assert_eq!(
            format_with_code(&CellInputValue::Number(1.0), "yyyy-mm-dd", &opts_1900).text,
            "1900-01-01"
        );
        // Serial 45292 is 2024-01-01
        assert_eq!(
            format_with_code(
                &CellInputValue::Number(45292.5),
                "yyyy-mm-dd hh:mm",
                &opts_1900
            )
            .text,
            "2024-01-01 12:00"
        );
        // Elapsed time `[h]:mm:ss` for 1.5 days = 36:00:00
        assert_eq!(
            format_cell_value(&CellInputValue::Number(1.5), Some(46), None, &opts_1900).text,
            "36:00:00"
        );

        let opts_1904 = FormatOptions {
            date_system: DateSystem::Date1904,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_with_code(&CellInputValue::Number(0.0), "yyyy-mm-dd", &opts_1904).text,
            "1904-01-01"
        );
    }

    #[test]
    fn detects_concealed_format_and_evaluates_conditions() {
        let opts = FormatOptions::default();
        assert!(is_concealed_format(";;;"));
        let res = format_with_code(&CellInputValue::Number(42.0), ";;;", &opts);
        assert!(res.is_concealed);
        assert_eq!(res.text, "");

        let cond_fmt = "[>=100][Green]\"High \"0;[<0][Red]\"Neg \"0;\"Low \"0";
        let hi = format_with_code(&CellInputValue::Number(150.0), cond_fmt, &opts);
        assert_eq!(hi.text, "High 150");
        assert_eq!(hi.color.as_deref(), Some("#008000"));
    }

    fn fmt(v: f64, code: &str) -> String {
        format_with_code(&CellInputValue::Number(v), code, &FormatOptions::default()).text
    }

    #[test]
    fn weekdays_follow_excel_serials() {
        // 2023-03-15 was a Wednesday; Excel calls 1900-01-01 a Sunday.
        assert_eq!(
            fmt(45000.0, "dddd, mmmm d, yyyy"),
            "Wednesday, March 15, 2023"
        );
        assert_eq!(fmt(45000.0, "ddd"), "Wed");
        assert_eq!(fmt(1.0, "dddd"), "Sunday");
        assert_eq!(fmt(61.0, "dddd"), "Thursday");
    }

    #[test]
    fn rounds_on_fifteen_significant_digits() {
        assert_eq!(fmt(1.005, "0.00"), "1.01");
        assert_eq!(fmt(9.995, "0.00"), "10.00");
        assert_eq!(fmt(0.005, "0.00"), "0.01");
        assert_eq!(fmt(0.0049, "0.00"), "0.00");
        assert_eq!(fmt(1e20, "0"), "100000000000000000000");
        assert_eq!(fmt(123_456_789_012_345_678.0, "0"), "123456789012346000");
        assert_eq!(fmt(-2.5, "0"), "-3");
    }

    #[test]
    fn keeps_the_decimal_point_of_optional_decimals() {
        assert_eq!(fmt(2.0, "0.##"), "2.");
        assert_eq!(fmt(1.5, "0.##"), "1.5");
        assert_eq!(fmt(0.5, "#.##"), ".5");
    }

    #[test]
    fn times_round_to_the_displayed_precision() {
        assert_eq!(fmt(0.999_999_9, "hh:mm:ss"), "00:00:00");
        assert_eq!(fmt(0.5 + 0.6 / 86_400.0, "hh:mm:ss"), "12:00:01");
        assert_eq!(fmt(1.95 / 86_400.0, "ss.0"), "02.0");
        assert_eq!(fmt(1.5 / 86_400.0, "mm:ss.0"), "00:01.5");
    }

    #[test]
    fn numbers_in_text_format_show_as_general() {
        assert_eq!(fmt(101.0, "@"), "101");
        assert_eq!(fmt(0.5, "@"), "0.5");
        assert_eq!(fmt(5.0, "\"@\"0"), "@5");
    }

    #[test]
    fn date_literals_keep_non_ascii_characters() {
        assert_eq!(fmt(45000.0, "yyyy\"年\"m\"月\"d\"日\""), "2023年3月15日");
        assert_eq!(fmt(45000.0, r"d\€m"), "15€3");
    }

    #[test]
    fn dates_outside_excel_range_show_hash_fill() {
        // Fuzz crash: a ~1e34 serial overflowed the day arithmetic.
        let huge = f64::from_le_bytes([114, 0, 1, 1, 0, 0, 0, 71]);
        assert_eq!(fmt(huge, "dGd"), "########");
        assert_eq!(fmt(f64::MAX, "[h]:mm"), "########");
        assert_eq!(fmt(-1.0, "yyyy-mm-dd"), "########");
        assert_eq!(fmt(2_958_466.0, "yyyy-mm-dd"), "########");
        assert_eq!(fmt(2_958_465.0, "yyyy-mm-dd"), "9999-12-31");
    }
}
