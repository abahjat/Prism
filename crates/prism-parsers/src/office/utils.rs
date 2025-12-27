// SPDX-License-Identifier: AGPL-3.0-only
//! Utility functions for Office format parsing

use crate::office::theme::Theme;
use prism_core::error::{Error, Result};

/// Parse an Excel cell reference (e.g., "A1", "B5", "AA10") into (row, col) indices
///
/// Returns (row, column) as zero-based indices
/// Example: "B5" -> (4, 1)
///
/// # Errors
/// Returns an error if the cell reference is invalid (e.g., empty, missing row/col).
pub fn parse_cell_ref(ref_str: &str) -> Result<(usize, usize)> {
    if ref_str.is_empty() {
        return Err(Error::ParseError("Empty cell reference".to_string()));
    }

    // Split into column letters and row numbers
    let col_end = ref_str
        .chars()
        .position(|c| c.is_ascii_digit())
        .ok_or_else(|| Error::ParseError(format!("Invalid cell reference: {}", ref_str)))?;

    let col_str = &ref_str[..col_end];
    let row_str = &ref_str[col_end..];

    let col = excel_column_to_index(col_str)?;
    let row = row_str
        .parse::<usize>()
        .map_err(|_| Error::ParseError(format!("Invalid row number: {}", row_str)))?
        .saturating_sub(1); // Excel rows are 1-based

    Ok((row, col))
}

/// Convert Excel column letter(s) to zero-based index
///
/// Examples:
/// - "A" -> 0
/// - "Z" -> 25
/// - "AA" -> 26
/// - "AB" -> 27
///
/// # Errors
/// Returns an error if the column string contains non-uppercase-alpha characters.
pub fn excel_column_to_index(col: &str) -> Result<usize> {
    if col.is_empty() {
        return Err(Error::ParseError("Empty column reference".to_string()));
    }

    let mut index = 0;
    for c in col.chars() {
        if !c.is_ascii_uppercase() {
            return Err(Error::ParseError(format!(
                "Invalid column character: {}",
                c
            )));
        }
        index = index * 26 + (c as usize - 'A' as usize + 1);
    }

    Ok(index - 1) // Convert to zero-based
}

/// Convert zero-based column index to Excel column letter(s)
///
/// Examples:
/// - 0 -> "A"
/// - 25 -> "Z"
/// - 26 -> "AA"
#[must_use]
pub fn index_to_excel_column(mut index: usize) -> String {
    let mut col = String::new();
    index += 1; // Convert to 1-based

    while index > 0 {
        let remainder = (index - 1) % 26;
        // remainder is guaranteed to be < 26, so it fits in u8.
        #[allow(clippy::cast_possible_truncation)]
        col.insert(0, (b'A' + remainder as u8) as char);
        index = (index - 1) / 26;
    }

    col
}

/// Helper to get attribute value as string
#[must_use]
pub fn attr_value(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

/// Helper to get optional attribute value from event
#[must_use]
pub fn attr_value_opt(event: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<String> {
    for attr in event.attributes().flatten() {
        if attr.key.as_ref() == key {
            return Some(attr_value(&attr.value));
        }
    }
    None
}

/// Resolve color from Word/Excel attributes, handling Theme references.
///
/// Priorities:
/// 1. `val` if it is a hex code (not "auto").
/// 2. `theme_color` resolved against `theme`.
///
/// Note: `tint` and `shade` are effectively ignored for now (TODO: Implement HSL adjustments).
#[must_use]
pub fn resolve_word_color(
    val: Option<&str>,
    theme_color: Option<&str>,
    tint: Option<f64>,
    _shade: Option<f64>,
    theme: &Theme,
) -> Option<String> {
    // 1. Direct hex value?
    if let Some(v) = val {
        if v != "auto" && !v.is_empty() {
            // Usually "FF0000" or "AABBCC"
            return Some(format!("#{}", v));
        }
    }

    // 2. Theme color?
    if let Some(tc) = theme_color {
        if let Some(hex) = theme.resolve_color(tc) {
            if let Some(t) = tint {
                return Some(apply_tint(&hex, t));
            }
            return Some(format!("#{}", hex));
        }
    }

    None
}

/// Apply Excel tint/shade to a hex color
fn apply_tint(hex: &str, tint: f64) -> String {
    if tint == 0.0 {
        return format!("#{}", hex);
    }

    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);

    let apply = |c: u8| -> u8 {
        let val = c as f64;
        if tint > 0.0 {
            // Lighten: value * (1 - tint) + (255 * tint)
            (val * (1.0 - tint) + (255.0 * tint)).round() as u8
        } else {
            // Darken: value * (1 + tint)
            (val * (1.0 + tint)).round() as u8
        }
    };

    format!("#{:02X}{:02X}{:02X}", apply(r), apply(g), apply(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cell_ref() {
        assert_eq!(parse_cell_ref("A1").unwrap(), (0, 0));
        assert_eq!(parse_cell_ref("B5").unwrap(), (4, 1));
        assert_eq!(parse_cell_ref("Z99").unwrap(), (98, 25));
        assert_eq!(parse_cell_ref("AA10").unwrap(), (9, 26));
    }

    #[test]
    fn test_parse_cell_ref_invalid() {
        assert!(parse_cell_ref("").is_err());
        assert!(parse_cell_ref("123").is_err());
        assert!(parse_cell_ref("ABC").is_err());
    }

    #[test]
    fn test_excel_column_to_index() {
        assert_eq!(excel_column_to_index("A").unwrap(), 0);
        assert_eq!(excel_column_to_index("B").unwrap(), 1);
        assert_eq!(excel_column_to_index("Z").unwrap(), 25);
        assert_eq!(excel_column_to_index("AA").unwrap(), 26);
        assert_eq!(excel_column_to_index("AB").unwrap(), 27);
        assert_eq!(excel_column_to_index("AZ").unwrap(), 51);
        assert_eq!(excel_column_to_index("BA").unwrap(), 52);
    }

    #[test]
    fn test_excel_column_to_index_invalid() {
        assert!(excel_column_to_index("").is_err());
        assert!(excel_column_to_index("1").is_err());
        assert!(excel_column_to_index("a").is_err());
    }

    #[test]
    fn test_index_to_excel_column() {
        assert_eq!(index_to_excel_column(0), "A");
        assert_eq!(index_to_excel_column(1), "B");
        assert_eq!(index_to_excel_column(25), "Z");
        assert_eq!(index_to_excel_column(26), "AA");
        assert_eq!(index_to_excel_column(27), "AB");
        assert_eq!(index_to_excel_column(51), "AZ");
        assert_eq!(index_to_excel_column(52), "BA");
    }

    #[test]
    fn test_round_trip() {
        for i in 0..1000 {
            let col = index_to_excel_column(i);
            let index = excel_column_to_index(&col).unwrap();
            assert_eq!(index, i);
        }
    }
}
