// SPDX-License-Identifier: AGPL-3.0-only
//! PPTX (Microsoft `PowerPoint`) parser
//!
//! Parses PPTX files into the Unified Document Model via [`prism_ppt`].

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::Document,
    error::Result,
    format::Format,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use prism_ppt::{is_pptx_zip, parse_pptx, presentation_to_udm};
use tracing::debug;

/// PPTX parser
#[derive(Debug, Clone)]
pub struct PptxParser {
    format: Format,
}

impl PptxParser {
    /// Create a new PPTX parser
    #[must_use]
    pub fn new() -> Self {
        Self {
            format: Format::pptx(),
        }
    }

    /// Create a new PPTX parser for a specific format variant (`PPSX`, `POTX`)
    #[must_use]
    pub fn new_with_format(format: Format) -> Self {
        Self { format }
    }

    /// Parse a PPTX byte slice directly into a [`prism_ppt::Presentation`].
    ///
    /// # Errors
    /// Returns an error if the ZIP archive is malformed or exceeds memory limits.
    pub fn parse_presentation(
        &self,
        data: &[u8],
        context: &ParseContext,
    ) -> Result<prism_ppt::Presentation> {
        let limit = crate::limits::max_decompressed_bytes(context);
        parse_pptx(data, limit)
    }
}

impl Default for PptxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PptxParser {
    fn format(&self) -> Format {
        self.format.clone()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        is_pptx_zip(data)
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing PPTX file: {:?}", context.filename);
        let pres = self.parse_presentation(data.as_ref(), &context)?;
        Ok(presentation_to_udm(&pres, "PPTX"))
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PPTX Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
                ParserFeature::ImageExtraction,
                ParserFeature::TableExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::document::ContentBlock;
    use prism_core::parser::ParseOptions;
    use std::io::{Cursor, Write};
    use zip::write::FileOptions;

    fn build_pptx_zip(files: &[(&str, &[u8])]) -> Bytes {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = FileOptions::default();
            for (name, content) in files {
                zip.start_file(*name, options).unwrap();
                zip.write_all(content).unwrap();
            }
            zip.finish().unwrap();
        }
        Bytes::from(buf.into_inner())
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_parse_pptx_with_background_and_placeholder_inheritance() {
        let presentation_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:sldSz cx="9144000" cy="6858000"/>
</p:presentation>"#;

        let theme_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme">
  <a:themeElements>
    <a:clrScheme name="Office">
      <a:dk1><a:srgbClr val="112233"/></a:dk1>
      <a:lt1><a:srgbClr val="FFFFFF"/></a:lt1>
      <a:accent1><a:srgbClr val="4472C4"/></a:accent1>
    </a:clrScheme>
    <a:fontScheme name="Office">
      <a:majorFont><a:latin typeface="Calibri Light"/></a:majorFont>
      <a:minorFont><a:latin typeface="Calibri"/></a:minorFont>
    </a:fontScheme>
  </a:themeElements>
</a:theme>"#;

        let master_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:sldMaster xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
             xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:bg>
      <p:bgPr>
        <a:solidFill><a:srgbClr val="F0F4F8"/></a:solidFill>
      </p:bgPr>
    </p:bg>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Title Placeholder 1"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr>
          <a:xfrm>
            <a:off x="457200" y="274638"/>
            <a:ext cx="8229600" cy="1143000"/>
          </a:xfrm>
        </p:spPr>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sldMaster>"#;

        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Title 1"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:r>
              <a:rPr sz="3200" b="1">
                <a:solidFill><a:schemeClr val="accent1"/></a:solidFill>
              </a:rPr>
              <a:t>Inherited Position Title</a:t>
            </a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let zip_bytes = build_pptx_zip(&[
            ("ppt/presentation.xml", presentation_xml),
            ("ppt/theme/theme1.xml", theme_xml),
            ("ppt/slideMasters/slideMaster1.xml", master_xml),
            ("ppt/slides/slide1.xml", slide_xml),
        ]);

        let parser = PptxParser::new();
        assert!(parser.can_parse(&zip_bytes));

        let context = ParseContext {
            format: Format::pptx(),
            filename: Some("test.pptx".to_string()),
            size: zip_bytes.len(),
            options: ParseOptions::default(),
        };

        let doc = parser.parse(zip_bytes, context).await.unwrap();
        assert_eq!(doc.page_count(), 1);

        let page = &doc.pages[0];
        assert!((page.dimensions.width - 720.0).abs() < 0.1);
        assert!((page.dimensions.height - 540.0).abs() < 0.1);

        // Background block should be emitted at z_index -1 with master solidFill #F0F4F8
        let has_bg = page.content.iter().any(|b| match b {
            ContentBlock::Text(tb) => {
                tb.style.z_index == Some(-1)
                    && tb
                        .style
                        .fill_color
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case("#F0F4F8"))
            }
            _ => false,
        });
        assert!(has_bg, "Expected master background fill block");

        // Title shape should inherit bounds from master placeholder (x = 457200 / 12700 = 36.0)
        let title_block = page
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::Text(tb)
                    if tb.extract_text().contains("Inherited Position Title") =>
                {
                    Some(tb)
                }
                _ => None,
            })
            .expect("Expected title text block");

        assert!(
            (title_block.bounds.x - 36.0).abs() < 0.5,
            "Expected inherited x=36.0, got {}",
            title_block.bounds.x
        );
        assert!(
            (title_block.bounds.width - 648.0).abs() < 0.5,
            "Expected inherited width=648.0, got {}",
            title_block.bounds.width
        );
    }
}
