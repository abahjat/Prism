//! Plain text format parsers
//!
//! Parsers for plain text files (.txt, .log, .json, .xml, .csv, .md, .html, etc.)

pub mod html;
pub mod plain;

// Re-export parsers
pub use html::HtmlParser;
pub use plain::{
    CsvParser, JsonParser, LogParser, MarkdownParser, TextParser, XmlParser,
};
