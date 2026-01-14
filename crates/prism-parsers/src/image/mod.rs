// SPDX-License-Identifier: AGPL-3.0-only
//! Image format parsers

/// BMP image parser
pub mod bmp;
/// External image converter (ImageMagick integration)
pub mod converter;
/// GIF image parser
pub mod gif;
/// ICO (Windows Icon) image parser
pub mod ico;
/// JPEG image parser
pub mod jpeg;
/// PNG image parser
pub mod png;
/// PSD image parser
pub mod psd;
/// SVG and SVGZ image parsers
pub mod svg;
/// TGA (Truevision) image parser
pub mod tga;
/// TIFF image parser
pub mod tiff;
/// Vector image parsers (EMF, EMZ, WMF, EPS)
pub mod vector;
/// WebP image parser
pub mod webp;

pub use bmp::BmpParser;
pub use gif::GifParser;
pub use ico::IcoParser;
pub use jpeg::JpegParser;
pub use png::PngParser;
pub use psd::PsdParser;
pub use svg::{SvgParser, SvgzParser};
pub use tga::TgaParser;
pub use tiff::TiffParser;
pub use vector::{EmfParser, EmzParser, EpsParser, WmfParser};
pub use webp::WebpParser;
