// SPDX-License-Identifier: AGPL-3.0-only
pub mod gzip;
pub mod tar;
pub mod zip;

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::Document,
    error::{Error, Result},
    format::Format,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};

/// Archive parser supporting ZIP, TAR, GZIP
pub struct ArchiveParser {
    format: Format,
}

impl ArchiveParser {
    /// Create a new archive parser for the specified format
    pub fn new(format: Format) -> Self {
        Self { format }
    }
}

#[async_trait]
impl Parser for ArchiveParser {
    fn format(&self) -> Format {
        self.format.clone()
    }

    fn can_parse(&self, _data: &[u8]) -> bool {
        true
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        // Delegate based on mime type
        if self.format.mime_type == "application/zip" {
            return zip::parse(context, data).await;
        } else if self.format.mime_type == "application/x-tar" {
            return tar::parse(context, data).await;
        } else if self.format.mime_type == "application/gzip" {
            return gzip::parse(context, data).await;
        }

        Err(Error::UnsupportedFormat(format!(
            "Unsupported archive format: {}",
            self.format.name
        )))
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: format!("{} Parser", self.format.name),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::MetadataExtraction,
                ParserFeature::TableExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::format::Format;
    use prism_core::parser::ParseOptions;
    use std::io::Write;

    // Explicitly reference crates to avoid ambiguity with modules
    use ::flate2 as flate2_crate;
    use ::tar as tar_crate;
    use ::zip as zip_crate;

    /*
    #[tokio::test]
    async fn test_zip_parser() {
        let mut buf = Vec::new();
        {
            let mut zip = zip_crate::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = zip_crate::write::FileOptions::default()
                .compression_method(zip_crate::CompressionMethod::Stored);
            zip.start_file("test.txt", options).unwrap();
            zip.write_all(b"Hello World").unwrap();
            zip.finish().unwrap();
        }

        let parser = ArchiveParser::new(Format::zip());
        let context = ParseContext {
            format: Format::zip(),
            filename: Some("test.zip".to_string()),
            size: buf.len(),
            options: ParseOptions::default(),
        };

        let result = parser.parse(Bytes::from(buf), context).await;
        assert!(result.is_ok());
        let doc = result.unwrap();
        assert!(!doc.pages.is_empty());
        assert!(!doc.pages[0].content.is_empty());
    }
    */

    #[tokio::test]
    async fn test_tar_parser() {
        let mut buf = Vec::new();
        {
            let mut tar = tar_crate::Builder::new(&mut buf);
            let mut header = tar_crate::Header::new_gnu();
            header.set_size(11);
            header.set_cksum();
            tar.append_data(&mut header, "test.txt", &b"Hello World"[..])
                .unwrap();
            tar.finish().unwrap();
        }

        let parser = ArchiveParser::new(Format::tar());
        let context = ParseContext {
            format: Format::tar(),
            filename: Some("test.tar".to_string()),
            size: buf.len(),
            options: ParseOptions::default(),
        };

        let result = parser.parse(Bytes::from(buf), context).await;
        assert!(result.is_ok());
        let doc = result.unwrap();
        assert!(!doc.pages.is_empty());
        assert!(!doc.pages[0].content.is_empty());
    }

    #[tokio::test]
    async fn test_gzip_parser() {
        let mut buf = Vec::new();
        {
            let mut encoder =
                flate2_crate::write::GzEncoder::new(&mut buf, flate2_crate::Compression::default());
            encoder.write_all(b"Hello World").unwrap();
            encoder.finish().unwrap();
        }

        let parser = ArchiveParser::new(Format::gzip());
        let context = ParseContext {
            format: Format::gzip(),
            filename: Some("test.txt.gz".to_string()),
            size: buf.len(),
            options: ParseOptions::default(),
        };

        let result = parser.parse(Bytes::from(buf), context).await;
        assert!(result.is_ok());
        let doc = result.unwrap();
        assert!(!doc.pages.is_empty());
        assert!(!doc.pages[0].content.is_empty());
    }
}
