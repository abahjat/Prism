// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Core
//!
//! Core types and traits for the Prism document conversion library.
//! # Prism Core
//!
//! Core document model and traits for the Prism document processing SDK.
//!
//! This crate provides:
//! - The Unified Document Model (UDM) that all formats parse into
//! - Core traits for parsers, renderers, and processors
//! - Error types and result handling
//! - Format detection utilities
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
//! │   Input     │────▶│   Parser    │────▶│  Document   │
//! │  (bytes)    │     │  (format)   │     │   (UDM)     │
//! └─────────────┘     └─────────────┘     └──────┬──────┘
//!                                                │
//!                     ┌─────────────┐            │
//!                     │  Renderer   │◀───────────┘
//!                     │  (output)   │
//!                     └─────────────┘
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod document;
pub mod error;
pub mod format;
pub mod license;
pub mod metadata;
pub mod parser;
pub mod render;

// Re-exports for convenience
pub use document::{ContentBlock, Document, ImageBlock, Page, TableBlock, TextBlock};
pub use error::{Error, Result};
pub use format::{detect_format, Format, FormatFamily, FormatSignature};
pub use metadata::Metadata;
pub use parser::{ParseContext, ParseOptions, Parser};

/// Prism SDK version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Initialize the Prism library
///
/// Call this once at application startup to initialize logging,
/// register built-in parsers, and prepare the runtime.
///
/// # Example
///
/// ```rust
/// use prism_core::init;
///
/// ///
///     init();
///     // ... use Prism
/// ```
pub fn init() {
    tracing::debug!("Initializing Prism v{}", VERSION);
    // Future: Initialize parser registry, load configs, etc.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn test_init() {
        init(); // Should not panic
    }
}
