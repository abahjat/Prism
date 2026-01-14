// SPDX-License-Identifier: AGPL-3.0-only
//! Image format parsers

/// Adobe Illustrator parser
pub mod ai;
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
/// JPEG 2000 image parser
pub mod jpeg2000;
/// PCX (Paintbrush) image parser
pub mod pcx;
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
/// WBMP (Wireless Bitmap) image parser
pub mod wbmp;
/// WebP image parser
pub mod webp;

pub use ai::AiParser;
pub use bmp::BmpParser;
pub use gif::GifParser;
pub use ico::IcoParser;
pub use jpeg::JpegParser;
pub use jpeg2000::Jpeg2000Parser;
pub use pcx::PcxParser;
pub use png::PngParser;
pub use psd::PsdParser;
pub use svg::{SvgParser, SvgzParser};
pub use tga::TgaParser;
pub use tiff::TiffParser;
pub use vector::{EmfParser, EmzParser, EpsParser, WmfParser};
pub use wbmp::WbmpParser;
pub use webp::WebpParser;
