// SPDX-License-Identifier: AGPL-3.0-only
//! Image format parsers

pub mod jpeg;
pub mod png;
pub mod tiff;

pub use jpeg::JpegParser;
pub use png::PngParser;
pub use tiff::TiffParser;
