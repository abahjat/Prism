// SPDX-License-Identifier: AGPL-3.0-only
//! Parser registry for managing and discovering format parsers.

use prism_core::format::Format;
use prism_core::parser::Parser;
use std::collections::HashMap;
use std::sync::Arc;

/// Registry for managing format parsers
///
/// The registry maintains a collection of available parsers and provides
/// methods to find the appropriate parser for a given format.
#[derive(Clone, Default)]
pub struct ParserRegistry {
    parsers: HashMap<String, Arc<dyn Parser>>,
}

impl ParserRegistry {
    /// Create a new empty parser registry
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry with all default parsers registered
    #[must_use]
    pub fn with_default_parsers() -> Self {
        let mut registry = Self::new();

        // Register archive parsers
        registry.register(Arc::new(crate::archive::ArchiveParser::new(Format::zip())));
        registry.register(Arc::new(crate::archive::ArchiveParser::new(Format::tar())));
        registry.register(Arc::new(crate::archive::ArchiveParser::new(Format::gzip())));
        registry.register(Arc::new(crate::archive::ArchiveParser::new(
            Format::seven_zip(),
        )));
        registry.register(Arc::new(
            crate::archive::ArchiveParser::new(Format::bzip2()),
        ));

        // Register text parsers
        registry.register(Arc::new(crate::text::CsvParser::new()));
        registry.register(Arc::new(crate::text::RtfParser::new()));
        registry.register(Arc::new(crate::text::TextParser::new()));
        registry.register(Arc::new(crate::text::MarkdownParser::new()));
        registry.register(Arc::new(crate::text::JsonParser::new()));
        registry.register(Arc::new(crate::text::XmlParser::new()));
        registry.register(Arc::new(crate::text::LogParser::new()));
        registry.register(Arc::new(crate::text::HtmlParser::new()));
        registry.register(Arc::new(crate::text::EpubParser::new()));

        // Register code parsers
        registry.register(Arc::new(crate::text::CodeParser::new(Format::rust())));
        registry.register(Arc::new(crate::text::CodeParser::new(Format::python())));
        registry.register(Arc::new(crate::text::CodeParser::new(Format::javascript())));
        registry.register(Arc::new(crate::text::CodeParser::new(Format::typescript())));
        registry.register(Arc::new(crate::text::CodeParser::new(Format::c())));
        registry.register(Arc::new(crate::text::CodeParser::new(Format::cpp())));
        registry.register(Arc::new(crate::text::CodeParser::new(Format::css())));

        // Register office parsers
        registry.register(Arc::new(crate::office::DocxParser::new()));
        registry.register(Arc::new(crate::office::XlsxParser::new()));
        registry.register(Arc::new(crate::office::PptxParser::new()));
        registry.register(Arc::new(crate::office::PptxParser::new_with_format(
            Format::potx(),
        )));
        registry.register(Arc::new(crate::office::PptxParser::new_with_format(
            Format::ppsx(),
        )));
        registry.register(Arc::new(crate::office::PptxParser::new_with_format(
            Format::pptm(),
        )));

        registry.register(Arc::new(crate::office::DocParser::new()));
        registry.register(Arc::new(crate::office::XlsParser::new()));
        registry.register(Arc::new(crate::office::PptParser::new()));
        registry.register(Arc::new(crate::office::MppParser::new()));
        registry.register(Arc::new(crate::office::OdtParser::new()));
        registry.register(Arc::new(crate::office::OdsParser::new()));
        registry.register(Arc::new(crate::office::OdpParser::new()));
        registry.register(Arc::new(crate::office::OdpParser::new_with_format(
            Format::otp(),
        )));
        registry.register(Arc::new(crate::office::OdgParser::new()));
        registry.register(Arc::new(crate::office::OneNoteParser::new()));
        registry.register(Arc::new(crate::office::VsdxParser::new()));
        registry.register(Arc::new(crate::office::XpsParser::new()));

        // Register PDF parsers
        registry.register(Arc::new(crate::pdf::PdfParser::new()));

        // Register email parsers
        registry.register(Arc::new(crate::email::MsgParser::new()));
        registry.register(Arc::new(crate::email::EmlParser::new()));
        registry.register(Arc::new(crate::email::MboxParser::new()));
        registry.register(Arc::new(crate::email::MhtParser::new()));
        // IcsParser and VcfParser are also available in lib.rs
        registry.register(Arc::new(crate::email::IcsParser::new()));
        registry.register(Arc::new(crate::email::VcfParser::new()));

        // Register image parsers
        registry.register(Arc::new(crate::image::TiffParser::new()));
        registry.register(Arc::new(crate::image::PngParser::new()));
        registry.register(Arc::new(crate::image::JpegParser::new()));
        registry.register(Arc::new(crate::image::GifParser::new()));
        registry.register(Arc::new(crate::image::WebpParser::new()));
        registry.register(Arc::new(crate::image::BmpParser::new()));
        registry.register(Arc::new(crate::image::IcoParser::new()));
        registry.register(Arc::new(crate::image::SvgParser::new()));
        registry.register(Arc::new(crate::image::SvgzParser::new()));
        registry.register(Arc::new(crate::image::TgaParser::new()));
        registry.register(Arc::new(crate::image::EmfParser::new()));
        registry.register(Arc::new(crate::image::EmzParser::new()));
        registry.register(Arc::new(crate::image::WmfParser::new()));
        registry.register(Arc::new(crate::image::EpsParser::new()));
        registry.register(Arc::new(crate::image::PsdParser::new()));
        registry.register(Arc::new(crate::image::Jpeg2000Parser::new()));
        registry.register(Arc::new(crate::image::PcxParser::new()));
        registry.register(Arc::new(crate::image::WbmpParser::new()));
        registry.register(Arc::new(crate::image::AiParser::new()));

        // Register CAD parsers
        registry.register(Arc::new(crate::cad::DxfParser::new()));

        // Register database parsers
        registry.register(Arc::new(crate::database::DbfParser::new()));
        registry.register(Arc::new(crate::database::SqliteParser::new()));

        // Register archive parsers for UNIX Compress
        registry.register(Arc::new(crate::archive::ArchiveParser::new(
            Format::unix_compress(),
        )));

        registry
    }

    /// Register a parser for a specific format
    ///
    /// # Arguments
    ///
    /// * `parser` - The parser implementation to register
    pub fn register(&mut self, parser: Arc<dyn Parser>) {
        let format = parser.format();
        self.parsers.insert(format.mime_type.clone(), parser);
    }

    /// Get a parser for the given format
    ///
    /// # Arguments
    ///
    /// * `format` - The document format
    ///
    /// # Returns
    ///
    /// The registered parser for this format, if available
    #[must_use]
    pub fn get_parser(&self, format: &Format) -> Option<Arc<dyn Parser>> {
        self.parsers.get(&format.mime_type).cloned()
    }

    /// Get a parser for the given format and data
    ///
    /// This method checks if the parser can actually handle the specific file
    /// by calling `can_parse()` before returning it.
    ///
    /// # Arguments
    ///
    /// * `format` - The document format
    /// * `data` - The file data to verify the parser can handle
    ///
    /// # Returns
    ///
    /// The registered parser for this format if it can parse the data
    #[must_use]
    pub fn get_parser_for_data(&self, format: &Format, data: &[u8]) -> Option<Arc<dyn Parser>> {
        self.parsers.get(&format.mime_type).and_then(|parser| {
            if parser.can_parse(data) {
                Some(parser.clone())
            } else {
                None
            }
        })
    }

    /// Get all registered parsers
    #[must_use]
    pub fn all_parsers(&self) -> Vec<Arc<dyn Parser>> {
        self.parsers.values().cloned().collect()
    }

    /// Check if a parser is registered for the given format
    #[must_use]
    pub fn has_parser(&self, format: &Format) -> bool {
        self.parsers.contains_key(&format.mime_type)
    }

    /// Get the number of registered parsers
    #[must_use]
    pub fn count(&self) -> usize {
        self.parsers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = ParserRegistry::new();
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_has_parser() {
        let registry = ParserRegistry::new();
        let format = Format::pdf();
        assert!(!registry.has_parser(&format));
    }

    #[test]
    fn test_python_parser_registration() {
        let registry = ParserRegistry::with_default_parsers();
        let format = Format::python();
        assert_eq!(format.mime_type, "text/x-python");
        assert!(
            registry.has_parser(&format),
            "Should have parser for Python"
        );

        // Also check by explicit string MIME type to be sure
        let parser = registry.parsers.get("text/x-python");
        assert!(parser.is_some(), "Should find parser by explicit mime type");
    }
}
