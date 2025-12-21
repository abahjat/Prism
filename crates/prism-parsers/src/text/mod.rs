//! Plain text format parsers
//!
//! Parsers for plain text files (.txt, .log, .json, .xml, .csv, .md, etc.)

pub mod plain;

// Re-export parsers
pub use plain::{
    CsvParser, JsonParser, LogParser, MarkdownParser, TextParser, XmlParser,
};
