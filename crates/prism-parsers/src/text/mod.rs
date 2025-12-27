// SPDX-License-Identifier: AGPL-3.0-only
//! Plain text format parsers
//!
//! Parsers for plain text files (.txt, .log, .json, .xml, .csv, .md, .html, etc.)

pub mod csv;
pub mod html;
pub mod plain;

// Re-export parsers
pub use csv::CsvParser;
pub use html::HtmlParser;
pub use plain::{JsonParser, LogParser, MarkdownParser, TextParser, XmlParser};
