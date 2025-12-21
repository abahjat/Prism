//! Image format parsers

pub mod jpeg;
pub mod png;

pub use jpeg::JpegParser;
pub use png::PngParser;
