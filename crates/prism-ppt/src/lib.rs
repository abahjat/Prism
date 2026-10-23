// SPDX-License-Identifier: AGPL-3.0-only
//! Native `PowerPoint` presentation model (`Presentation`), `.pptx` and legacy `.ppt` (`[MS-PPT]`)
//! readers, master/layout placeholder inheritance, and slide layout for Prism.

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

pub mod layout;
pub mod model;
pub mod ppt;
pub mod pptx;
pub mod udm;

pub use layout::layout_presentation;
pub use model::{
    AutofitMode, BulletKind, DEFAULT_SLIDE_HEIGHT_PT, DEFAULT_SLIDE_WIDTH_PT, NotesSlide,
    PlaceholderRef, PptTextAlign, PptTextRun, Presentation, PresentationForensicFacts, ShapeFill,
    ShapeKind, ShapeStroke, Slide, SlideComment, SlideLayout, SlideMaster, SlideShape, SlideTable,
    SlideTableCell, SlideTableRow, TextBody, TextLevelStyle, TextListStyle, TextParagraph,
    TextRunStyle, TextVerticalAnchor,
};
pub use ppt::{DEFAULT_MAX_PPT_STREAM_BYTES, is_ppt_bytes, parse_ppt};
pub use pptx::{DEFAULT_MAX_PPTX_ENTRY_BYTES, is_pptx_zip, parse_pptx};
pub use udm::presentation_to_udm;

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::error::Error;
    use prism_layout::{LayoutOptions, MarkupMode};
    use std::io::{Cursor, Write};

    fn build_pptx_zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
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
    fn parses_and_lays_out_pptx_hierarchy_autofit_notes_comments_and_charts() {
        let pres_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>
  <p:sldIdLst>
    <p:sldId id="256" r:id="rId2"/>
    <p:sldId id="257" r:id="rId3"/>
  </p:sldIdLst>
  <p:sldSz cx="12192000" cy="6858000"/>
</p:presentation>"#;

        let pres_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
</Relationships>"#;

        let master_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:sldMaster xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
             xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:bg><p:bgPr><a:solidFill><a:srgbClr val="F4F6F9"/></a:solidFill></p:bgPr></p:bg>
    <p:spTree>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="2" name="Master Banner"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="0" y="0"/><a:ext cx="12192000" cy="457200"/></a:xfrm>
          <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
          <a:solidFill><a:srgbClr val="1F4E79"/></a:solidFill>
        </p:spPr>
      </p:sp>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="3" name="Title Placeholder 1"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="914400" y="685800"/><a:ext cx="10363200" cy="1143000"/></a:xfrm>
          <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
        </p:spPr>
      </p:sp>
    </p:spTree>
  </p:cSld>
  <p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/>
</p:sldMaster>"#;

        let layout_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:sldLayout xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
             xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" showMasterSp="1">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
        <p:spPr/>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sldLayout>"#;

        let layout_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/>
</Relationships>"#;

        let slide1_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:bodyPr><a:normAutofit fontScale="80000"/></a:bodyPr>
          <a:lstStyle/>
          <a:p><a:r><a:t>Inherited Position Title</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="9" name="OffSlide" hidden="1"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="-5000000" y="-5000000"/><a:ext cx="100000" cy="100000"/></a:xfrm>
        </p:spPr>
        <p:txBody><a:bodyPr/><a:p><a:r><a:t>Secret Hidden Offslide</a:t></a:r></a:p></p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let slide1_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide1.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments/comment1.xml"/>
</Relationships>"#;

        let notes1_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:notes xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
         xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr>
        <p:spPr/>
        <p:txBody><a:bodyPr/><a:p><a:r><a:t>Speaker note for slide 1</a:t></a:r></a:p></p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:notes>"#;

        let comment1_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:cmLst xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cm authorId="0" dt="2026-01-01T00:00:00.000"><p:pos x="100" y="100"/><p:text>Check Q4 numbers</p:text></p:cm>
</p:cmLst>"#;

        let slide2_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" show="0">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="2" name="Title 2"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
        <p:spPr/>
        <p:txBody><a:bodyPr/><a:p><a:r><a:t>Backup Hidden Slide</a:t></a:r></a:p></p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let zip_bytes = build_pptx_zip(&[
            ("ppt/presentation.xml", pres_xml),
            ("ppt/_rels/presentation.xml.rels", pres_rels),
            ("ppt/slideMasters/slideMaster1.xml", master_xml),
            ("ppt/slideLayouts/slideLayout1.xml", layout_xml),
            ("ppt/slideLayouts/_rels/slideLayout1.xml.rels", layout_rels),
            ("ppt/slides/slide1.xml", slide1_xml),
            ("ppt/slides/_rels/slide1.xml.rels", slide1_rels),
            ("ppt/notesSlides/notesSlide1.xml", notes1_xml),
            ("ppt/comments/comment1.xml", comment1_xml),
            ("ppt/slides/slide2.xml", slide2_xml),
        ]);

        assert!(is_pptx_zip(&zip_bytes));
        let pres = parse_pptx(&zip_bytes, DEFAULT_MAX_PPTX_ENTRY_BYTES).unwrap();
        assert_eq!(pres.slides.len(), 2);
        assert_eq!(pres.forensic.hidden_slide_numbers, vec![2]);
        assert_eq!(pres.forensic.hidden_shape_count, 1);
        assert_eq!(pres.forensic.off_slide_shape_count, 1);

        // Layout with show_hidden_text = true includes both slides + notes + comments
        let opts_all = LayoutOptions {
            show_hidden_text: true,
            markup_mode: MarkupMode::AllMarkup,
            ..LayoutOptions::default()
        };
        let doc_all = layout_presentation(&pres, &opts_all).unwrap();
        assert_eq!(doc_all.pages.len(), 2);
        let p1_text = doc_all.pages[0].text();
        assert!(p1_text.contains("Inherited Position Title"));
        assert!(p1_text.contains("Speaker note for slide 1"));
        assert!(p1_text.contains("Check Q4 numbers"));

        // Layout with show_hidden_text = false omits hidden slide 2
        let opts_visible = LayoutOptions {
            show_hidden_text: false,
            markup_mode: MarkupMode::NoMarkup,
            ..LayoutOptions::default()
        };
        let doc_vis = layout_presentation(&pres, &opts_visible).unwrap();
        assert_eq!(doc_vis.pages.len(), 1);
    }

    #[test]
    fn encrypted_legacy_ppt_returns_encrypted_document_error() {
        let cursor = Cursor::new(Vec::new());
        let mut comp = cfb::CompoundFile::create(cursor).unwrap();
        {
            let mut stream = comp.create_stream("/PowerPoint Document").unwrap();
            // CryptSession10Container record header (recVer=0x0F, recType=0x2F14, recLen=0)
            let hdr: [u8; 8] = [0x0F, 0x00, 0x14, 0x2F, 0x00, 0x00, 0x00, 0x00];
            stream.write_all(&hdr).unwrap();
        }
        let bytes = comp.into_inner().into_inner();
        assert!(is_ppt_bytes(&bytes));
        let err = parse_ppt(&bytes, DEFAULT_MAX_PPT_STREAM_BYTES).unwrap_err();
        assert!(matches!(err, Error::Encrypted(_)));
    }

    #[test]
    fn parses_legacy_ppt_slide_and_text_chars_atom() {
        let cursor = Cursor::new(Vec::new());
        let mut comp = cfb::CompoundFile::create(cursor).unwrap();
        {
            let mut stream = comp.create_stream("/PowerPoint Document").unwrap();
            // Build a SlideContainer (recVer=0x0F, recType=0x03EE) containing a PPDrawing -> DgContainer -> SpgrContainer -> SpContainer -> ClientTextbox -> TextCharsAtom
            let text_utf16: Vec<u8> = "Legacy PPT Slide Title"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();

            let mut tca = Vec::new();
            tca.extend_from_slice(&0x0000_u16.to_le_bytes());
            tca.extend_from_slice(&0x0FA0_u16.to_le_bytes());
            #[allow(clippy::cast_possible_truncation)]
            tca.extend_from_slice(&(text_utf16.len() as u32).to_le_bytes());
            tca.extend_from_slice(&text_utf16);

            let wrap_container = |ver_inst: u16, rec_type: u16, inner: &[u8]| -> Vec<u8> {
                let mut v = Vec::with_capacity(8 + inner.len());
                v.extend_from_slice(&ver_inst.to_le_bytes());
                v.extend_from_slice(&rec_type.to_le_bytes());
                #[allow(clippy::cast_possible_truncation)]
                v.extend_from_slice(&(inner.len() as u32).to_le_bytes());
                v.extend_from_slice(inner);
                v
            };

            let client_tb = wrap_container(0x000F, 0xF00D, &tca);
            // Sp record (non-patriarch, shape_id=1025, flags=0)
            let sp_atom = wrap_container(0x0012, 0xF00A, &[1, 4, 0, 0, 0, 0, 0, 0]);
            // ClientAnchor (top=80, left=80, right=4000, bottom=800 in master units)
            let mut anchor_payload = Vec::new();
            for v in [80_i32, 80, 4000, 800] {
                anchor_payload.extend_from_slice(&v.to_le_bytes());
            }
            let client_anchor = wrap_container(0x0000, 0xF010, &anchor_payload);

            let mut sp_inner = Vec::new();
            sp_inner.extend_from_slice(&sp_atom);
            sp_inner.extend_from_slice(&client_anchor);
            sp_inner.extend_from_slice(&client_tb);
            let sp_container = wrap_container(0x000F, 0xF004, &sp_inner);

            // Patriarch dummy SpContainer first inside SpgrContainer
            let pat_atom = wrap_container(0x0002, 0xF00A, &[0, 4, 0, 0, 5, 0, 0, 0]);
            let pat_container = wrap_container(0x000F, 0xF004, &pat_atom);

            let mut spgr_inner = Vec::new();
            spgr_inner.extend_from_slice(&pat_container);
            spgr_inner.extend_from_slice(&sp_container);
            let spgr_container = wrap_container(0x000F, 0xF003, &spgr_inner);
            let dg_container = wrap_container(0x000F, 0xF002, &spgr_container);
            let pp_drawing = wrap_container(0x000F, 0x040C, &dg_container);
            let slide_container = wrap_container(0x000F, 0x03EE, &pp_drawing);

            stream.write_all(&slide_container).unwrap();
        }
        let bytes = comp.into_inner().into_inner();
        let pres = parse_ppt(&bytes, DEFAULT_MAX_PPT_STREAM_BYTES).unwrap();
        assert_eq!(pres.slides.len(), 1);
        let doc = layout_presentation(&pres, &LayoutOptions::default()).unwrap();
        assert!(doc.pages[0].text().contains("Legacy PPT Slide Title"));
    }
}
