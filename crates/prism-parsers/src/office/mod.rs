// SPDX-License-Identifier: AGPL-3.0-only
//! Office format parsers
//!
//! Parsers for Microsoft Office Open XML formats (DOCX, XLSX, PPTX)
//! and legacy Office binary formats.

pub mod docx;
pub mod excel_styles;
pub mod legacy;
pub mod pptx;
pub mod relationships;
pub mod shapes;
pub mod slides;
pub mod styles;
pub mod tables;
pub mod theme;
pub mod utils;
pub mod xlsx;

// Re-export parsers
pub use docx::DocxParser;
pub use legacy::{DocParser, PptParser, XlsParser};
pub use pptx::PptxParser;
pub use theme::*;
pub use xlsx::XlsxParser;
