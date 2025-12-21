//! Utility functions for Office format parsing

use prism_core::error::{Error, Result};

/// Parse an Excel cell reference (e.g., "A1", "B5", "AA10") into (row, col) indices
///
/// Returns (row, column) as zero-based indices
/// Example: "B5" -> (4, 1)
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
pub fn index_to_excel_column(mut index: usize) -> String {
    let mut col = String::new();
    index += 1; // Convert to 1-based

    while index > 0 {
        let remainder = (index - 1) % 26;
        col.insert(0, (b'A' + remainder as u8) as char);
        index = (index - 1) / 26;
    }

    col
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
