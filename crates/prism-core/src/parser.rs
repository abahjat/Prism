// SPDX-License-Identifier: AGPL-3.0-only
//! # Parser Traits
//!
//! Core traits for implementing document parsers.

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::error::Result;
use crate::format::Format;
use crate::tree::{
    ContainerLimits, DEFAULT_MAX_CONTAINER_BYTES, DEFAULT_MAX_CONTAINER_DEPTH,
    DEFAULT_MAX_CONTAINER_MEMBERS,
};

/// Options for parsing documents
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseOptions {
    /// Whether to extract images
    pub extract_images: bool,

    /// Whether to preserve formatting
    pub preserve_formatting: bool,

    /// Whether to extract structure (headings, TOC)
    pub extract_structure: bool,

    /// Maximum memory to use (in bytes)
    pub max_memory: Option<usize>,

    /// Timeout for parsing (in seconds)
    pub timeout: Option<u64>,

    /// Password for encrypted documents
    pub password: Option<String>,

    /// Maximum container recursion depth (defaults to [`DEFAULT_MAX_CONTAINER_DEPTH`]).
    #[serde(default)]
    pub max_container_depth: Option<u32>,

    /// Maximum total expanded bytes across container members (defaults to [`DEFAULT_MAX_CONTAINER_BYTES`]).
    #[serde(default)]
    pub max_container_bytes: Option<u64>,

    /// Maximum total member count across a container family tree (defaults to [`DEFAULT_MAX_CONTAINER_MEMBERS`]).
    #[serde(default)]
    pub max_container_members: Option<usize>,
}

impl ParseOptions {
    /// Return the effective [`ContainerLimits`] configured on these [`ParseOptions`].
    #[must_use]
    pub fn container_limits(&self) -> ContainerLimits {
        ContainerLimits {
            max_depth: self
                .max_container_depth
                .unwrap_or(DEFAULT_MAX_CONTAINER_DEPTH),
            max_total_bytes: self
                .max_container_bytes
                .unwrap_or(DEFAULT_MAX_CONTAINER_BYTES),
            max_members: self
                .max_container_members
                .unwrap_or(DEFAULT_MAX_CONTAINER_MEMBERS),
        }
    }
}

/// Context provided to parsers during parsing
#[derive(Debug, Clone)]
pub struct ParseContext {
    /// The detected format
    pub format: Format,

    /// Source filename (if available)
    pub filename: Option<String>,

    /// File size
    pub size: usize,

    /// Parse options
    pub options: ParseOptions,
}

/// Trait for document parsers
///
/// Implement this trait to add support for a new document format.
// async_trait wraps the #[must_use] future in a Box, which this lint flags
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Parser: Send + Sync {
    /// Get the format this parser handles
    fn format(&self) -> Format;

    /// Check if this parser can handle the given data
    ///
    /// This is called after format detection to verify the parser
    /// can actually process the document.
    fn can_parse(&self, data: &[u8]) -> bool;

    /// Parse a document from bytes
    ///
    /// # Arguments
    ///
    /// * `data` - The document content
    /// * `context` - Parse context with format, options, etc.
    ///
    /// # Returns
    ///
    /// A parsed Document in the UDM format
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document>;

    /// Get parser metadata (name, version, supported features)
    fn metadata(&self) -> ParserMetadata {
        ParserMetadata::default()
    }
}

/// Metadata about a parser
#[derive(Debug, Clone, Default)]
pub struct ParserMetadata {
    /// Parser name
    pub name: String,

    /// Parser version
    pub version: String,

    /// Supported features
    pub features: Vec<ParserFeature>,

    /// Whether the parser requires sandboxing
    pub requires_sandbox: bool,
}

/// Features that a parser may support
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserFeature {
    /// Can extract text
    TextExtraction,

    /// Can extract images
    ImageExtraction,

    /// Can extract tables
    TableExtraction,

    /// Can extract document structure
    StructureExtraction,

    /// Can extract metadata
    MetadataExtraction,

    /// Can handle encrypted documents
    EncryptionSupport,

    /// Supports streaming parsing
    StreamingSupport,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_options_default() {
        let opts = ParseOptions::default();
        assert!(!opts.extract_images);
        assert!(!opts.preserve_formatting);
        let limits = opts.container_limits();
        assert_eq!(limits.max_depth, DEFAULT_MAX_CONTAINER_DEPTH);
        assert_eq!(limits.max_total_bytes, DEFAULT_MAX_CONTAINER_BYTES);
        assert_eq!(limits.max_members, DEFAULT_MAX_CONTAINER_MEMBERS);
    }

    #[test]
    fn test_parse_context() {
        let context = ParseContext {
            format: Format::pdf(),
            filename: Some("test.pdf".to_string()),
            size: 1024,
            options: ParseOptions::default(),
        };

        assert_eq!(context.size, 1024);
        assert_eq!(context.filename, Some("test.pdf".to_string()));
    }
}
