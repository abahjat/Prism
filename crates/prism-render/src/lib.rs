//! # Prism Render
//!
//! Rendering engine for converting Prism documents (UDM) to various output formats.
//!
//! This crate provides renderers that take documents in the Unified Document Model
//! and produce output in formats like HTML, PDF, images, and SVG.
//!
//! ## Supported Output Formats
//!
//! - **HTML5**: Responsive, accessible HTML with CSS
//! - **PDF**: PDF output (planned)
//! - **PNG/JPEG**: Raster image output (planned)
//! - **SVG**: Vector graphics output (planned)
//! - **Text**: Plain text output (planned)
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
// pub mod pdf;
// pub mod image;
// pub mod svg;
// pub mod text;

/// Prism render version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
