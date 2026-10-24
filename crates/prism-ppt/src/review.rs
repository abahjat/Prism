// SPDX-License-Identifier: AGPL-3.0-only
//! Hidden-content and forensic findings provider for `PowerPoint` presentations (`.pptx` and `.ppt`).

use crate::model::{Presentation, ShapeFill, ShapeKind, Slide, SlideShape, TextBody};
use prism_core::review::{
    Finding, FindingKind, HiddenContentReport, Location, ReviewRect, Severity,
};
use prism_layout::{LayoutDocument, Rgba};

impl Presentation {
    /// Generate the cross-format [`HiddenContentReport`] for this presentation.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn hidden_content_report(
        &self,
        format_id: &str,
        layout: Option<&LayoutDocument>,
    ) -> HiddenContentReport {
        let mut report = HiddenContentReport::for_format(format_id);

        for (idx, slide) in self.slides.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let slide_index = idx as u32;

            // 1. Hidden slide
            if slide.hidden {
                let slide_text = extract_slide_text(slide);
                report.push(Finding::new(
                    FindingKind::HiddenSlide,
                    Severity::High,
                    Location::Slide {
                        slide_index,
                        rects: vec![ReviewRect::new(
                            0.0,
                            0.0,
                            self.slide_width_pt,
                            self.slide_height_pt,
                        )],
                    },
                    None,
                    (!slide_text.is_empty())
                        .then_some(slide_text)
                        .or_else(|| Some(format!("Hidden slide {}", slide.number))),
                    serde_json::json!({
                        "slide_number": slide.number,
                        "slide_id": slide.id,
                    }),
                ));
            }

            // 2. Speaker notes
            if let Some(ref notes) = slide.notes
                && !notes.plain_text.trim().is_empty()
            {
                report.push(Finding::new(
                    FindingKind::SpeakerNotes,
                    Severity::Info,
                    Location::Slide {
                        slide_index,
                        rects: Vec::new(),
                    },
                    None,
                    Some(notes.plain_text.clone()),
                    serde_json::json!({
                        "slide_number": slide.number,
                    }),
                ));
            }

            // 3. Slide comments
            for comment in &slide.comments {
                report.push(Finding::new(
                    FindingKind::Comment,
                    Severity::Notable,
                    Location::Slide {
                        slide_index,
                        rects: vec![ReviewRect::new(comment.x_pt, comment.y_pt, 24.0, 24.0)],
                    },
                    None,
                    Some(comment.text.clone()),
                    serde_json::json!({
                        "slide_number": slide.number,
                        "author": comment.author,
                        "initials": comment.initials,
                        "date": comment.date,
                    }),
                ));
            }

            // 4. Inspect shapes on the slide
            let slide_bg = match slide.background {
                ShapeFill::Solid(c) => c,
                _ => Rgba::WHITE,
            };
            self.inspect_slide_shapes(
                slide_index,
                slide.number,
                &slide.shapes,
                slide_bg,
                &mut report,
            );
        }

        // 5. Forensic facts (VBA, external links, media)
        if self.forensic.vba_project_present {
            report.push(Finding::new(
                FindingKind::MacroCode,
                Severity::High,
                Location::PartPath {
                    path: "ppt/vbaProject.bin".to_string(),
                },
                None,
                Some("VBA macro project present".to_string()),
                serde_json::json!({ "vba_project_present": true }),
            ));
        }

        for link in &self.forensic.external_links {
            report.push(Finding::new(
                FindingKind::ExternalLinkOrConnection,
                Severity::Notable,
                Location::Document,
                None,
                Some(link.clone()),
                serde_json::json!({ "target": link }),
            ));
        }

        for media in &self.forensic.media_objects {
            report.push(Finding::new(
                FindingKind::EmbeddedFile,
                Severity::Info,
                Location::PartPath {
                    path: media.clone(),
                },
                None,
                Some(media.clone()),
                serde_json::json!({ "media_target": media }),
            ));
        }

        if self.author.is_some() || !self.forensic.comment_authors.is_empty() {
            report.push(Finding::new(
                FindingKind::AuthorAndModificationHistory,
                Severity::Info,
                Location::Document,
                None,
                self.author.clone(),
                serde_json::json!({
                    "author": self.author,
                    "title": self.title,
                    "comment_authors": self.forensic.comment_authors,
                }),
            ));
        }

        // 6. Merge positioned layout concealment checks
        if let Some(layout_doc) = layout {
            for lf in layout_doc.inspect_concealed_text(true) {
                let duplicate = report
                    .items
                    .iter()
                    .any(|existing| existing.kind == lf.kind && existing.excerpt == lf.excerpt);
                if !duplicate {
                    report.push(lf);
                }
            }
        }

        report
    }

    fn inspect_slide_shapes(
        &self,
        slide_index: u32,
        slide_number: u32,
        shapes: &[SlideShape],
        parent_bg: Rgba,
        report: &mut HiddenContentReport,
    ) {
        for shape in shapes {
            let rect = ReviewRect::new(
                shape.x_pt,
                shape.y_pt,
                shape.width_pt.max(1.0),
                shape.height_pt.max(1.0),
            );
            let loc = Location::Slide {
                slide_index,
                rects: vec![rect],
            };
            let shape_text = extract_shape_text(shape);

            if shape.hidden && !shape_text.trim().is_empty() {
                report.push(Finding::new(
                    FindingKind::HiddenText,
                    Severity::High,
                    loc.clone(),
                    None,
                    Some(shape_text.clone()),
                    serde_json::json!({
                        "slide_number": slide_number,
                        "shape_name": shape.name,
                        "hidden_shape": true,
                    }),
                ));
            }

            if shape.has_explicit_xfrm
                && !shape_text.trim().is_empty()
                && (shape.x_pt + shape.width_pt < 0.0
                    || shape.y_pt + shape.height_pt < 0.0
                    || shape.x_pt > self.slide_width_pt
                    || shape.y_pt > self.slide_height_pt)
            {
                report.push(Finding::new(
                    FindingKind::OffPageOrOffSlideText,
                    Severity::High,
                    loc.clone(),
                    None,
                    Some(shape_text.clone()),
                    serde_json::json!({
                        "slide_number": slide_number,
                        "shape_name": shape.name,
                        "x_pt": shape.x_pt,
                        "y_pt": shape.y_pt,
                    }),
                ));
            }

            let effective_bg = match shape.fill {
                ShapeFill::Solid(c) => c,
                _ => parent_bg,
            };

            match &shape.kind {
                ShapeKind::AutoShape {
                    text_body: Some(tb),
                } => {
                    inspect_text_body(slide_index, slide_number, &loc, tb, effective_bg, report);
                }
                ShapeKind::Table(tbl) => {
                    for row in &tbl.rows {
                        for cell in &row.cells {
                            let cell_bg = match cell.fill {
                                ShapeFill::Solid(c) => c,
                                _ => effective_bg,
                            };
                            if let Some(ref tb) = cell.text_body {
                                inspect_text_body(
                                    slide_index,
                                    slide_number,
                                    &loc,
                                    tb,
                                    cell_bg,
                                    report,
                                );
                            }
                        }
                    }
                }
                ShapeKind::Group { children, .. } => {
                    self.inspect_slide_shapes(
                        slide_index,
                        slide_number,
                        children,
                        effective_bg,
                        report,
                    );
                }
                ShapeKind::SmartArt { shapes: inner, .. } => {
                    self.inspect_slide_shapes(
                        slide_index,
                        slide_number,
                        inner,
                        effective_bg,
                        report,
                    );
                }
                _ => {}
            }
        }
    }
}

fn inspect_text_body(
    slide_index: u32,
    slide_number: u32,
    loc: &Location,
    tb: &TextBody,
    bg: Rgba,
    report: &mut HiddenContentReport,
) {
    let _ = slide_index;
    for para in &tb.paragraphs {
        for run in &para.runs {
            if run.text.is_empty() {
                continue;
            }
            if run.text.chars().any(|c| {
                matches!(
                    c,
                    '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}'
                )
            }) {
                report.push(Finding::new(
                    FindingKind::ZeroWidthCharacters,
                    Severity::Notable,
                    loc.clone(),
                    None,
                    Some(run.text.clone()),
                    serde_json::json!({ "slide_number": slide_number }),
                ));
            }
            if run.text.trim().is_empty() {
                continue;
            }
            if let Some(sz) = run.style.font_size_pt
                && sz < 2.0
            {
                report.push(Finding::new(
                    FindingKind::TinyText,
                    Severity::High,
                    loc.clone(),
                    None,
                    Some(run.text.clone()),
                    serde_json::json!({
                        "slide_number": slide_number,
                        "font_size_pt": sz,
                    }),
                ));
            }
            if let Some(fg) = run.style.color {
                let dist = (i32::from(fg.r) - i32::from(bg.r)).abs()
                    + (i32::from(fg.g) - i32::from(bg.g)).abs()
                    + (i32::from(fg.b) - i32::from(bg.b)).abs();
                if dist <= 24 {
                    report.push(Finding::new(
                        FindingKind::WhiteOrLowContrastText,
                        Severity::High,
                        loc.clone(),
                        None,
                        Some(run.text.clone()),
                        serde_json::json!({
                            "slide_number": slide_number,
                            "text_color": fg.to_hex_rgb(),
                            "background_color": bg.to_hex_rgb(),
                        }),
                    ));
                }
            }
        }
    }
}

fn extract_slide_text(slide: &Slide) -> String {
    let mut parts = Vec::new();
    for shape in &slide.shapes {
        let t = extract_shape_text(shape);
        if !t.trim().is_empty() {
            parts.push(t);
        }
    }
    parts.join(" ")
}

fn extract_shape_text(shape: &SlideShape) -> String {
    match &shape.kind {
        ShapeKind::AutoShape {
            text_body: Some(tb),
        } => extract_tb_text(tb),
        ShapeKind::Table(tbl) => {
            let mut parts = Vec::new();
            for row in &tbl.rows {
                for cell in &row.cells {
                    if let Some(ref tb) = cell.text_body {
                        let s = extract_tb_text(tb);
                        if !s.trim().is_empty() {
                            parts.push(s);
                        }
                    }
                }
            }
            parts.join(" ")
        }
        ShapeKind::Group { children, .. } => children
            .iter()
            .map(extract_shape_text)
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        ShapeKind::SmartArt {
            shapes,
            fallback_items,
        } => {
            let s: Vec<_> = shapes
                .iter()
                .map(extract_shape_text)
                .filter(|t| !t.trim().is_empty())
                .collect();
            if s.is_empty() {
                fallback_items.join(" ")
            } else {
                s.join(" ")
            }
        }
        _ => String::new(),
    }
}

fn extract_tb_text(tb: &TextBody) -> String {
    tb.paragraphs
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
