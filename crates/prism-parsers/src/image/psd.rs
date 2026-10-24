// SPDX-License-Identifier: AGPL-3.0-only
use async_trait::async_trait;
use bytes::Bytes;
use image::ImageFormat;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, ImageBlock, ImageResource, Page, PageMetadata, Rect,
        ShapeStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::io::Cursor;

/// Most pixels a PSD composite may have before it is refused (the format allows 30000 x 30000).
const MAX_PSD_PIXELS: u64 = 64_000_000;

/// Read a big-endian `u16` / `u32` at `pos`, or fail if the file ends first.
fn be_u16(data: &[u8], pos: usize) -> Result<u16> {
    data.get(pos..pos + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| Error::ParseError("Truncated PSD file".to_string()))
}

fn be_u32(data: &[u8], pos: usize) -> Result<u32> {
    data.get(pos..pos + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| Error::ParseError("Truncated PSD file".to_string()))
}

/// Decode one PackBits-compressed row into exactly `out.len()` bytes.
fn unpack_bits_row(src: &[u8], out: &mut [u8]) -> Result<()> {
    let bad = || Error::ParseError("Malformed PSD RLE row".to_string());
    let (mut i, mut o) = (0usize, 0usize);
    while o < out.len() {
        let n = *src.get(i).ok_or_else(bad)?;
        i += 1;
        match n.cmp(&128) {
            std::cmp::Ordering::Less => {
                let len = usize::from(n) + 1;
                let lit = src.get(i..i + len).ok_or_else(bad)?;
                out.get_mut(o..o + len)
                    .ok_or_else(bad)?
                    .copy_from_slice(lit);
                i += len;
                o += len;
            }
            std::cmp::Ordering::Greater => {
                let len = 257 - usize::from(n);
                let b = *src.get(i).ok_or_else(bad)?;
                out.get_mut(o..o + len).ok_or_else(bad)?.fill(b);
                i += 1;
                o += len;
            }
            // 128 is a no-op.
            std::cmp::Ordering::Equal => {}
        }
    }
    Ok(())
}

/// Decode the flattened composite in a PSD's image data section to RGBA.
///
/// Supports what the viewer needs and nothing more: version 1 files, 8 bits per channel,
/// grayscale or RGB (an extra channel is alpha), raw or RLE. Every length is checked against
/// the file, and the pixel count is capped before anything is allocated.
#[allow(clippy::many_single_char_names)]
fn decode_psd_composite(data: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    if data.len() < 26 || &data[0..4] != b"8BPS" {
        return Err(Error::ParseError("Invalid PSD signature".to_string()));
    }
    match be_u16(data, 4)? {
        1 => {}
        2 => {
            return Err(Error::UnsupportedFormat(
                "PSB (large document) files are not supported".to_string(),
            ));
        }
        other => return Err(Error::ParseError(format!("Unknown PSD version {other}"))),
    }
    let channels = usize::from(be_u16(data, 12)?);
    let height = be_u32(data, 14)?;
    let width = be_u32(data, 18)?;
    let depth = be_u16(data, 22)?;
    let mode = be_u16(data, 24)?;
    let color_channels = match mode {
        1 => 1, // Grayscale
        3 => 3, // RGB
        other => {
            return Err(Error::UnsupportedFormat(format!(
                "PSD color mode {other} is not supported (grayscale and RGB only)"
            )));
        }
    };
    if depth != 8 {
        return Err(Error::UnsupportedFormat(format!(
            "PSD with {depth} bits per channel is not supported (8 only)"
        )));
    }
    if width == 0 || height == 0 || channels < color_channels {
        return Err(Error::ParseError(
            "PSD header has no pixels or too few channels".to_string(),
        ));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PSD_PIXELS {
        return Err(Error::MemoryLimitExceeded {
            used: usize::try_from(pixels).unwrap_or(usize::MAX),
            limit: usize::try_from(MAX_PSD_PIXELS).unwrap_or(usize::MAX),
        });
    }
    let (w, h) = (width as usize, height as usize);
    let plane_len = w * h;

    // Skip the color mode data, image resources and layer/mask sections.
    let mut pos = 26usize;
    for _ in 0..3 {
        let len = be_u32(data, pos)? as usize;
        pos = pos
            .checked_add(4 + len)
            .filter(|&p| p <= data.len())
            .ok_or_else(|| Error::ParseError("PSD section runs past the file".to_string()))?;
    }
    let compression = be_u16(data, pos)?;
    pos += 2;

    // Only the color planes and one alpha plane are read; they come first.
    let wanted = channels.min(color_channels + 1);
    let mut planes = vec![vec![0u8; plane_len]; wanted];
    match compression {
        0 => {
            for (c, plane) in planes.iter_mut().enumerate() {
                let start = pos + c * plane_len;
                let src = data.get(start..start + plane_len).ok_or_else(|| {
                    Error::ParseError("PSD image data runs past the file".to_string())
                })?;
                plane.copy_from_slice(src);
            }
        }
        1 => {
            // A byte count per row of every channel, then the rows in channel order.
            let rows = channels * h;
            let counts_end = pos + rows * 2;
            let mut row_pos = counts_end;
            for row in 0..wanted * h {
                let count = usize::from(be_u16(data, pos + row * 2)?);
                let src = data.get(row_pos..row_pos + count).ok_or_else(|| {
                    Error::ParseError("PSD RLE data runs past the file".to_string())
                })?;
                let (c, y) = (row / h, row % h);
                unpack_bits_row(src, &mut planes[c][y * w..(y + 1) * w])?;
                row_pos += count;
            }
        }
        other => {
            return Err(Error::UnsupportedFormat(format!(
                "PSD image compression {other} is not supported (raw and RLE only)"
            )));
        }
    }

    let mut rgba = Vec::with_capacity(plane_len * 4);
    for i in 0..plane_len {
        let (r, g, b) = if color_channels == 1 {
            (planes[0][i], planes[0][i], planes[0][i])
        } else {
            (planes[0][i], planes[1][i], planes[2][i])
        };
        let a = planes.get(color_channels).map_or(255, |p| p[i]);
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    Ok((width, height, rgba))
}

/// PSD image parser
#[derive(Debug, Clone)]
pub struct PsdParser;

impl PsdParser {
    /// Create a new PSD parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for PsdParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PsdParser {
    fn format(&self) -> Format {
        Format::psd()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        // PSD magic: 8BPS
        if data.len() < 4 {
            return false;
        }
        data[0] == 0x38 && data[1] == 0x42 && data[2] == 0x50 && data[3] == 0x53
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let (width, height, rgba) = decode_psd_composite(&data)?;

        // Convert to PNG for web display
        let image = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            Error::ParseError("Failed to create RGBA image from PSD data".to_string())
        })?;

        let dynamic_image = image::DynamicImage::ImageRgba8(image);
        let mut png_data = Vec::new();
        dynamic_image
            .write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)
            .map_err(|e| Error::ParseError(format!("Failed to encode PSD as PNG: {e}")))?;

        let resource_id = "psd_preview".to_string();

        let image_resource = ImageResource {
            id: resource_id.clone(),
            mime_type: "image/png".to_string(),
            data: Some(png_data),
            url: None,
            width,
            height,
        };

        #[allow(clippy::cast_precision_loss)]
        let image_block = ImageBlock {
            bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            resource_id: resource_id.clone(),
            alt_text: Some("PSD Preview".to_string()),
            format: Some("image/vnd.adobe.photoshop".to_string()),
            original_size: Some(Dimensions::new(f64::from(width), f64::from(height))),
            style: ShapeStyle::default(),
            rotation: 0.0,
        };

        let page = Page {
            number: 1,
            dimensions: Dimensions {
                width: f64::from(width),
                height: f64::from(height),
            },
            content: vec![ContentBlock::Image(image_block)],
            metadata: PageMetadata::default(),
            annotations: Vec::new(),
        };

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", "PSD");

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;
        document.resources.images = vec![image_resource];

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PSD Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::ImageExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 RGB PSD with an alpha channel and empty optional sections.
    fn psd_bytes(compression: u16, planes: &[[u8; 4]]) -> Vec<u8> {
        let mut d = b"8BPS".to_vec();
        d.extend_from_slice(&1u16.to_be_bytes());
        d.extend_from_slice(&[0; 6]);
        #[allow(clippy::cast_possible_truncation)]
        d.extend_from_slice(&(planes.len() as u16).to_be_bytes());
        d.extend_from_slice(&2u32.to_be_bytes()); // height
        d.extend_from_slice(&2u32.to_be_bytes()); // width
        d.extend_from_slice(&8u16.to_be_bytes());
        d.extend_from_slice(&3u16.to_be_bytes()); // RGB
        d.extend_from_slice(&[0; 12]); // three empty sections
        d.extend_from_slice(&compression.to_be_bytes());
        if compression == 0 {
            for p in planes {
                d.extend_from_slice(p);
            }
        } else {
            // Each 2-byte row as one literal run: header 1, then the two bytes.
            for _ in 0..planes.len() * 2 {
                d.extend_from_slice(&3u16.to_be_bytes());
            }
            for p in planes {
                for row in p.chunks(2) {
                    d.push(1);
                    d.extend_from_slice(row);
                }
            }
        }
        d
    }

    #[test]
    fn raw_and_rle_composites_decode_to_the_same_pixels() {
        let planes = [
            [10, 20, 30, 40],
            [50, 60, 70, 80],
            [90, 100, 110, 120],
            [255, 0, 255, 0],
        ];
        let raw = decode_psd_composite(&psd_bytes(0, &planes)).unwrap();
        let rle = decode_psd_composite(&psd_bytes(1, &planes)).unwrap();
        assert_eq!(raw, rle);
        assert_eq!((raw.0, raw.1), (2, 2));
        assert_eq!(&raw.2[..8], &[10, 50, 90, 255, 20, 60, 100, 0]);
    }

    #[test]
    fn section_lengths_past_the_end_are_errors() {
        // `parse_any` fuzz crash: a section length far beyond the file panicked inside the
        // `psd` crate.
        let mut d = psd_bytes(0, &[[0; 4], [0; 4], [0; 4]]);
        d[26..30].copy_from_slice(&0x6E66_6900u32.to_be_bytes());
        assert!(decode_psd_composite(&d).is_err());
        // A huge declared size is refused before allocating.
        let mut big = psd_bytes(0, &[[0; 4], [0; 4], [0; 4]]);
        big[14..18].copy_from_slice(&30_000u32.to_be_bytes());
        big[18..22].copy_from_slice(&30_000u32.to_be_bytes());
        assert!(matches!(
            decode_psd_composite(&big),
            Err(Error::MemoryLimitExceeded { .. })
        ));
        // Truncated RLE rows are errors too.
        let mut rle = psd_bytes(1, &[[1; 4], [2; 4], [3; 4]]);
        rle.truncate(rle.len() - 2);
        assert!(decode_psd_composite(&rle).is_err());
    }
}
