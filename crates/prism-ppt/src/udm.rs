// SPDX-License-Identifier: AGPL-3.0-only
//! Conversion from [`Presentation`] to [`prism_core::document::Document`] (Unified Document Model).

use crate::model::{
    Presentation, ShapeFill, ShapeKind, Slide, SlideLayout, SlideMaster, SlideShape, SlideTable,
    TextBody,
};
use prism_core::document::{
    ContentBlock, Dimensions, Document, ImageBlock, Page, PageMetadata, Rect, ShapeStyle,
    TableBlock, TableCell, TableRow, TextBlock, TextRun, TextStyle,
};
use prism_core::metadata::Metadata;
use prism_drawingml::DrawingTheme;

/// Convert a [`Presentation`] into a [`prism_core::document::Document`].
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn presentation_to_udm(pres: &Presentation, format_label: &str) -> Document {
    let slide_dims = Dimensions::new(
        f64::from(pres.slide_width_pt),
        f64::from(pres.slide_height_pt),
    );
    let fallback_theme = DrawingTheme::default();
    let mut pages = Vec::new();

    for slide in &pres.slides {
        let (layout, master, theme) = pres.resolve_slide_hierarchy(slide, &fallback_theme);
        let mut content = Vec::new();

        let eff_bg = match &slide.background {
            ShapeFill::Inherited => layout
                .map(|l| &l.background)
                .filter(|b| !matches!(b, ShapeFill::Inherited))
                .or_else(|| {
                    master
                        .map(|m| &m.background)
                        .filter(|b| !matches!(b, ShapeFill::Inherited))
                }),
            other => Some(other),
        };
        if let Some(bg_fill) = eff_bg {
            let bg_bounds = Rect::new(0.0, 0.0, slide_dims.width, slide_dims.height);
            match bg_fill {
                ShapeFill::Solid(c) => {
                    content.push(ContentBlock::Text(TextBlock {
                        runs: Vec::new(),
                        paragraph_style: None,
                        bounds: bg_bounds,
                        vertical_alignment: None,
                        style: ShapeStyle {
                            fill_color: Some(c.to_hex_rgb()),
                            z_index: Some(-1),
                            ..ShapeStyle::default()
                        },
                        rotation: 0.0,
                    }));
                }
                ShapeFill::Picture { resource_id } => {
                    content.push(ContentBlock::Image(ImageBlock {
                        bounds: bg_bounds,
                        resource_id: resource_id.clone(),
                        alt_text: None,
                        format: None,
                        original_size: None,
                        style: ShapeStyle {
                            z_index: Some(-1),
                            ..ShapeStyle::default()
                        },
                        rotation: 0.0,
                    }));
                }
                _ => {}
            }
        }

        for shape in &slide.shapes {
            convert_shape_to_udm(pres, slide, shape, layout, master, theme, &mut content);
        }

        if let Some(ref notes) = slide.notes
            && !notes.plain_text.is_empty()
        {
            content.push(ContentBlock::Text(TextBlock {
                runs: vec![TextRun::new(&notes.plain_text)],
                paragraph_style: Some("Notes".to_string()),
                bounds: Rect::default(),
                vertical_alignment: None,
                style: ShapeStyle::default(),
                rotation: 0.0,
            }));
        }

        pages.push(Page {
            number: slide.number,
            dimensions: slide_dims,
            content,
            annotations: Vec::new(),
            metadata: PageMetadata {
                label: Some(format!("Slide {}", slide.number)),
                ..PageMetadata::default()
            },
        });
    }

    if pages.is_empty() {
        pages.push(Page::new(1, slide_dims));
    }

    let mut metadata = Metadata::new();
    metadata.title.clone_from(&pres.title);
    metadata.author.clone_from(&pres.author);
    metadata.add_custom("format", format_label);
    #[allow(clippy::cast_possible_wrap)]
    metadata.add_custom("slide_count", pages.len() as i64);
    if let Some(first_theme) = pres.themes.values().next() {
        if !first_theme.name.is_empty() {
            metadata.add_custom("theme_name", first_theme.name.clone());
        }
        metadata.add_custom("theme_font_major", first_theme.major_latin_font.clone());
        metadata.add_custom("theme_font_minor", first_theme.minor_latin_font.clone());
    }

    let mut document = Document::builder().metadata(metadata).build();
    document.pages = pages;
    document.resources.images = pres.images.values().cloned().collect();
    document.resources.fonts = pres.fonts.values().cloned().collect();
    document
}

#[allow(clippy::only_used_in_recursion, clippy::too_many_lines)]
fn convert_shape_to_udm(
    pres: &Presentation,
    slide: &Slide,
    shape: &SlideShape,
    layout: Option<&SlideLayout>,
    master: Option<&SlideMaster>,
    theme: &DrawingTheme,
    out: &mut Vec<ContentBlock>,
) {
    let (layout_ph, master_ph) = shape.placeholder.as_ref().map_or((None, None), |ph| {
        Presentation::resolve_placeholder_chain(ph, layout, master)
    });

    let has_own_bounds = shape.width_pt > 0.0 && shape.height_pt > 0.0;
    let (x_pt, y_pt, w_pt, h_pt) = if has_own_bounds {
        (shape.x_pt, shape.y_pt, shape.width_pt, shape.height_pt)
    } else if let Some(lph) = layout_ph.filter(|s| s.width_pt > 0.0 && s.height_pt > 0.0) {
        (lph.x_pt, lph.y_pt, lph.width_pt, lph.height_pt)
    } else if let Some(mph) = master_ph.filter(|s| s.width_pt > 0.0 && s.height_pt > 0.0) {
        (mph.x_pt, mph.y_pt, mph.width_pt, mph.height_pt)
    } else {
        (shape.x_pt, shape.y_pt, shape.width_pt, shape.height_pt)
    };

    let bounds = Rect::new(
        f64::from(x_pt),
        f64::from(y_pt),
        f64::from(w_pt.max(1.0)),
        f64::from(h_pt.max(1.0)),
    );

    let eff_fill = match &shape.fill {
        ShapeFill::Inherited => layout_ph
            .map(|s| &s.fill)
            .filter(|f| !matches!(f, ShapeFill::Inherited))
            .or_else(|| {
                master_ph
                    .map(|s| &s.fill)
                    .filter(|f| !matches!(f, ShapeFill::Inherited))
            })
            .unwrap_or(&shape.fill),
        other => other,
    };
    let eff_stroke = shape
        .stroke
        .as_ref()
        .or_else(|| layout_ph.and_then(|s| s.stroke.as_ref()))
        .or_else(|| master_ph.and_then(|s| s.stroke.as_ref()));

    let shape_style = ShapeStyle {
        fill_color: match eff_fill {
            ShapeFill::Solid(c) => Some(c.to_hex_rgb()),
            _ => None,
        },
        stroke_color: eff_stroke.map(|s| s.color.to_hex_rgb()),
        stroke_width: eff_stroke.map(|s| f64::from(s.width_pt)),
        ..ShapeStyle::default()
    };

    match &shape.kind {
        ShapeKind::AutoShape { text_body } => {
            // Only the slide's own text: the text in layout and master placeholders is prompt
            // text ("Click to add title"), which PowerPoint never shows on the slide.
            let runs = text_body
                .as_ref()
                .map(|tb| convert_text_body_runs(tb, slide.number, theme))
                .unwrap_or_default();
            if !runs.is_empty()
                || shape_style.fill_color.is_some()
                || shape_style.stroke_color.is_some()
            {
                out.push(ContentBlock::Text(TextBlock {
                    runs,
                    paragraph_style: None,
                    bounds,
                    vertical_alignment: None,
                    style: shape_style,
                    rotation: f64::from(shape.rotation_deg),
                }));
            }
        }
        ShapeKind::Picture { resource_id, .. } => {
            out.push(ContentBlock::Image(ImageBlock {
                bounds,
                resource_id: resource_id.clone(),
                alt_text: shape.alt_text.clone(),
                format: None,
                original_size: None,
                style: shape_style,
                rotation: f64::from(shape.rotation_deg),
            }));
        }
        ShapeKind::Group { children, .. } => {
            for child in children {
                convert_shape_to_udm(pres, slide, child, layout, master, theme, out);
            }
        }
        ShapeKind::Table(tbl) => {
            out.push(ContentBlock::Table(convert_table_to_udm(
                tbl,
                bounds,
                slide.number,
                theme,
            )));
        }
        ShapeKind::Chart(chart) => {
            if let Some(ref title) = chart.title {
                out.push(ContentBlock::Text(TextBlock {
                    runs: vec![TextRun::new(title)],
                    paragraph_style: None,
                    bounds,
                    vertical_alignment: None,
                    style: shape_style,
                    rotation: 0.0,
                }));
            }
        }
        ShapeKind::SmartArt {
            shapes,
            fallback_items,
        } => {
            for s in shapes {
                convert_shape_to_udm(pres, slide, s, layout, master, theme, out);
            }
            if shapes.is_empty() && !fallback_items.is_empty() {
                out.push(ContentBlock::Text(TextBlock {
                    runs: vec![TextRun::new(fallback_items.join(" • "))],
                    paragraph_style: None,
                    bounds,
                    vertical_alignment: None,
                    style: shape_style,
                    rotation: 0.0,
                }));
            }
        }
        ShapeKind::MediaPoster { label, .. } => {
            out.push(ContentBlock::Text(TextBlock {
                runs: vec![TextRun::new(format!("[{label}]"))],
                paragraph_style: None,
                bounds,
                vertical_alignment: None,
                style: shape_style,
                rotation: 0.0,
            }));
        }
        ShapeKind::Connector { .. } => {}
    }
}

fn convert_text_body_runs(tb: &TextBody, slide_num: u32, theme: &DrawingTheme) -> Vec<TextRun> {
    let mut runs = Vec::new();
    for (p_idx, para) in tb.paragraphs.iter().enumerate() {
        for r in &para.runs {
            let txt = if r.field_type.as_deref() == Some("slidenum") {
                slide_num.to_string()
            } else {
                r.text.clone()
            };
            if txt.is_empty() {
                continue;
            }
            runs.push(TextRun {
                text: txt,
                style: TextStyle {
                    font_family: Some(
                        r.style
                            .font_family
                            .clone()
                            .unwrap_or_else(|| theme.minor_latin_font.clone()),
                    ),
                    font_size: Some(f64::from(r.style.font_size_pt.unwrap_or(18.0))),
                    bold: r.style.bold.unwrap_or(false),
                    italic: r.style.italic.unwrap_or(false),
                    underline: r.style.underline.unwrap_or(false),
                    strikethrough: r.style.strikethrough.unwrap_or(false),
                    color: r.style.color.map(prism_layout::Rgba::to_hex_rgb),
                    ..TextStyle::default()
                },
                bounds: None,
                char_positions: None,
            });
        }
        if p_idx + 1 < tb.paragraphs.len()
            && !runs.is_empty()
            && let Some(last) = runs.last_mut()
            && !last.text.ends_with('\n')
        {
            last.text.push('\n');
        }
    }
    runs
}

fn convert_table_to_udm(
    tbl: &SlideTable,
    bounds: Rect,
    slide_num: u32,
    theme: &DrawingTheme,
) -> TableBlock {
    let mut rows = Vec::new();
    let mut max_cols = tbl.col_widths_pt.len();
    for row in &tbl.rows {
        let mut cells = Vec::new();
        for cell in &row.cells {
            if cell.h_merge || cell.v_merge {
                continue;
            }
            let mut content = Vec::new();
            if let Some(ref tb) = cell.text_body {
                let runs = convert_text_body_runs(tb, slide_num, theme);
                if !runs.is_empty() {
                    content.push(ContentBlock::Text(TextBlock {
                        runs,
                        paragraph_style: None,
                        bounds: Rect::default(),
                        vertical_alignment: None,
                        style: ShapeStyle::default(),
                        rotation: 0.0,
                    }));
                }
            }
            cells.push(TableCell {
                content,
                col_span: usize::from(cell.grid_span.max(1)),
                row_span: usize::from(cell.row_span.max(1)),
                background_color: match cell.fill {
                    ShapeFill::Solid(c) => Some(c.to_hex_rgb()),
                    _ => None,
                },
                borders: None,
            });
        }
        max_cols = max_cols.max(cells.len());
        rows.push(TableRow {
            cells,
            height: Some(f64::from(row.height_pt)),
        });
    }
    TableBlock {
        bounds,
        rows,
        column_count: max_cols,
        column_widths: tbl.col_widths_pt.iter().map(|&w| f64::from(w)).collect(),
        style: ShapeStyle::default(),
        rotation: 0.0,
    }
}
