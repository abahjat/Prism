//! # Prism Parsers
//!
//! Format parser implementations for the Prism document processing SDK.
//!
//! This crate contains parsers for various document formats. Each parser
//! implements the `Parser` trait from `prism-core` and converts format-specific
//! documents into the Unified Document Model (UDM).
//!
//! ## Supported Format Families
//!
//! - **Office**: DOCX, XLSX, PPTX, DOC, XLS, PPT (planned)
//! - **PDF**: PDF 1.x-2.0, PDF/A (planned)
//! - **Email**: MSG, EML, PST (planned)
//! - **Images**: JPEG, PNG, TIFF, GIF, BMP (planned)
//! - **Archives**: ZIP, RAR, 7z, TAR (planned)
//! - **CAD**: DWG, DXF (planned)
//!
//! ## Usage
//!
//! ```rust,no_run
//! use prism_parsers::registry::ParserRegistry;
//! use prism_core::format::detect_format;
//!
//! # async fn example() -> prism_core::Result<()> {
//! let registry = ParserRegistry::new();
//! let data = std::fs::read("document.pdf")?;
//! let data_len = data.len();
//!
//! // Detect format
//! let format_result = detect_format(&data, Some("document.pdf"))
//!     .ok_or_else(|| prism_core::Error::DetectionFailed("Unknown format".to_string()))?;
//!
//! // Get appropriate parser
//! let parser = registry.get_parser(&format_result.format)
//!     .ok_or_else(|| prism_core::Error::UnsupportedFormat(format_result.format.name.clone()))?;
//!
//! // Parse document
//! let document = parser.parse(
//!     bytes::Bytes::from(data),
//!     prism_core::parser::ParseContext {
//!         format: format_result.format,
//!         filename: Some("document.pdf".to_string()),
//!         size: data_len,
//!         options: Default::default(),
//!     }
//! ).await?;
//!
//! println!("Parsed document with {} pages", document.page_count());
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod registry;

// Individual parser modules (to be implemented)
// pub mod office;
// pub mod pdf;
// pub mod image;
// pub mod email;
// pub mod archive;
// pub mod cad;

/// Prism parsers version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
