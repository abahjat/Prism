// SPDX-License-Identifier: AGPL-3.0-only
//! Image format parsers

/// BMP image parser
pub mod bmp;
/// External image converter (ImageMagick integration)
pub mod converter;
/// GIF image parser
pub mod gif;
/// JPEG image parser
pub mod jpeg;
/// PNG image parser
pub mod png;
/// SVG image parser
pub mod svg;
/// TIFF image parser
pub mod tiff;
/// Vector image parsers (EMF, EMZ, WMF, EPS)
pub mod vector;
/// WebP image parser
pub mod webp;

pub use bmp::BmpParser;
pub use gif::GifParser;
pub use jpeg::JpegParser;
pub use png::PngParser;
pub use svg::SvgParser;
pub use tiff::TiffParser;
pub use vector::{EmfParser, EmzParser, EpsParser, WmfParser};
pub use webp::WebpParser;
