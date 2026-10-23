// SPDX-License-Identifier: AGPL-3.0-only
//! DOCX (Microsoft Word) parser
//!
//! Parses DOCX files into the Unified Document Model via [`prism_word`].

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::Document,
    error::Result,
    format::Format,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use prism_word::{is_docx_zip, parse_docx, word_to_udm};
use tracing::debug;

/// DOCX parser
#[derive(Debug, Clone)]
pub struct DocxParser;

impl DocxParser {
    /// Create a new DOCX parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Parse a DOCX byte slice directly into a rich [`prism_word::WordDocument`].
    ///
    /// # Errors
    /// Returns an error if the ZIP archive is malformed or exceeds memory limits.
    pub fn parse_word_document(
        &self,
        data: &[u8],
        context: &ParseContext,
    ) -> Result<prism_word::WordDocument> {
        let limit = crate::limits::max_decompressed_bytes(context);
        parse_docx(data, context.filename.as_deref(), limit)
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
        is_docx_zip(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing DOCX file: {:?}", context.filename);
        let word_doc = self.parse_word_document(data.as_ref(), &context)?;
        Ok(word_to_udm(&word_doc, "DOCX"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::document::{ContentBlock, ImageBlock, TextRun};
    use prism_core::error::Error;
    use std::io::{Cursor, Write};

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    fn build_docx(body: &str, styles: Option<&str>, extra: &[(&str, &[u8])]) -> Bytes {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::FileOptions::default();
        let document = format!(
            r#"<?xml version="1.0"?><w:document {NS}><w:body>{body}</w:body></w:document>"#
        );
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document.as_bytes()).unwrap();
        if let Some(styles) = styles {
            zip.start_file("word/styles.xml", options).unwrap();
            zip.write_all(
                format!(r#"<?xml version="1.0"?><w:styles {NS}>{styles}</w:styles>"#).as_bytes(),
            )
            .unwrap();
        }
        for (name, data) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        Bytes::from(zip.finish().unwrap().into_inner())
    }

    async fn parse(data: Bytes) -> Document {
        let context = ParseContext {
            format: Format::docx(),
            filename: Some("test.docx".to_string()),
            size: data.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        DocxParser::new().parse(data, context).await.unwrap()
    }

    fn runs(document: &Document) -> Vec<&TextRun> {
        document.pages[0]
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(&text.runs),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[tokio::test]
    async fn explicit_off_overrides_style_and_empty_properties_apply() {
        let styles =
            r#"<w:style w:type="paragraph" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>"#;
        let body = r#"<w:p><w:pPr><w:pStyle w:val="Strong"/></w:pPr>
            <w:r><w:t>bold </w:t></w:r>
            <w:r><w:rPr><w:b w:val="0"/><w:sz w:val="28"/><w:rFonts w:ascii="Georgia"/><w:color w:val="FF0000"/></w:rPr><w:t>plain</w:t></w:r>
            <w:r><w:rPr><w:u w:val="none"/><w:i w:val="false"/></w:rPr><w:t>x</w:t></w:r>
        </w:p>"#;
        let document = parse(build_docx(body, Some(styles), &[])).await;
        let runs = runs(&document);
        assert!(runs[0].style.bold);
        assert!(!runs[1].style.bold, "w:val=0 must turn bold off");
        assert_eq!(runs[1].style.font_size, Some(14.0));
        assert_eq!(runs[1].style.font_family.as_deref(), Some("Georgia"));
        assert_eq!(runs[1].style.color.as_deref(), Some("#FF0000"));
        assert!(!runs[2].style.underline && !runs[2].style.italic);
    }

    #[tokio::test]
    async fn field_codes_hidden_and_revisions_marked() {
        let body = r#"<w:p>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r>
            <w:r><w:instrText> HYPERLINK "https://example.com" </w:instrText></w:r>
            <w:r><w:fldChar w:fldCharType="separate"/></w:r>
            <w:r><w:t>link</w:t></w:r>
            <w:r><w:fldChar w:fldCharType="end"/></w:r>
            <w:ins w:id="1" w:author="A"><w:r><w:t>added</w:t></w:r></w:ins>
            <w:del w:id="2" w:author="A"><w:r><w:delText>removed</w:delText></w:r></w:del>
            <w:r><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r>
        </w:p>"#;
        let document = parse(build_docx(body, None, &[])).await;
        assert_eq!(document.extract_text(), "linkaddedremoveda\tb");
        let runs = runs(&document);
        let added = runs.iter().find(|r| r.text == "added").unwrap();
        let removed = runs.iter().find(|r| r.text == "removed").unwrap();
        let normal = runs.iter().find(|r| r.text == "link").unwrap();
        assert!(added.style.underline && !added.style.strikethrough);
        assert!(removed.style.strikethrough);
        assert!(!normal.style.underline && !normal.style.strikethrough);
    }

    #[tokio::test]
    async fn page_size_from_final_section() {
        let body = r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>
            <w:sectPr><w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/><w:pgMar w:top="1440" w:right="1080" w:bottom="1440" w:left="1080"/></w:sectPr>"#;
        let document = parse(build_docx(body, None, &[])).await;
        let dims = document.pages[0].dimensions;
        assert!((dims.width - 841.9).abs() < 0.1 && (dims.height - 595.3).abs() < 0.1);
        let margins = document.pages[0].metadata.margins.expect("w:pgMar margins");
        assert!((margins.top - 72.0).abs() < 0.1);
        assert!((margins.bottom - 72.0).abs() < 0.1);
        assert!((margins.left - 54.0).abs() < 0.1);
        assert!((margins.right - 54.0).abs() < 0.1);
    }

    #[tokio::test]
    async fn image_type_and_extent() {
        let mut png = Vec::new();
        image::RgbImage::new(4, 3)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
        let body = r#"<w:p><w:r><w:drawing><wp:inline>
            <wp:extent cx="914400" cy="457200"/><wp:docPr id="1" name="Picture 1" descr="Company logo"/>
            <a:graphic><a:graphicData><a:blip r:embed="rId5"/></a:graphicData></a:graphic>
        </wp:inline></w:drawing></w:r></w:p>"#;
        let document = parse(build_docx(
            body,
            None,
            &[
                ("word/_rels/document.xml.rels", rels),
                ("word/media/image1.png", &png),
            ],
        ))
        .await;
        let resource = &document.resources.images[0];
        assert_eq!(resource.mime_type, "image/png");
        assert_eq!((resource.width, resource.height), (4, 3));
        let image: &ImageBlock = document.pages[0]
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::Image(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let size = image.original_size.unwrap();
        assert!((size.width - 72.0).abs() < 0.01 && (size.height - 36.0).abs() < 0.01);
        assert_eq!(image.alt_text.as_deref(), Some("Company logo"));
    }

    #[tokio::test]
    async fn zip_entry_decompression_bomb_rejected() {
        let large_body = format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", "x".repeat(64 * 1024));
        let data = build_docx(&large_body, None, &[]);
        let context = ParseContext {
            format: Format::docx(),
            filename: Some("bomb.docx".to_string()),
            size: data.len(),
            options: prism_core::parser::ParseOptions {
                max_memory: Some(4096),
                ..prism_core::parser::ParseOptions::default()
            },
        };
        let err = DocxParser::new().parse(data, context).await.unwrap_err();
        assert!(
            matches!(err, Error::MemoryLimitExceeded { limit: 4096, .. }),
            "expected MemoryLimitExceeded, got {err:?}"
        );
    }
}
