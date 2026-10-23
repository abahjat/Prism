// SPDX-License-Identifier: AGPL-3.0-only
//! Rich-text paragraph shaping and layout engine built on `parley` and [`FontSystem`].

use crate::fonts::FontSystem;
use crate::model::{
    Affine, Cluster, Decorations, FontId, GlyphRun, LineDecoration, Point, PositionedGlyph, Rgba,
    SourceRef, normalize_zero,
};
use parley::LayoutContext;
use parley::layout::Alignment;
use parley::style::{FontFamily, FontStyle, FontWeight, LineHeight, StyleProperty};

/// Horizontal paragraph alignment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextAlign {
    /// Align text to the left edge (or start edge).
    #[default]
    Left,
    /// Center text horizontally.
    Center,
    /// Align text to the right edge (or end edge).
    Right,
    /// Justify text across the available line width (except the last line).
    Justify,
}

/// Vertical baseline script offset for superscript or subscript runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScriptPosition {
    /// Normal baseline.
    #[default]
    Normal,
    /// Raised baseline with reduced font size (`0.65×` size, `-0.35×` shift).
    Superscript,
    /// Lowered baseline with reduced font size (`0.65×` size, `+0.20×` shift).
    Subscript,
}

/// Line spacing rule for a paragraph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineSpacing {
    /// Proportional multiple of the font's natural line height (e.g. `1.15`).
    Multiple(f32),
    /// Exact line height in points.
    ExactPt(f32),
    /// Minimum line height in points.
    AtLeastPt(f32),
}

impl Default for LineSpacing {
    fn default() -> Self {
        Self::Multiple(1.15)
    }
}

/// Specification of a single styled text span within a paragraph.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct SpanSpec {
    /// UTF-8 text of the span.
    pub text: String,
    /// Requested font family name (e.g. `"Calibri"`, `"Arial"`).
    pub family: String,
    /// Font size in points.
    pub size: f32,
    /// Bold weight flag.
    pub bold: bool,
    /// Italic style flag.
    pub italic: bool,
    /// Fill color of the glyphs.
    pub color: Rgba,
    /// Underline flag.
    pub underline: bool,
    /// Strikethrough flag.
    pub strikethrough: bool,
    /// Optional background highlight color.
    pub highlight: Option<Rgba>,
    /// Superscript / subscript positioning.
    pub script: ScriptPosition,
    /// Originating UDM run index.
    pub run_index: u32,
}

impl Default for SpanSpec {
    fn default() -> Self {
        Self {
            text: String::new(),
            family: "Calibri".to_string(),
            size: 11.0,
            bold: false,
            italic: false,
            color: Rgba::BLACK,
            underline: false,
            strikethrough: false,
            highlight: None,
            script: ScriptPosition::Normal,
            run_index: 0,
        }
    }
}

/// Specification of a paragraph to be shaped and broken into lines.
#[derive(Debug, Clone, Default)]
pub struct ParagraphSpec {
    /// Styled spans comprising the paragraph text.
    pub spans: Vec<SpanSpec>,
    /// Horizontal text alignment.
    pub align: TextAlign,
    /// Line spacing rule.
    pub line_spacing: LineSpacing,
    /// Maximum wrapping width in points (`None` for unbounded single-line layout).
    pub max_width: Option<f32>,
    /// First-line indent in points (kept for paragraph layout integration).
    pub first_line_indent: f32,
    /// Top-left origin `(x, y)` of the paragraph box in page/group coordinates.
    pub origin: Point,
    /// Originating UDM page index.
    pub page_index: u32,
    /// Originating UDM content block index.
    pub block_index: u32,
}

/// Result of shaping and line-breaking a [`ParagraphSpec`].
#[derive(Debug, Clone, Default)]
pub struct ShapedParagraph {
    /// Positioned [`GlyphRun`]s for all lines in the paragraph.
    pub runs: Vec<GlyphRun>,
    /// Total height of the shaped paragraph in points.
    pub height: f32,
    /// Maximum line width encountered in points.
    pub width: f32,
    /// Number of broken lines.
    pub line_count: usize,
}

/// Stateful text layout engine wrapping `parley::LayoutContext`.
pub struct TextLayoutEngine {
    layout_cx: LayoutContext<usize>,
}

impl Default for TextLayoutEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextLayoutEngine {
    /// Create a new [`TextLayoutEngine`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            layout_cx: LayoutContext::new(),
        }
    }

    /// Shape and line-break a [`ParagraphSpec`] using `font_sys`, producing positioned [`GlyphRun`]s.
    #[allow(clippy::too_many_lines)]
    pub fn layout_paragraph(
        &mut self,
        spec: &ParagraphSpec,
        font_sys: &mut FontSystem,
    ) -> ShapedParagraph {
        if spec.spans.is_empty() || spec.spans.iter().all(|s| s.text.is_empty()) {
            return ShapedParagraph::default();
        }

        // Build full paragraph text and resolve font families up front.
        let mut full_text = String::new();
        let mut span_ranges = Vec::with_capacity(spec.spans.len());
        let mut resolved_families = Vec::with_capacity(spec.spans.len());

        for span in &spec.spans {
            let start = full_text.len();
            full_text.push_str(&span.text);
            let end = full_text.len();
            span_ranges.push(start..end);
            resolved_families.push(font_sys.resolve_family(&span.family));
        }

        let mut builder =
            self.layout_cx
                .ranged_builder(&mut font_sys.font_cx, &full_text, 1.0, false);

        for (idx, span) in spec.spans.iter().enumerate() {
            let range = span_ranges[idx].clone();
            if range.is_empty() {
                continue;
            }
            let base_size = span.size.max(1.0);
            let effective_size = match span.script {
                ScriptPosition::Normal => base_size,
                ScriptPosition::Superscript | ScriptPosition::Subscript => base_size * 0.65,
            };

            builder.push(StyleProperty::Brush(idx), range.clone());
            builder.push(
                StyleProperty::FontFamily(FontFamily::named(&resolved_families[idx])),
                range.clone(),
            );
            builder.push(StyleProperty::FontSize(effective_size), range.clone());
            builder.push(
                StyleProperty::FontWeight(if span.bold {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                }),
                range.clone(),
            );
            builder.push(
                StyleProperty::FontStyle(if span.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                }),
                range.clone(),
            );

            let lh = match spec.line_spacing {
                LineSpacing::Multiple(m) => LineHeight::FontSizeRelative(m.max(0.5)),
                LineSpacing::ExactPt(pt) => LineHeight::Absolute(pt.max(1.0)),
                LineSpacing::AtLeastPt(pt) => {
                    LineHeight::Absolute(pt.max(base_size * 1.15).max(1.0))
                }
            };
            builder.push(StyleProperty::LineHeight(lh), range);
        }

        let mut layout = builder.build(&full_text);
        let max_advance = spec.max_width.map(|w| w.max(1.0));
        layout.break_all_lines(max_advance);

        let alignment = match spec.align {
            TextAlign::Left => Alignment::Start,
            TextAlign::Center => Alignment::Center,
            TextAlign::Right => Alignment::End,
            TextAlign::Justify => Alignment::Justify,
        };
        layout.align(alignment, parley::AlignmentOptions::default());

        let styles = layout.styles();
        let mut out_runs = Vec::new();
        let mut line_count = 0usize;

        for line in layout.lines() {
            line_count += 1;
            let line_metrics = line.metrics();
            let mut cursor_x = line_metrics.offset;

            for parley_run in line.runs() {
                let font_data = parley_run.font();

                // Group consecutive visual clusters that share the same span brush index
                let mut pending_span_idx: Option<usize> = None;
                let mut pending_font_id = FontId(0);
                let mut run_start_x = cursor_x;
                let mut run_text = String::new();
                let mut run_glyphs: Vec<PositionedGlyph> = Vec::new();
                let mut run_clusters: Vec<Cluster> = Vec::new();

                for cluster in parley_run.visual_clusters() {
                    let style_idx = usize::from(cluster.style_index());
                    let span_idx = styles
                        .get(style_idx)
                        .map_or(0, |s| s.brush)
                        .min(spec.spans.len() - 1);
                    let span = &spec.spans[span_idx];
                    let resolved_fam = &resolved_families[span_idx];
                    let font_id = font_sys.font_id_for_parley_font(
                        font_data,
                        resolved_fam,
                        span.bold,
                        span.italic,
                    );

                    if pending_span_idx != Some(span_idx) || pending_font_id != font_id {
                        if let Some(prev_idx) = pending_span_idx
                            && (!run_glyphs.is_empty() || !run_text.is_empty())
                        {
                            out_runs.push(build_glyph_run(
                                spec,
                                &spec.spans[prev_idx],
                                pending_font_id,
                                run_start_x,
                                line_metrics.baseline,
                                std::mem::take(&mut run_text),
                                std::mem::take(&mut run_glyphs),
                                std::mem::take(&mut run_clusters),
                            ));
                        }
                        pending_span_idx = Some(span_idx);
                        pending_font_id = font_id;
                        run_start_x = cursor_x;
                    }

                    let text_range = cluster.text_range();
                    let cluster_str = full_text.get(text_range).unwrap_or("");
                    #[allow(clippy::cast_possible_truncation)]
                    let local_text_start = run_text.len() as u32;
                    run_text.push_str(cluster_str);
                    #[allow(clippy::cast_possible_truncation)]
                    let local_text_end = run_text.len() as u32;

                    let local_cluster_x = cursor_x - run_start_x;
                    #[allow(clippy::cast_possible_truncation)]
                    let glyph_start = run_glyphs.len() as u32;

                    let mut glyph_pen_x = local_cluster_x;
                    for g in cluster.glyphs() {
                        #[allow(clippy::cast_possible_truncation)]
                        let gid = g.id as u16;
                        run_glyphs.push(PositionedGlyph {
                            glyph_id: gid,
                            x: normalize_zero(glyph_pen_x + g.x),
                            y: normalize_zero(-g.y),
                            advance: normalize_zero(g.advance),
                        });
                        glyph_pen_x += g.advance;
                    }

                    #[allow(clippy::cast_possible_truncation)]
                    let glyph_end = run_glyphs.len() as u32;
                    let cluster_adv = cluster.advance();

                    run_clusters.push(Cluster {
                        text_range: local_text_start..local_text_end,
                        glyph_range: glyph_start..glyph_end,
                        x: normalize_zero(local_cluster_x),
                        advance: normalize_zero(cluster_adv),
                    });

                    cursor_x += cluster_adv;
                }

                if let Some(prev_idx) = pending_span_idx
                    && (!run_glyphs.is_empty() || !run_text.is_empty())
                {
                    out_runs.push(build_glyph_run(
                        spec,
                        &spec.spans[prev_idx],
                        pending_font_id,
                        run_start_x,
                        line_metrics.baseline,
                        run_text,
                        run_glyphs,
                        run_clusters,
                    ));
                }
            }
        }

        ShapedParagraph {
            runs: out_runs,
            height: layout.height(),
            width: layout.width(),
            line_count,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_glyph_run(
    paragraph: &ParagraphSpec,
    span: &SpanSpec,
    font_id: FontId,
    run_start_x: f32,
    line_baseline_y: f32,
    text: String,
    glyphs: Vec<PositionedGlyph>,
    clusters: Vec<Cluster>,
) -> GlyphRun {
    let base_size = span.size.max(1.0);
    let (effective_size, baseline_shift) = match span.script {
        ScriptPosition::Normal => (base_size, 0.0),
        ScriptPosition::Superscript => (base_size * 0.65, -base_size * 0.35),
        ScriptPosition::Subscript => (base_size * 0.65, base_size * 0.20),
    };

    let thickness = (effective_size * 0.06).max(0.5);
    let decorations = Decorations {
        underline: span.underline.then_some(LineDecoration {
            color: span.color,
            offset: effective_size * 0.12,
            thickness,
        }),
        strikethrough: span.strikethrough.then_some(LineDecoration {
            color: span.color,
            offset: -effective_size * 0.30,
            thickness,
        }),
        highlight: span.highlight,
    };

    let tx = normalize_zero(paragraph.origin.x + run_start_x);
    let ty = normalize_zero(paragraph.origin.y + line_baseline_y + baseline_shift);

    GlyphRun {
        font: font_id,
        size: effective_size,
        color: span.color,
        transform: Affine::translate(tx, ty),
        glyphs,
        text,
        clusters,
        decorations,
        source: SourceRef::new(paragraph.page_index, paragraph.block_index, span.run_index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, LayoutDocument, LayoutOptions, LayoutPage, Size};
    use crate::text_ops::SearchOptions;

    #[test]
    fn test_layout_paragraph_shapes_and_extracts_text() {
        let mut font_sys = FontSystem::new(&LayoutOptions::default());
        let mut engine = TextLayoutEngine::new();

        let spec = ParagraphSpec {
            spans: vec![
                SpanSpec {
                    text: "Hello ".to_string(),
                    family: "Calibri".to_string(),
                    size: 12.0,
                    bold: true,
                    color: Rgba::rgb(0x1F, 0x4E, 0x79),
                    run_index: 0,
                    ..SpanSpec::default()
                },
                SpanSpec {
                    text: "Prism Layout Engine!".to_string(),
                    family: "Calibri".to_string(),
                    size: 12.0,
                    italic: true,
                    underline: true,
                    run_index: 1,
                    ..SpanSpec::default()
                },
            ],
            align: TextAlign::Left,
            line_spacing: LineSpacing::Multiple(1.2),
            max_width: Some(300.0),
            first_line_indent: 0.0,
            origin: Point::new(72.0, 72.0),
            page_index: 0,
            block_index: 0,
        };

        let shaped = engine.layout_paragraph(&spec, &mut font_sys);
        assert_eq!(shaped.line_count, 1);
        assert!(shaped.runs.len() >= 2);
        assert!(shaped.height > 10.0);

        let mut page = LayoutPage::new(0, Size::LETTER);
        for run in shaped.runs {
            page.items.push(Item::Glyphs(run));
        }

        assert_eq!(page.text(), "Hello Prism Layout Engine!");

        let doc = LayoutDocument {
            pages: vec![page],
            fonts: font_sys.store,
            images: crate::model::ImageStore::new(),
            font_report: font_sys.report,
        };

        let hits = doc.find("prism layout", SearchOptions::default());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "Prism Layout");
        assert_eq!(hits[0].rects.len(), 1);
        assert!(doc.font_report.entries.contains_key("Calibri"));
    }
}
