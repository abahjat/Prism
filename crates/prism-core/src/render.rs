// SPDX-License-Identifier: AGPL-3.0-only
//! # Renderer Traits
//!
//! Core traits for rendering documents from the UDM to various output formats.

use async_trait::async_trait;
use bytes::Bytes;

use crate::document::Document;
use crate::error::Result;
use crate::format::Format;

/// Options for rendering documents
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Target format
    pub format: Option<Format>,

    /// Whether to include images
    pub include_images: bool,

    /// Whether to preserve formatting
    pub preserve_formatting: bool,

    /// Page range to render (None = all pages)
    pub page_range: Option<PageRange>,

    /// DPI for raster output formats
    pub dpi: Option<u32>,

    /// Quality for lossy formats (0-100)
    pub quality: Option<u8>,
}

/// A range of pages to render
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageRange {
    /// All pages
    All,

    /// Specific pages (1-indexed)
    Pages(Vec<u32>),

    /// Page range (inclusive, 1-indexed)
    Range { start: u32, end: u32 },
}

/// Context for rendering operations
#[derive(Debug, Clone)]
pub struct RenderContext {
    /// Render options
    pub options: RenderOptions,

    /// Target filename (optional hint)
    pub filename: Option<String>,
}

/// Trait for document renderers
///
/// Implement this trait to add support for rendering to a new output format.
#[async_trait]
pub trait Renderer: Send + Sync {
    /// Get the output format this renderer produces
    fn output_format(&self) -> Format;

    /// Render a document to bytes
    ///
    /// # Arguments
    ///
    /// * `document` - The document to render (in UDM format)
    /// * `context` - Render context with options
    ///
    /// # Returns
    ///
    /// The rendered document as bytes
    async fn render(&self, document: &Document, context: RenderContext) -> Result<Bytes>;

    /// Get renderer metadata
    fn metadata(&self) -> RendererMetadata {
        RendererMetadata::default()
    }
}

/// Metadata about a renderer
#[derive(Debug, Clone, Default)]
pub struct RendererMetadata {
    /// Renderer name
    pub name: String,

    /// Renderer version
    pub version: String,

    /// Supported features
    pub features: Vec<RenderFeature>,
}

/// Features that a renderer may support
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFeature {
    /// Can render text
    TextRendering,

    /// Can render images
    ImageRendering,

    /// Can render tables
    TableRendering,

    /// Can render vector graphics
    VectorRendering,

    /// Supports page ranges
    PageRangeSupport,

    /// Supports streaming output
    StreamingSupport,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_options_default() {
        let opts = RenderOptions::default();
        assert!(!opts.include_images);
        assert!(!opts.preserve_formatting);
    }

    #[test]
    fn test_page_range() {
        let range = PageRange::Range { start: 1, end: 10 };
        assert_eq!(range, PageRange::Range { start: 1, end: 10 });
    }
}
