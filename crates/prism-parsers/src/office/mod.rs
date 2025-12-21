//! Office format parsers
//!
//! Parsers for Microsoft Office Open XML formats (DOCX, XLSX, PPTX)
//! and legacy Office binary formats.

pub mod docx;
pub mod legacy;
pub mod pptx;
pub mod xlsx;
pub mod utils;

// Re-export parsers
pub use docx::DocxParser;
pub use legacy::{DocParser, PptParser, XlsParser};
pub use pptx::PptxParser;
pub use xlsx::XlsxParser;
