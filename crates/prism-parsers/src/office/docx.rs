//! DOCX (Microsoft Word) parser
//!
//! Parses DOCX files into the Unified Document Model with high fidelity.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{
        ContentBlock, Dimensions, Document, Page, PageMetadata, Rect, TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::Cursor;
use tracing::{debug, warn};
use zip::ZipArchive;

use crate::office::relationships::Relationships;
use crate::office::styles::Styles;
use crate::office::tables;
use crate::office::utils;

/// DOCX parser
#[derive(Debug, Clone)]
pub struct DocxParser;

impl DocxParser {
    /// Create a new DOCX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if data is a valid DOCX file (ZIP with word/document.xml)
    fn is_docx_zip(data: &[u8]) -> bool {
        if data.len() < 4 || &data[0..2] != b"PK" {
            return false;
        }

        let cursor = std::io::Cursor::new(data);
        if let Ok(mut archive) = ZipArchive::new(cursor) {
            for i in 0..archive.len() {
                if let Ok(file) = archive.by_index(i) {
                    if file.name() == "word/document.xml" {
                        return true;
                    }
                }
            }
        }
        false
    }
}

impl Default for DocxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for DocxParser {
    fn format(&self) -> Format {
        Format::docx()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        Self::is_docx_zip(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing DOCX file: {:?}", context.filename);

        let cursor = Cursor::new(data.as_ref());
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| Error::ParseError(format!("Failed to open DOCX ZIP: {}", e)))?;

        // 1. Parse Relationships
        let mut _rels = Relationships::new();
        if let Ok(mut file) = archive.by_name("word/_rels/document.xml.rels") {
            use std::io::Read;
            let mut xml = String::new();
            file.read_to_string(&mut xml).ok(); // Ignore errors, rels are optional-ish
            if let Ok(r) = Relationships::from_xml(&xml) {
                _rels = r;
            }
        }

        // 2. Parse Styles
        let mut styles = Styles::new();
        if let Ok(mut file) = archive.by_name("word/styles.xml") {
            use std::io::Read;
            let mut xml = String::new();
            file.read_to_string(&mut xml).ok();
            if let Ok(s) = Styles::from_xml(&xml) {
                styles = s;
            }
        }

        // 3. Parse Document Content
        let mut document_xml = String::new();
        match archive.by_name("word/document.xml") {
            Ok(mut file) => {
                use std::io::Read;
                file.read_to_string(&mut document_xml).map_err(|e| {
                    Error::ParseError(format!("Failed to read document.xml: {}", e))
                })?;
            }
            Err(_) => return Err(Error::ParseError("word/document.xml not found".to_string())),
        }

        // Streaming Parse of Document XML
        let mut reader = Reader::from_str(&document_xml);
        reader.trim_text(false);
        let mut buf = Vec::new();

        let mut pages = Vec::new();
        let mut current_page_content = Vec::new();

        // State for paragraph parsing
        let mut in_paragraph = false;
        let mut current_paragraph_runs = Vec::new();
        let mut current_paragraph_style: Option<String> = None;

        // State for run parsing
        let mut in_run = false;
        let mut current_run_text = String::new();
        let mut current_run_style = TextStyle::default();
        let mut in_run_props = false;

        // Count paragraphs for approximate pagination
        let mut para_count = 0;
        const PARAS_PER_PAGE: usize = 50;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = e.name();
                    match name.as_ref() {
                        b"w:p" => {
                            in_paragraph = true;
                            current_paragraph_runs.clear();
                            current_paragraph_style = None;
                            para_count += 1;
                        }
                        b"w:pPr" => {
                            // Paragraph properties (e.g. style)
                            // We need to parse this eagerly to apply to the paragraph
                        }
                        b"w:pStyle" => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"w:val" {
                                    current_paragraph_style = Some(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        b"w:r" => {
                            if in_paragraph {
                                in_run = true;
                                current_run_text.clear();
                                current_run_style = TextStyle::default();
                                // TODO: Apply paragraph style defaults here?
                            }
                        }
                        b"w:rPr" => in_run_props = true,
                        // Run Properties
                        b"w:b" if in_run_props => current_run_style.bold = true,
                        b"w:i" if in_run_props => current_run_style.italic = true,
                        b"w:u" if in_run_props => current_run_style.underline = true,
                        b"w:color" if in_run_props => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"w:val" {
                                    let val = utils::attr_value(&attr.value);
                                    if val != "auto" {
                                        current_run_style.color = Some(format!("#{}", val));
                                    }
                                }
                            }
                        }
                        b"w:sz" if in_run_props => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"w:val" {
                                    if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                        current_run_style.font_size = Some(val / 2.0);
                                    }
                                }
                            }
                        }
                        b"w:rFonts" if in_run_props => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"w:ascii" {
                                    current_run_style.font_family =
                                        Some(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        b"w:t" => {
                            // Text content
                        }
                        b"w:tbl" => {
                            // Delegate to table parser
                            // Note: parse_table expects we just consumed <w:tbl>
                            match tables::parse_table(&mut reader) {
                                Ok(table_block) => {
                                    current_page_content.push(ContentBlock::Table(table_block));
                                }
                                Err(e) => warn!("Failed to parse table: {}", e),
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(e)) => {
                    // Handle empty tags like <w:b/>
                    let name = e.name();
                    match name.as_ref() {
                        b"w:b" if in_run_props => current_run_style.bold = true,
                        b"w:i" if in_run_props => current_run_style.italic = true,
                        b"w:u" if in_run_props => current_run_style.underline = true,
                        b"w:pStyle" => {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"w:val" {
                                    current_paragraph_style = Some(utils::attr_value(&attr.value));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(e)) => {
                    match e.name().as_ref() {
                        b"w:p" => {
                            if !current_paragraph_runs.is_empty() {
                                let block = TextBlock {
                                    runs: current_paragraph_runs.clone(),
                                    paragraph_style: current_paragraph_style.clone(),
                                    bounds: Rect::default(),
                                };
                                current_page_content.push(ContentBlock::Text(block));

                                // Pagination logic
                                if para_count >= PARAS_PER_PAGE {
                                    pages.push(Page {
                                        number: (pages.len() + 1) as u32,
                                        dimensions: Dimensions::LETTER,
                                        content: current_page_content.clone(),
                                        annotations: Vec::new(),
                                        metadata: PageMetadata::default(),
                                    });
                                    current_page_content.clear();
                                    para_count = 0;
                                }
                            }
                            in_paragraph = false;
                        }
                        b"w:r" => {
                            if !current_run_text.is_empty() {
                                // Resolve style against global styles if needed
                                let effective_style = styles.resolve_text_style(
                                    current_paragraph_style.as_deref(),
                                    &current_run_style,
                                );

                                current_paragraph_runs.push(TextRun {
                                    text: current_run_text.clone(),
                                    style: effective_style,
                                    bounds: None,
                                    char_positions: None,
                                });
                            }
                            in_run = false;
                        }
                        b"w:rPr" => in_run_props = false,
                        _ => {}
                    }
                }
                Ok(Event::Text(e)) => {
                    if in_run {
                        if let Ok(text) = e.unescape() {
                            current_run_text.push_str(&text);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    warn!("XML error: {}", e);
                    break;
                }
                _ => {}
            }
            buf.clear();
        }

        // Add final page
        if !current_page_content.is_empty() {
            pages.push(Page {
                number: (pages.len() + 1) as u32,
                dimensions: Dimensions::LETTER,
                content: current_page_content,
                annotations: Vec::new(),
                metadata: PageMetadata::default(),
            });
        }

        // Ensure at least one page
        if pages.is_empty() {
            pages.push(Page::new(1, Dimensions::LETTER));
        }

        let mut metadata = Metadata::new();
        if let Some(filename) = context.filename {
            metadata.title = Some(filename);
        }
        metadata.add_custom("format", "DOCX");

        let mut document = Document::builder().metadata(metadata).build();
        document.pages = pages;
        document.structure.headings = Vec::new(); // TODO: Extract headings from structure

        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "DOCX Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}
