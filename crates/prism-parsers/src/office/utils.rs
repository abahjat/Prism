// SPDX-License-Identifier: AGPL-3.0-only
//! Shared utilities for Office XML parsers (`ODF`, `VSDX`, `XPS`)

use quick_xml::escape::{resolve_xml_entity, unescape};
use quick_xml::events::{BytesRef, BytesStart};

/// Parse an Excel cell reference (e.g., "A1", "BC123") into (row, col) indices (0-based).
#[must_use]
pub fn parse_cell_ref(cell_ref: &str) -> (usize, usize) {
    let mut col_str = String::new();
    let mut row_str = String::new();

    for c in cell_ref.chars() {
        if c.is_ascii_alphabetic() {
            col_str.push(c);
        } else if c.is_ascii_digit() {
            row_str.push(c);
        }
    }

    let col = excel_column_to_index(&col_str);
    let row = row_str.parse::<usize>().unwrap_or(1).saturating_sub(1);

    (row, col)
}

/// Convert Excel column letters (A, B, ..., Z, AA, AB, ...) to 0-based index.
#[must_use]
pub fn excel_column_to_index(col: &str) -> usize {
    let mut result = 0usize;
    for c in col.chars() {
        if c.is_ascii_alphabetic() {
            let val = (c.to_ascii_uppercase() as usize) - ('A' as usize) + 1;
            result = result * 26 + val;
        }
    }
    result.saturating_sub(1)
}

/// Convert 0-based index to Excel column letters (0 -> A, 25 -> Z, 26 -> AA).
#[must_use]
pub fn index_to_excel_column(mut index: usize) -> String {
    let mut result = String::new();
    loop {
        let remainder = index % 26;
        result.insert(0, (b'A' + u8::try_from(remainder).unwrap_or(0)) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    result
}

/// Unescape an attribute value string from `quick-xml`.
#[must_use]
pub fn attr_value(value: &str) -> String {
    unescape(value).map_or_else(|_| value.to_string(), std::borrow::Cow::into_owned)
}

/// Extract an optional attribute value from a `BytesStart` event by attribute name.
#[must_use]
pub fn attr_value_opt(event: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    for attr in event.attributes().flatten() {
        if attr.key.as_ref().as_bytes() == key {
            return Some(attr_value(&attr.value));
        }
    }
    None
}

/// Resolve an XML entity/character reference from a `BytesRef` event.
#[must_use]
pub fn resolve_general_ref(event: &BytesRef<'_>) -> Option<String> {
    if let Some(ch) = event.resolve_char_ref().ok().flatten() {
        return Some(ch.to_string());
    }

    resolve_xml_entity(event.as_ref()).map(str::to_string)
}

/// Parse an Office Open XML boolean/on-off attribute value.
#[must_use]
pub fn on_off_value(val: &str) -> bool {
    !matches!(val, "0" | "false" | "off")
}
