// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Core
//!
//! Core document model and traits for the Prism document processing SDK.
//!
//! This crate provides:
//! - The Unified Document Model (UDM) that all formats parse into
//! - Core traits for parsers, renderers, and processors
//! - Cross-format hidden-content reporting (`review`) and container family trees (`tree`)
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
pub mod hash;
pub mod license;
pub mod metadata;
pub mod parser;
pub mod render;
pub mod review;
pub mod source;
pub mod text;
pub mod tree;

// Re-exports for convenience
pub use document::{
    ContentBlock, Document, EmbeddedBlock, EmbeddedKind, ImageBlock, Page, TableBlock, TextBlock,
};
pub use error::{Error, Result};
pub use format::{Format, FormatFamily, FormatSignature, detect_format};
pub use metadata::Metadata;
pub use parser::{ParseContext, ParseOptions, Parser};
pub use review::{
    Finding, FindingCategory, FindingKind, FormatCoverage, HiddenContentReport, Location,
    ReviewRect, Severity, SourceRef,
};
pub use source::{DEFAULT_DETECTION_PREFIX_BYTES, DocumentSource, SeekableReader};
pub use tree::{ContainerLimits, ContainerNode, ContainerNodeStatus, FamilyTree};

/// Prism SDK version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Git commit hash
pub const GIT_HASH: &str = env!("GIT_HASH");
/// Full version string
pub const FULL_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("GIT_HASH"));
/// Layout algorithm version number (bumped whenever layout output bytes may change).
pub const LAYOUT_ALGORITHM_VERSION: u32 = 1;
/// Combined deterministic engine version string used for annotation sidecars, caches, and manifests.
pub const ENGINE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-layout.1");

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
/// init();
/// ```
pub fn init() {
    tracing::debug!("Initializing Prism v{}", VERSION);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert_ne!(VERSION, "");
        assert!(ENGINE_VERSION.contains("-layout."));
    }

    #[test]
    fn test_init() {
        init(); // Should not panic
    }
}
