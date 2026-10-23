// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Render
//!
//! Rendering engine for converting Prism documents (UDM and positioned `LayoutDocument` pages)
//! to output formats including HTML5, SVG, and PNG/RGBA raster images.
//!
//! ## Supported Output Formats
//!
//! - **HTML5**: Responsive, accessible HTML with CSS (`html`)
//! - **SVG**: Deterministic vector graphics with `<defs>` glyph outlines and selectable text layer (`svg`)
//! - **PNG/RGBA**: Pure-Rust CPU rasterization via `tiny-skia` (`raster`)
//!
//! ## Usage
//!
//! ```rust,no_run
//! use prism_render::html::HtmlRenderer;
//! use prism_core::render::{Renderer, RenderContext, RenderOptions};
//! use prism_core::format::Format;
//!
//! # async fn example(document: prism_core::Document) -> prism_core::Result<()> {
//! let renderer = HtmlRenderer::new();
//!
//! let context = RenderContext {
//!     options: RenderOptions {
//!         format: Some(Format::pdf()), // Target format hint
//!         include_images: true,
//!         preserve_formatting: true,
//!         ..Default::default()
//!     },
//!     filename: Some("output.html".to_string()),
//! };
//!
//! let html_bytes = renderer.render(&document, context).await?;
//! std::fs::write("output.html", html_bytes)?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod html;
pub mod pdf;
pub mod raster;
pub mod sanitize;
pub mod svg;

/// Prism render version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
