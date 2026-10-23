// SPDX-License-Identifier: AGPL-3.0-only
//! Pure-Rust PDF export (`krilla`) for [`LayoutDocument`].
//!
//! Exports any [`LayoutDocument`] (whether originating from PPTX, DOCX, TXT, or PDF)
//! into a deterministic, searchable PDF 1.7 document with subsetted fonts, vector paths,
//! clips, gradients, images, and link annotations.

use krilla::action::{Action, LinkAction};
use krilla::annotation::{Annotation, LinkAnnotation, Target};
use krilla::color::rgb;
use krilla::geom::{
    Path as KrillaPath, PathBuilder as KrillaPathBuilder, Point as KrillaPoint, Rect as KrillaRect,
    Size as KrillaSize, Transform as KrillaTransform,
};
use krilla::image::Image as KrillaImage;
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{
    Fill as KrillaFill, FillRule as KrillaFillRule, LineCap as KrillaLineCap,
    LineJoin as KrillaLineJoin, LinearGradient as KrillaLinearGradient,
    RadialGradient as KrillaRadialGradient, SpreadMethod, Stop as KrillaStop,
    Stroke as KrillaStroke, StrokeDash as KrillaStrokeDash,
};
use krilla::surface::Surface;
use krilla::text::{Font as KrillaFont, GlyphId as KrillaGlyphId, KrillaGlyph};
use krilla::{Data as KrillaData, Document as KrillaDocument, SerializeSettings};
use prism_core::error::{Error, Result};
use prism_layout::{
    Affine, FillPaint, FillRule, FontId, FontStore, GlyphRun, GroupItem, ImageId, ImageItem, Item,
    LayoutDocument, LayoutOptions, LineCap, LineDecoration, LineJoin, NORMALIZED_UPEM, PathCommand,
    PathItem, Rect, Rgba, StrokeStyle,
};
use std::collections::BTreeMap;
use std::sync::Arc;

const NOTO_SANS_FALLBACK_TTF: &[u8] = include_bytes!("../../../assets/fonts/NotoSans-Regular.ttf");

/// Options controlling PDF export from [`LayoutDocument`].
#[derive(Debug, Clone)]
pub struct PdfExportOptions {
    /// Compress PDF content streams with Deflate (default `true`).
    pub compress_streams: bool,
    /// Resource limits enforced during export.
    pub layout_options: LayoutOptions,
}

impl Default for PdfExportOptions {
    fn default() -> Self {
        Self {
            compress_streams: true,
            layout_options: LayoutOptions::default(),
        }
    }
}

struct PdfExportState<'a> {
    fonts: &'a FontStore,
    options: &'a PdfExportOptions,
    krilla_fonts: BTreeMap<FontId, KrillaFont>,
    fallback_font: Option<KrillaFont>,
    krilla_images: BTreeMap<ImageId, KrillaImage>,
    pending_links: Vec<(Rect, String)>,
    glyph_count: usize,
}

/// Export a [`LayoutDocument`] to a deterministic, searchable PDF byte vector.
///
/// # Errors
///
/// Returns an [`Error`] if resource limits (`max_pages`, `max_glyphs_per_page`, `max_recursion_depth`)
/// are exceeded or if `krilla` serialization fails.
pub fn export_document_pdf(doc: &LayoutDocument, options: &PdfExportOptions) -> Result<Vec<u8>> {
    if doc.pages.len() > options.layout_options.max_pages {
        return Err(Error::InvalidInput(format!(
            "Document page count ({}) exceeds max_pages ({})",
            doc.pages.len(),
            options.layout_options.max_pages
        )));
    }

    let settings = SerializeSettings {
        pretty: false,
        compress_content_streams: options.compress_streams,
        no_device_cs: false,
        ascii_compatible: false,
        xmp_metadata: false,
        cmyk_profile: None,
        enable_tagging: false,
        ..SerializeSettings::default()
    };

    let mut krilla_doc = KrillaDocument::new_with(settings);

    let mut krilla_fonts = BTreeMap::new();
    for (&font_id, entry) in &doc.fonts.fonts {
        if !entry.data.is_empty() {
            let data: KrillaData = Arc::new(entry.data.clone()).into();
            if let Some(kf) = KrillaFont::new(data, entry.face_index) {
                krilla_fonts.insert(font_id, kf);
            }
        }
    }

    let fallback_data: KrillaData = Arc::new(NOTO_SANS_FALLBACK_TTF.to_vec()).into();
    let fallback_font = KrillaFont::new(fallback_data, 0);

    let mut krilla_images = BTreeMap::new();
    for (&img_id, entry) in &doc.images.images {
        if let Some(ki) = load_krilla_image(entry) {
            krilla_images.insert(img_id, ki);
        }
    }

    let mut state = PdfExportState {
        fonts: &doc.fonts,
        options,
        krilla_fonts,
        fallback_font,
        krilla_images,
        pending_links: Vec::new(),
        glyph_count: 0,
    };

    for layout_page in &doc.pages {
        let w = layout_page.size.width.max(1.0);
        let h = layout_page.size.height.max(1.0);
        let page_settings = PageSettings::from_wh(w, h)
            .ok_or_else(|| Error::RenderError(format!("Invalid PDF page dimensions: {w}x{h}")))?;

        state.pending_links.clear();
        state.glyph_count = 0;

        let mut pdf_page = krilla_doc.start_page_with(page_settings);
        {
            let mut surface = pdf_page.surface();
            write_items(
                &layout_page.items,
                Affine::IDENTITY,
                0,
                &mut state,
                &mut surface,
            )?;
            surface.finish();
        }

        for (rect, target) in state.pending_links.drain(..) {
            if let Some(k_rect) =
                KrillaRect::from_xywh(rect.x, rect.y, rect.width.max(1.0), rect.height.max(1.0))
            {
                let link = LinkAnnotation::new(
                    k_rect,
                    Target::Action(Action::Link(LinkAction::new(target.clone()))),
                );
                pdf_page.add_annotation(Annotation::new_link(link, Some(target)));
            }
        }

        pdf_page.finish();
    }

    let pdf_bytes = krilla_doc
        .finish()
        .map_err(|e| Error::RenderError(format!("PDF serialization failed: {e:?}")))?;
    Ok(pdf_bytes)
}

fn write_items(
    items: &[Item],
    world_transform: Affine,
    depth: u32,
    state: &mut PdfExportState<'_>,
    surface: &mut Surface<'_>,
) -> Result<()> {
    if depth > state.options.layout_options.max_recursion_depth {
        return Err(Error::InvalidInput(format!(
            "Group nesting depth ({depth}) exceeds max_recursion_depth ({})",
            state.options.layout_options.max_recursion_depth
        )));
    }

    for item in items {
        match item {
            Item::Glyphs(run) => write_glyph_run(run, state, surface)?,
            Item::Path(path_item) => write_path_item(path_item, surface),
            Item::Image(img_item) => write_image_item(img_item, state, surface),
            Item::Group(group) => write_group_item(group, world_transform, depth, state, surface)?,
            Item::Link(link) => {
                let p0 = world_transform
                    .transform_point(prism_layout::Point::new(link.rect.x, link.rect.y));
                let p1 = world_transform.transform_point(prism_layout::Point::new(
                    link.rect.right(),
                    link.rect.bottom(),
                ));
                let r = Rect::new(
                    p0.x.min(p1.x),
                    p0.y.min(p1.y),
                    (p0.x - p1.x).abs(),
                    (p0.y - p1.y).abs(),
                );
                state.pending_links.push((r, link.target.clone()));
            }
            Item::Anchor(_) => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn write_glyph_run(
    run: &GlyphRun,
    state: &mut PdfExportState<'_>,
    surface: &mut Surface<'_>,
) -> Result<()> {
    state.glyph_count = state.glyph_count.saturating_add(run.glyphs.len());
    if state.glyph_count > state.options.layout_options.max_glyphs_per_page {
        return Err(Error::InvalidInput(format!(
            "PDF export glyph count ({}) exceeds max_glyphs_per_page ({})",
            state.glyph_count, state.options.layout_options.max_glyphs_per_page
        )));
    }

    if run.glyphs.is_empty()
        && run.text.is_empty()
        && run.decorations.highlight.is_none()
        && run.decorations.underline.is_none()
        && run.decorations.strikethrough.is_none()
    {
        return Ok(());
    }

    let has_transform = !run.transform.is_identity();
    if has_transform {
        surface.push_transform(&to_krilla_transform(run.transform));
    }

    let run_advance = run
        .glyphs
        .iter()
        .fold(0.0_f32, |acc, g| acc.max(g.x + g.advance))
        .max(
            run.clusters
                .iter()
                .fold(0.0_f32, |acc, c| acc.max(c.x + c.advance)),
        );

    if let Some(hl) = run.decorations.highlight
        && run_advance > 0.0
        && hl.a > 0
    {
        let ascent = run.size * 0.8;
        if let Some(rect_path) = build_rect_path(Rect::new(0.0, -ascent, run_advance, run.size)) {
            surface.set_fill(Some(rgba_to_krilla_fill(hl, FillRule::NonZero)));
            surface.set_stroke(None);
            surface.draw_path(&rect_path);
        }
    }

    let font_size = run.size.max(0.5);
    if let Some(krilla_font) = state.krilla_fonts.get(&run.font).cloned()
        && !run.glyphs.is_empty()
    {
        let k_glyphs = build_krilla_glyphs(run, font_size);
        surface.set_fill(Some(rgba_to_krilla_fill(run.color, FillRule::NonZero)));
        surface.set_stroke(None);
        surface.draw_glyphs(
            KrillaPoint::from_xy(0.0, 0.0),
            &k_glyphs,
            krilla_font,
            &run.text,
            font_size,
            false,
        );
    } else {
        // Outline fallback for Type1/CFF fonts without raw TrueType tables
        if run.color.a > 0 {
            let scale = font_size / NORMALIZED_UPEM;
            surface.set_fill(Some(rgba_to_krilla_fill(run.color, FillRule::NonZero)));
            surface.set_stroke(None);
            for g in &run.glyphs {
                if let Some(cmds) = state.fonts.glyph_outline(run.font, g.glyph_id)
                    && let Some(path) = build_krilla_path(&cmds)
                {
                    let gt = Affine::translate(g.x, g.y).then(Affine::scale(scale));
                    surface.push_transform(&to_krilla_transform(gt));
                    surface.draw_path(&path);
                    surface.pop();
                }
            }
        }

        // Emit invisible searchable text layer when `run.text` is non-empty
        if !run.text.is_empty()
            && let Some(ref fb_font) = state.fallback_font
        {
            let k_glyphs = vec![KrillaGlyph::new(
                KrillaGlyphId::new(0),
                (run_advance / font_size).max(0.5),
                0.0,
                0.0,
                0.0,
                0..run.text.len(),
                None,
            )];
            surface.set_fill(Some(rgba_to_krilla_fill(
                Rgba::TRANSPARENT,
                FillRule::NonZero,
            )));
            surface.set_stroke(None);
            surface.draw_glyphs(
                KrillaPoint::from_xy(0.0, 0.0),
                &k_glyphs,
                fb_font.clone(),
                &run.text,
                font_size,
                false,
            );
        }
    }

    if let Some(ul) = run.decorations.underline
        && run_advance > 0.0
    {
        write_line_decoration(ul, run_advance, surface);
    }
    if let Some(st) = run.decorations.strikethrough
        && run_advance > 0.0
    {
        write_line_decoration(st, run_advance, surface);
    }

    if has_transform {
        surface.pop();
    }

    Ok(())
}

fn build_krilla_glyphs(run: &GlyphRun, font_size: f32) -> Vec<KrillaGlyph> {
    // Map each glyph index to its cluster's UTF-8 byte range in `run.text`
    let mut glyph_text_ranges = vec![0..0; run.glyphs.len()];
    for cluster in &run.clusters {
        let g_start = (cluster.glyph_range.start as usize).min(run.glyphs.len());
        let g_end = (cluster.glyph_range.end as usize).min(run.glyphs.len());
        let t_start = (cluster.text_range.start as usize).min(run.text.len());
        let t_end = (cluster.text_range.end as usize).min(run.text.len());
        if g_start < g_end && t_start <= t_end {
            // Assign the cluster's text range to the first glyph in the cluster
            glyph_text_ranges[g_start] = t_start..t_end;
        }
    }

    let mut cur_x_pt = 0.0_f32;
    let mut out = Vec::with_capacity(run.glyphs.len());
    for (idx, g) in run.glyphs.iter().enumerate() {
        let x_offset_em = (g.x - cur_x_pt) / font_size;
        let y_offset_em = -g.y / font_size;
        let x_advance_em = g.advance / font_size;
        cur_x_pt += g.advance;

        out.push(KrillaGlyph::new(
            KrillaGlyphId::new(u32::from(g.glyph_id)),
            x_advance_em,
            x_offset_em,
            y_offset_em,
            0.0,
            glyph_text_ranges[idx].clone(),
            None,
        ));
    }
    out
}

fn write_line_decoration(dec: LineDecoration, width: f32, surface: &mut Surface<'_>) {
    if dec.color.a == 0 || dec.thickness <= 0.0 {
        return;
    }
    let mut pb = KrillaPathBuilder::new();
    pb.move_to(0.0, dec.offset);
    pb.line_to(width, dec.offset);
    if let Some(path) = pb.finish() {
        surface.set_fill(None);
        surface.set_stroke(Some(KrillaStroke {
            paint: rgb::Color::new(dec.color.r, dec.color.g, dec.color.b).into(),
            width: dec.thickness,
            opacity: norm_f32(dec.color.opacity()),
            ..KrillaStroke::default()
        }));
        surface.draw_path(&path);
    }
}

fn write_path_item(path_item: &PathItem, surface: &mut Surface<'_>) {
    let Some(path) = build_krilla_path(&path_item.commands) else {
        return;
    };

    let fill = path_item
        .fill
        .as_ref()
        .and_then(|fp| convert_fill_paint(fp, path_item.fill_rule));
    let stroke = path_item
        .stroke
        .as_ref()
        .filter(|s| s.width > 0.0 && s.color.a > 0)
        .map(convert_stroke_style);

    if fill.is_none() && stroke.is_none() {
        return;
    }

    let has_transform = !path_item.transform.is_identity();
    if has_transform {
        surface.push_transform(&to_krilla_transform(path_item.transform));
    }

    surface.set_fill(fill);
    surface.set_stroke(stroke);
    surface.draw_path(&path);

    if has_transform {
        surface.pop();
    }
}

fn write_image_item(img_item: &ImageItem, state: &PdfExportState<'_>, surface: &mut Surface<'_>) {
    if img_item.rect.width <= 0.0 || img_item.rect.height <= 0.0 || img_item.opacity <= 0.0 {
        return;
    }
    let Some(k_image) = state.krilla_images.get(&img_item.image).cloned() else {
        return;
    };
    let Some(k_size) = KrillaSize::from_wh(img_item.rect.width, img_item.rect.height) else {
        return;
    };

    let mut pushes = 0usize;
    if !img_item.transform.is_identity() {
        surface.push_transform(&to_krilla_transform(img_item.transform));
        pushes += 1;
    }
    if img_item.rect.x.abs() > 1e-5 || img_item.rect.y.abs() > 1e-5 {
        surface.push_transform(&KrillaTransform::from_translate(
            img_item.rect.x,
            img_item.rect.y,
        ));
        pushes += 1;
    }
    if img_item.opacity < 1.0 - 1e-4 {
        surface.push_opacity(norm_f32(img_item.opacity));
        pushes += 1;
    }

    surface.draw_image(k_image, k_size);

    for _ in 0..pushes {
        surface.pop();
    }
}

fn write_group_item(
    group: &GroupItem,
    parent_world: Affine,
    depth: u32,
    state: &mut PdfExportState<'_>,
    surface: &mut Surface<'_>,
) -> Result<()> {
    if group.children.is_empty() || group.opacity <= 0.0 {
        return Ok(());
    }

    let mut pushes = 0usize;
    if !group.transform.is_identity() {
        surface.push_transform(&to_krilla_transform(group.transform));
        pushes += 1;
    }
    if let Some(ref clip) = group.clip
        && let Some(clip_path) = build_krilla_path(&clip.commands)
    {
        surface.push_clip_path(&clip_path, &convert_fill_rule(clip.fill_rule));
        pushes += 1;
    }
    if group.opacity < 1.0 - 1e-4 {
        surface.push_opacity(norm_f32(group.opacity));
        pushes += 1;
    }

    let next_world = parent_world.then(group.transform);
    let res = write_items(&group.children, next_world, depth + 1, state, surface);

    for _ in 0..pushes {
        surface.pop();
    }

    res
}

fn load_krilla_image(entry: &prism_layout::ImageEntry) -> Option<KrillaImage> {
    if entry.mime_type == "image/x-rgba8" {
        let expected = (entry.width as usize)
            .checked_mul(entry.height as usize)?
            .checked_mul(4)?;
        if entry.data.len() == expected && entry.width > 0 && entry.height > 0 {
            return Some(KrillaImage::from_rgba8(
                entry.data.clone(),
                entry.width,
                entry.height,
            ));
        }
        return None;
    }

    let data: KrillaData = Arc::new(entry.data.clone()).into();
    if let Ok(img) = KrillaImage::from_png(data.clone(), true) {
        return Some(img);
    }
    if let Ok(img) = KrillaImage::from_jpeg(data.clone(), true) {
        return Some(img);
    }
    if let Ok(img) = KrillaImage::from_webp(data.clone(), true) {
        return Some(img);
    }
    if let Ok(img) = KrillaImage::from_gif(data, true) {
        return Some(img);
    }

    // Fallback: decode via `image` crate into RGBA8
    let decoded = image::load_from_memory(&entry.data).ok()?.to_rgba8();
    let (w, h) = decoded.dimensions();
    if w > 0 && h > 0 {
        Some(KrillaImage::from_rgba8(decoded.into_raw(), w, h))
    } else {
        None
    }
}

fn convert_fill_paint(paint: &FillPaint, rule: FillRule) -> Option<KrillaFill> {
    let k_rule = convert_fill_rule(rule);
    match paint {
        FillPaint::Solid(c) => {
            if c.a == 0 {
                return None;
            }
            Some(rgba_to_krilla_fill(*c, rule))
        }
        FillPaint::LinearGradient(grad) => {
            if grad.stops.is_empty() {
                return None;
            }
            let stops = grad
                .stops
                .iter()
                .map(|s| KrillaStop {
                    offset: norm_f32(s.offset),
                    color: rgb::Color::new(s.color.r, s.color.g, s.color.b).into(),
                    opacity: norm_f32(s.color.opacity()),
                })
                .collect();
            let lg = KrillaLinearGradient {
                x1: grad.start.x,
                y1: grad.start.y,
                x2: grad.end.x,
                y2: grad.end.y,
                transform: KrillaTransform::identity(),
                spread_method: SpreadMethod::Pad,
                stops,
                anti_alias: true,
            };
            Some(KrillaFill {
                paint: lg.into(),
                opacity: NormalizedF32::ONE,
                rule: k_rule,
            })
        }
        FillPaint::RadialGradient(grad) => {
            if grad.stops.is_empty() {
                return None;
            }
            let stops = grad
                .stops
                .iter()
                .map(|s| KrillaStop {
                    offset: norm_f32(s.offset),
                    color: rgb::Color::new(s.color.r, s.color.g, s.color.b).into(),
                    opacity: norm_f32(s.color.opacity()),
                })
                .collect();
            let rg = KrillaRadialGradient {
                fx: grad.center.x,
                fy: grad.center.y,
                fr: 0.0,
                cx: grad.center.x,
                cy: grad.center.y,
                cr: grad.radius.max(0.1),
                transform: KrillaTransform::identity(),
                spread_method: SpreadMethod::Pad,
                stops,
                anti_alias: true,
            };
            Some(KrillaFill {
                paint: rg.into(),
                opacity: NormalizedF32::ONE,
                rule: k_rule,
            })
        }
    }
}

fn rgba_to_krilla_fill(c: Rgba, rule: FillRule) -> KrillaFill {
    KrillaFill {
        paint: rgb::Color::new(c.r, c.g, c.b).into(),
        opacity: norm_f32(c.opacity()),
        rule: convert_fill_rule(rule),
    }
}

fn convert_fill_rule(rule: FillRule) -> KrillaFillRule {
    match rule {
        FillRule::NonZero => KrillaFillRule::NonZero,
        FillRule::EvenOdd => KrillaFillRule::EvenOdd,
    }
}

fn convert_stroke_style(s: &StrokeStyle) -> KrillaStroke {
    let dash = (!s.dash_array.is_empty()).then(|| KrillaStrokeDash {
        array: s.dash_array.clone(),
        offset: s.dash_offset,
    });
    KrillaStroke {
        paint: rgb::Color::new(s.color.r, s.color.g, s.color.b).into(),
        width: s.width.max(0.1),
        miter_limit: 10.0,
        line_cap: match s.line_cap {
            LineCap::Butt => KrillaLineCap::Butt,
            LineCap::Round => KrillaLineCap::Round,
            LineCap::Square => KrillaLineCap::Square,
        },
        line_join: match s.line_join {
            LineJoin::Miter => KrillaLineJoin::Miter,
            LineJoin::Round => KrillaLineJoin::Round,
            LineJoin::Bevel => KrillaLineJoin::Bevel,
        },
        opacity: norm_f32(s.color.opacity()),
        dash,
    }
}

fn build_krilla_path(commands: &[PathCommand]) -> Option<KrillaPath> {
    if commands.is_empty() {
        return None;
    }
    let mut pb = KrillaPathBuilder::new();
    for cmd in commands {
        match *cmd {
            PathCommand::MoveTo(p) => pb.move_to(p.x, p.y),
            PathCommand::LineTo(p) => pb.line_to(p.x, p.y),
            PathCommand::QuadTo { ctrl, to } => pb.quad_to(ctrl.x, ctrl.y, to.x, to.y),
            PathCommand::CurveTo { ctrl1, ctrl2, to } => {
                pb.cubic_to(ctrl1.x, ctrl1.y, ctrl2.x, ctrl2.y, to.x, to.y);
            }
            PathCommand::Close => pb.close(),
        }
    }
    pb.finish()
}

fn build_rect_path(r: Rect) -> Option<KrillaPath> {
    let mut pb = KrillaPathBuilder::new();
    pb.move_to(r.x, r.y);
    pb.line_to(r.right(), r.y);
    pb.line_to(r.right(), r.bottom());
    pb.line_to(r.x, r.bottom());
    pb.close();
    pb.finish()
}

fn to_krilla_transform(a: Affine) -> KrillaTransform {
    let [c0, c1, c2, c3, c4, c5] = a.coeffs;
    KrillaTransform::from_row(c0, c1, c2, c3, c4, c5)
}

fn norm_f32(val: f32) -> NormalizedF32 {
    NormalizedF32::new(val.clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_layout::{
        FontSystem, ImageStore, LayoutPage, LineSpacing, ParagraphSpec, Point, Size, SpanSpec,
        TextAlign, TextLayoutEngine, interpret_pdf_bytes,
    };

    #[test]
    fn test_pdf_export_round_trip_searchable_and_deterministic() {
        let mut font_sys = FontSystem::new(&LayoutOptions::default());
        let mut engine = TextLayoutEngine::new();

        let spec = ParagraphSpec {
            spans: vec![SpanSpec {
                text: "Searchable PDF Export Round-Trip".to_string(),
                family: "Calibri".to_string(),
                size: 16.0,
                bold: true,
                color: Rgba::rgb(0x1E, 0x29, 0x3B),
                ..SpanSpec::default()
            }],
            align: TextAlign::Left,
            line_spacing: LineSpacing::Multiple(1.2),
            max_width: Some(400.0),
            first_line_indent: 0.0,
            origin: Point::new(50.0, 60.0),
            page_index: 0,
            block_index: 0,
        };
        let shaped = engine.layout_paragraph(&spec, &mut font_sys);

        let mut page = LayoutPage::new(0, Size::LETTER);
        for run in shaped.runs {
            page.items.push(Item::Glyphs(run));
        }

        let doc = LayoutDocument {
            pages: vec![page],
            fonts: font_sys.store,
            images: ImageStore::new(),
            font_report: font_sys.report,
        };

        let opts = PdfExportOptions::default();
        let pdf1 = export_document_pdf(&doc, &opts).expect("export pdf 1");
        let pdf2 = export_document_pdf(&doc, &opts).expect("export pdf 2");
        assert_eq!(
            pdf1, pdf2,
            "Exported PDF must be byte-identical across runs"
        );
        assert!(pdf1.starts_with(b"%PDF-"));

        // Re-interpret the exported PDF with `hayro` and verify text is searchable
        let reparsed =
            interpret_pdf_bytes(&pdf1, &LayoutOptions::default()).expect("re-interpret pdf");
        assert_eq!(reparsed.pages.len(), 1);
        let extracted = reparsed.pages[0].text();
        assert!(
            extracted.contains("Searchable PDF Export Round-Trip"),
            "Expected searchable round-trip text, got {extracted:?}"
        );
    }
}
