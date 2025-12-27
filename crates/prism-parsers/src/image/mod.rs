// SPDX-License-Identifier: AGPL-3.0-only
//! Image format parsers

/// JPEG image parser
pub mod jpeg;
/// PNG image parser
pub mod png;
/// TIFF image parser
pub mod tiff;

pub use jpeg::JpegParser;
pub use png::PngParser;
pub use tiff::TiffParser;
