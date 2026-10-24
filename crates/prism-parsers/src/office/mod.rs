// SPDX-License-Identifier: AGPL-3.0-only
//! Office format parsers
//!
//! Parsers for Microsoft Office Open XML formats (DOCX, XLSX, PPTX),
//! legacy Office binary formats (DOC, XLS, PPT, MPP), OpenDocument formats, OneNote, and Visio.

pub mod docx;
pub mod legacy;
pub mod odf;
pub mod onenote;
pub mod pptx;
pub mod utils;
pub mod vsdx;
pub mod xlsx;
pub mod xps;

// Re-export parsers
pub use docx::DocxParser;
pub use legacy::{DocParser, MppParser, PptParser, XlsParser};
pub use odf::{OdgParser, OdpParser, OdsParser, OdtParser};
pub use onenote::OneNoteParser;
pub use pptx::PptxParser;
pub use vsdx::VsdxParser;
pub use xlsx::XlsxParser;
pub use xps::XpsParser;
