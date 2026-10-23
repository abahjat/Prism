// SPDX-License-Identifier: AGPL-3.0-only
//! Native `PowerPoint` presentation layout engine (`Presentation` → `LayoutDocument`).

use crate::model::{
    AutofitMode, BulletKind, PlaceholderRef, PptTextAlign, Presentation, ShapeFill, ShapeKind,
    ShapeStroke, Slide, SlideComment, SlideLayout, SlideMaster, SlideShape, SlideTable, TextBody,
    TextLevelStyle, TextParagraph, TextRunStyle, TextVerticalAnchor,
};
use prism_core::error::{Error, Result};
use prism_drawingml::{
    DrawingTheme, build_custom_geometry_paths, build_preset_geometry_paths, render_chart,
};
use prism_layout::{
    Affine, AnchorItem, FillPaint, FillRule, FontSourceKind, FontSystem, GradientStop, GroupItem,
    ImageId, ImageItem, ImageStore, Item, LayoutDocument, LayoutOptions, LayoutPage, LineCap,
    LineJoin, LineSpacing, LinearGradient, LinkItem, MarkupMode, ParagraphSpec, PathCommand,
    PathItem, Point, Rect, Rgba, ScriptPosition, Size, SpanSpec, StrokeStyle, TextAlign,
    TextLayoutEngine,
};
use prism_word::{is_metafile, render_metafile};
use std::collections::BTreeMap;

/// Lay out a [`Presentation`] into a positioned [`LayoutDocument`].
///
/// # Errors
///
/// Returns an error if resource limits (`max_pages`, `max_image_pixels`) in `options` are exceeded.
#[allow(clippy::too_many_lines)]
pub fn layout_presentation(pres: &Presentation, options: &LayoutOptions) -> Result<LayoutDocument> {
    let mut font_sys = FontSystem::new(options);
    for font_res in pres.fonts.values() {
        if let Some(ref data) = font_res.data {
            font_sys.register_face_bytes(
                &font_res.family,
                400,
                false,
                FontSourceKind::Embedded,
                data.clone(),
                0,
            );
        }
    }

    let mut images = ImageStore::new();
    let mut image_id_map: BTreeMap<String, ImageId> = BTreeMap::new();
    let mut raw_media: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    for (res_key, img_res) in &pres.images {
        if let Some(ref bytes) = img_res.data {
            raw_media.insert(res_key.clone(), bytes.clone());
            if !is_metafile(bytes) {
                let w = img_res.width.max(1);
                let h = img_res.height.max(1);
                if let Ok(id) = images.register(&img_res.mime_type, w, h, bytes.clone(), options) {
                    image_id_map.insert(res_key.clone(), id);
                }
            }
        }
    }

    let mut text_engine = TextLayoutEngine::default();
    let fallback_theme = DrawingTheme::default();
    let mut pages = Vec::new();

    for slide in &pres.slides {
        if slide.hidden && !options.show_hidden_text {
            continue;
        }
        if pages.len() >= options.max_pages {
            // An error rather than a silently shortened deck.
            return Err(Error::InvalidInput(format!(
                "Presentation slide count exceeds max_pages ({})",
                options.max_pages
            )));
        }

        #[allow(clippy::cast_possible_truncation)]
        let page_index = pages.len() as u32;
        let slide_size = Size::new(pres.slide_width_pt, pres.slide_height_pt);
        let mut page = LayoutPage::new(page_index, slide_size);
        page.label = Some(if slide.hidden {
            format!("Slide {} (Hidden)", slide.number)
        } else {
            format!("Slide {}", slide.number)
        });

        let (layout, master, theme) = pres.resolve_slide_hierarchy(slide, &fallback_theme);

        let mut ctx = SlideRenderCtx {
            pres,
            slide,
            layout,
            master,
            theme,
            options,
            font_sys: &mut font_sys,
            text_engine: &mut text_engine,
            image_id_map: &image_id_map,
            raw_media: &raw_media,
            page_index,
            block_counter: 0,
        };

        // 1. Render slide background
        let bg_fill = if slide.background != ShapeFill::Inherited {
            &slide.background
        } else if let Some(l) = layout
            && l.background != ShapeFill::Inherited
        {
            &l.background
        } else if let Some(m) = master
            && m.background != ShapeFill::Inherited
        {
            &m.background
        } else {
            &ShapeFill::Solid(Rgba::WHITE)
        };
        let slide_rect = Rect::new(0.0, 0.0, pres.slide_width_pt, pres.slide_height_pt);
        render_background_fill(bg_fill, slide_rect, &ctx, &mut page.items);

        // 2. Render decorative master and layout shapes (shapes without `<p:ph>`)
        if slide.show_master_shapes {
            let show_master_sp = layout.is_none_or(|l| l.show_master_shapes);
            if show_master_sp && let Some(m) = master {
                for m_shape in &m.shapes {
                    if m_shape.placeholder.is_none() {
                        render_slide_shape(m_shape, None, &mut ctx, &mut page.items, 0);
                    }
                }
            }
            if let Some(l) = layout {
                for l_shape in &l.shapes {
                    if l_shape.placeholder.is_none() {
                        render_slide_shape(l_shape, None, &mut ctx, &mut page.items, 0);
                    }
                }
            }
        }

        // 3. Render slide shapes
        for s_shape in &slide.shapes {
            render_slide_shape(s_shape, None, &mut ctx, &mut page.items, 0);
        }

        // 4. Render slide comments when in review markup mode
        if matches!(
            options.markup_mode,
            MarkupMode::AllMarkup | MarkupMode::SimpleMarkup
        ) && !slide.comments.is_empty()
        {
            render_slide_comments(&slide.comments, &mut ctx, &mut page.items);
        }

        // 5. Render speaker notes strip/callout when present and in AllMarkup mode
        if options.markup_mode == MarkupMode::AllMarkup
            && let Some(ref notes) = slide.notes
            && !notes.plain_text.is_empty()
        {
            render_speaker_notes_banner(&notes.plain_text, slide_rect, &mut ctx, &mut page.items);
        }

        pages.push(page);
    }

    if pages.is_empty() {
        let mut page = LayoutPage::new(0, Size::new(pres.slide_width_pt, pres.slide_height_pt));
        page.label = Some("Slide 1".to_string());
        pages.push(page);
    }

    Ok(LayoutDocument {
        pages,
        fonts: font_sys.store,
        images,
        font_report: font_sys.report,
    })
}

struct SlideRenderCtx<'a> {
    pres: &'a Presentation,
    slide: &'a Slide,
    layout: Option<&'a SlideLayout>,
    master: Option<&'a SlideMaster>,
    theme: &'a DrawingTheme,
    options: &'a LayoutOptions,
    font_sys: &'a mut FontSystem,
    text_engine: &'a mut TextLayoutEngine,
    image_id_map: &'a BTreeMap<String, ImageId>,
    raw_media: &'a BTreeMap<String, Vec<u8>>,
    page_index: u32,
    block_counter: u32,
}

#[derive(Debug, Clone, Copy)]
struct GroupTransform {
    ch_off_x: f32,
    ch_off_y: f32,
    scale_x: f32,
    scale_y: f32,
    dest_x: f32,
    dest_y: f32,
}

impl GroupTransform {
    fn map_rect(self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(
            self.dest_x + (x - self.ch_off_x) * self.scale_x,
            self.dest_y + (y - self.ch_off_y) * self.scale_y,
            (w * self.scale_x).max(1.0),
            (h * self.scale_y).max(1.0),
        )
    }
}

fn render_background_fill(
    fill: &ShapeFill,
    rect: Rect,
    ctx: &SlideRenderCtx<'_>,
    items: &mut Vec<Item>,
) {
    match fill {
        ShapeFill::Solid(c) => {
            items.push(Item::Path(PathItem {
                commands: rect_commands(rect),
                fill: Some(FillPaint::Solid(*c)),
                fill_rule: FillRule::NonZero,
                stroke: None,
                transform: Affine::IDENTITY,
            }));
        }
        ShapeFill::LinearGradient { angle_deg, stops } => {
            items.push(Item::Path(PathItem {
                commands: rect_commands(rect),
                fill: Some(build_linear_grad_paint(rect, *angle_deg, stops)),
                fill_rule: FillRule::NonZero,
                stroke: None,
                transform: Affine::IDENTITY,
            }));
        }
        ShapeFill::Picture { resource_id } => {
            items.push(Item::Path(PathItem {
                commands: rect_commands(rect),
                fill: Some(FillPaint::Solid(Rgba::WHITE)),
                fill_rule: FillRule::NonZero,
                stroke: None,
                transform: Affine::IDENTITY,
            }));
            if let Some(&img_id) = ctx.image_id_map.get(resource_id) {
                items.push(Item::Image(ImageItem {
                    image: img_id,
                    rect,
                    transform: Affine::IDENTITY,
                    opacity: 1.0,
                    alt_text: None,
                }));
            }
        }
        ShapeFill::Inherited | ShapeFill::NoFill => {
            items.push(Item::Path(PathItem {
                commands: rect_commands(rect),
                fill: Some(FillPaint::Solid(Rgba::WHITE)),
                fill_rule: FillRule::NonZero,
                stroke: None,
                transform: Affine::IDENTITY,
            }));
        }
    }
}

#[allow(clippy::too_many_lines)]
fn render_slide_shape(
    shape: &SlideShape,
    grp_tf: Option<GroupTransform>,
    ctx: &mut SlideRenderCtx<'_>,
    out_items: &mut Vec<Item>,
    depth: u32,
) {
    if depth > ctx.options.max_recursion_depth {
        return;
    }
    if shape.hidden && !ctx.options.show_hidden_text {
        return;
    }

    // Resolve placeholder inheritance chain (`master -> layout -> slide`)
    let (layout_ph, master_ph) = if let Some(ref ph) = shape.placeholder {
        Presentation::resolve_placeholder_chain(ph, ctx.layout, ctx.master)
    } else {
        (None, None)
    };

    let (raw_x, raw_y, raw_w, raw_h, rot_deg) = resolve_shape_geometry(
        shape,
        layout_ph,
        master_ph,
        ctx.pres.slide_width_pt,
        ctx.pres.slide_height_pt,
    );
    let mut rect = if let Some(gt) = grp_tf {
        gt.map_rect(raw_x, raw_y, raw_w, raw_h)
    } else {
        Rect::new(raw_x, raw_y, raw_w.max(1.0), raw_h.max(1.0))
    };

    let fill = resolve_shape_fill(shape, layout_ph, master_ph);
    let stroke = resolve_shape_stroke(shape, layout_ph, master_ph);
    let preset = shape
        .preset_geom
        .as_deref()
        .or_else(|| layout_ph.and_then(|s| s.preset_geom.as_deref()))
        .or_else(|| master_ph.and_then(|s| s.preset_geom.as_deref()))
        .unwrap_or("rect");

    let mut shape_items: Vec<Item> = Vec::new();

    match &shape.kind {
        ShapeKind::AutoShape { text_body } => {
            // If `spAutoFit` is enabled, pre-measure text height so the shape background grows if needed
            let effective_tb = text_body
                .as_ref()
                .or_else(|| {
                    layout_ph.and_then(|s| match &s.kind {
                        ShapeKind::AutoShape { text_body } => text_body.as_ref(),
                        _ => None,
                    })
                })
                .or_else(|| {
                    master_ph.and_then(|s| match &s.kind {
                        ShapeKind::AutoShape { text_body } => text_body.as_ref(),
                        _ => None,
                    })
                });

            let mut pre_shaped_text_items = Vec::new();
            if let Some(tb) = effective_tb {
                let (tb_items, measured_h) = layout_text_body(
                    tb,
                    shape.placeholder.as_ref(),
                    layout_ph,
                    master_ph,
                    rect,
                    fill,
                    ctx,
                );
                let eff_autofit = resolve_autofit(tb, layout_ph, master_ph);
                if eff_autofit == AutofitMode::Shape && measured_h > rect.height {
                    rect.height = measured_h;
                }
                pre_shaped_text_items = tb_items;
            }

            render_shape_geometry(shape, preset, rect, fill, stroke, ctx, &mut shape_items);
            shape_items.extend(pre_shaped_text_items);
        }
        ShapeKind::Picture {
            resource_id,
            opacity,
            ..
        } => {
            render_shape_geometry(shape, preset, rect, fill, stroke, ctx, &mut shape_items);
            if let Some(raw_bytes) = ctx.raw_media.get(resource_id)
                && is_metafile(raw_bytes)
            {
                let mf = render_metafile(raw_bytes, rect, ctx.font_sys, ctx.text_engine);
                shape_items.extend(mf);
            } else if let Some(&img_id) = ctx.image_id_map.get(resource_id) {
                shape_items.push(Item::Image(ImageItem {
                    image: img_id,
                    rect,
                    transform: Affine::IDENTITY,
                    opacity: *opacity,
                    alt_text: shape.alt_text.clone(),
                }));
            }
        }
        ShapeKind::Group {
            ch_off_x_pt,
            ch_off_y_pt,
            ch_ext_w_pt,
            ch_ext_h_pt,
            children,
        } => {
            let sx = if *ch_ext_w_pt > 0.01 {
                rect.width / *ch_ext_w_pt
            } else {
                1.0
            };
            let sy = if *ch_ext_h_pt > 0.01 {
                rect.height / *ch_ext_h_pt
            } else {
                1.0
            };
            let child_tf = GroupTransform {
                ch_off_x: *ch_off_x_pt,
                ch_off_y: *ch_off_y_pt,
                scale_x: sx,
                scale_y: sy,
                dest_x: rect.x,
                dest_y: rect.y,
            };
            for child in children {
                render_slide_shape(child, Some(child_tf), ctx, &mut shape_items, depth + 1);
            }
        }
        ShapeKind::Table(table) => {
            render_slide_table(table, rect, ctx, &mut shape_items);
        }
        ShapeKind::Chart(chart) => {
            let chart_items = render_chart(chart, rect, ctx.theme, ctx.font_sys, ctx.text_engine);
            shape_items.extend(chart_items);
        }
        ShapeKind::SmartArt {
            shapes,
            fallback_items,
        } => {
            if !shapes.is_empty() {
                for s in shapes {
                    render_slide_shape(s, grp_tf, ctx, &mut shape_items, depth + 1);
                }
            } else if !fallback_items.is_empty() {
                render_smartart_fallback(fallback_items, rect, ctx, &mut shape_items);
            }
        }
        ShapeKind::MediaPoster {
            poster_resource_id,
            label,
            ..
        } => {
            render_media_poster(
                poster_resource_id.as_deref(),
                label,
                rect,
                ctx,
                &mut shape_items,
            );
        }
        ShapeKind::Connector {
            head_arrow,
            tail_arrow,
        } => {
            render_connector(
                rect,
                stroke,
                shape.flip_h,
                shape.flip_v,
                *head_arrow,
                *tail_arrow,
                &mut shape_items,
            );
        }
    }

    if let Some(ref target) = shape.hyperlink {
        shape_items.push(Item::Link(LinkItem {
            rect,
            target: target.clone(),
        }));
    }

    if rot_deg.abs() > 0.05 && !shape_items.is_empty() {
        let cx = rect.x + rect.width * 0.5;
        let cy = rect.y + rect.height * 0.5;
        let tf = Affine::translate(cx, cy)
            .then(Affine::rotate_deg(rot_deg))
            .then(Affine::translate(-cx, -cy));
        out_items.push(Item::Group(GroupItem {
            transform: tf,
            clip: None,
            opacity: 1.0,
            children: shape_items,
        }));
    } else {
        out_items.extend(shape_items);
    }
}

fn resolve_shape_geometry(
    shape: &SlideShape,
    layout_ph: Option<&SlideShape>,
    master_ph: Option<&SlideShape>,
    slide_w: f32,
    slide_h: f32,
) -> (f32, f32, f32, f32, f32) {
    if shape.has_explicit_xfrm && shape.width_pt > 0.0 && shape.height_pt > 0.0 {
        return (
            shape.x_pt,
            shape.y_pt,
            shape.width_pt,
            shape.height_pt,
            shape.rotation_deg,
        );
    }
    if let Some(lp) = layout_ph
        && lp.has_explicit_xfrm
        && lp.width_pt > 0.0
        && lp.height_pt > 0.0
    {
        return (lp.x_pt, lp.y_pt, lp.width_pt, lp.height_pt, lp.rotation_deg);
    }
    if let Some(mp) = master_ph
        && mp.has_explicit_xfrm
        && mp.width_pt > 0.0
        && mp.height_pt > 0.0
    {
        return (mp.x_pt, mp.y_pt, mp.width_pt, mp.height_pt, mp.rotation_deg);
    }
    // Fallback default placeholder positions
    let ph_type = shape
        .placeholder
        .as_ref()
        .and_then(|p| p.ph_type.as_deref())
        .unwrap_or("body");
    match ph_type {
        "title" | "ctrTitle" => (
            slide_w * 0.07,
            slide_h * 0.08,
            slide_w * 0.86,
            slide_h * 0.18,
            0.0,
        ),
        "subTitle" => (
            slide_w * 0.10,
            slide_h * 0.32,
            slide_w * 0.80,
            slide_h * 0.25,
            0.0,
        ),
        "dt" => (
            slide_w * 0.05,
            slide_h * 0.90,
            slide_w * 0.25,
            slide_h * 0.06,
            0.0,
        ),
        "ftr" => (
            slide_w * 0.35,
            slide_h * 0.90,
            slide_w * 0.30,
            slide_h * 0.06,
            0.0,
        ),
        "sldNum" => (
            slide_w * 0.70,
            slide_h * 0.90,
            slide_w * 0.25,
            slide_h * 0.06,
            0.0,
        ),
        _ => (
            slide_w * 0.07,
            slide_h * 0.26,
            slide_w * 0.86,
            slide_h * 0.62,
            0.0,
        ),
    }
}

fn resolve_shape_fill<'a>(
    shape: &'a SlideShape,
    layout_ph: Option<&'a SlideShape>,
    master_ph: Option<&'a SlideShape>,
) -> &'a ShapeFill {
    if shape.fill != ShapeFill::Inherited {
        return &shape.fill;
    }
    if let Some(lp) = layout_ph
        && lp.fill != ShapeFill::Inherited
    {
        return &lp.fill;
    }
    if let Some(mp) = master_ph
        && mp.fill != ShapeFill::Inherited
    {
        return &mp.fill;
    }
    &ShapeFill::NoFill
}

fn resolve_shape_stroke<'a>(
    shape: &'a SlideShape,
    layout_ph: Option<&'a SlideShape>,
    master_ph: Option<&'a SlideShape>,
) -> Option<&'a ShapeStroke> {
    shape
        .stroke
        .as_ref()
        .or_else(|| layout_ph.and_then(|s| s.stroke.as_ref()))
        .or_else(|| master_ph.and_then(|s| s.stroke.as_ref()))
}

fn render_shape_geometry(
    shape: &SlideShape,
    preset: &str,
    rect: Rect,
    fill: &ShapeFill,
    stroke: Option<&ShapeStroke>,
    ctx: &SlideRenderCtx<'_>,
    items: &mut Vec<Item>,
) {
    let fill_paint = match fill {
        ShapeFill::Solid(c) => Some(FillPaint::Solid(*c)),
        ShapeFill::LinearGradient { angle_deg, stops } => {
            Some(build_linear_grad_paint(rect, *angle_deg, stops))
        }
        ShapeFill::Picture { resource_id } => {
            if let Some(&img_id) = ctx.image_id_map.get(resource_id) {
                items.push(Item::Image(ImageItem {
                    image: img_id,
                    rect,
                    transform: Affine::IDENTITY,
                    opacity: 1.0,
                    alt_text: None,
                }));
            }
            None
        }
        ShapeFill::Inherited | ShapeFill::NoFill => None,
    };

    let stroke_style = stroke.map(to_layout_stroke);
    if fill_paint.is_none() && stroke_style.is_none() {
        return;
    }

    let commands = if let Some(ref cust) = shape.custom_geom {
        build_custom_geometry_paths(cust, rect)
    } else {
        #[allow(clippy::cast_precision_loss)]
        let adj_vec: Vec<(String, f64)> = shape
            .adjust_values
            .iter()
            .map(|(k, &v)| (k.clone(), v as f64))
            .collect();
        build_preset_geometry_paths(preset, rect, &adj_vec)
    };

    if !commands.is_empty() {
        items.push(Item::Path(PathItem {
            commands,
            fill: fill_paint,
            fill_rule: FillRule::NonZero,
            stroke: stroke_style,
            transform: Affine::IDENTITY,
        }));
    }
}

#[allow(clippy::fn_params_excessive_bools)]
fn render_connector(
    rect: Rect,
    stroke: Option<&ShapeStroke>,
    flip_h: bool,
    flip_v: bool,
    head_arrow: bool,
    tail_arrow: bool,
    items: &mut Vec<Item>,
) {
    let default_stroke = ShapeStroke {
        color: Rgba::rgb(0x44, 0x72, 0xC4),
        width_pt: 1.0,
        dash: None,
    };
    let st = stroke.unwrap_or(&default_stroke);
    let x1 = if flip_h { rect.right() } else { rect.x };
    let y1 = if flip_v { rect.bottom() } else { rect.y };
    let x2 = if flip_h { rect.x } else { rect.right() };
    let y2 = if flip_v { rect.y } else { rect.bottom() };

    items.push(Item::Path(PathItem {
        commands: vec![
            PathCommand::MoveTo(Point::new(x1, y1)),
            PathCommand::LineTo(Point::new(x2, y2)),
        ],
        fill: None,
        fill_rule: FillRule::NonZero,
        stroke: Some(to_layout_stroke(st)),
        transform: Affine::IDENTITY,
    }));

    if tail_arrow {
        emit_arrowhead(Point::new(x1, y1), Point::new(x2, y2), st.color, items);
    }
    if head_arrow {
        emit_arrowhead(Point::new(x2, y2), Point::new(x1, y1), st.color, items);
    }
}

fn emit_arrowhead(from: Point, to: Point, color: Rgba, items: &mut Vec<Item>) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len = dx.hypot(dy);
    if len < 1.0 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let size = 6.0_f32;
    let base_x = to.x - ux * size;
    let base_y = to.y - uy * size;
    let px = -uy * (size * 0.55);
    let py = ux * (size * 0.55);
    items.push(Item::Path(PathItem {
        commands: vec![
            PathCommand::MoveTo(to),
            PathCommand::LineTo(Point::new(base_x + px, base_y + py)),
            PathCommand::LineTo(Point::new(base_x - px, base_y - py)),
            PathCommand::Close,
        ],
        fill: Some(FillPaint::Solid(color)),
        fill_rule: FillRule::NonZero,
        stroke: None,
        transform: Affine::IDENTITY,
    }));
}

fn resolve_autofit(
    tb: &TextBody,
    layout_ph: Option<&SlideShape>,
    master_ph: Option<&SlideShape>,
) -> AutofitMode {
    if tb.autofit != AutofitMode::None {
        return tb.autofit;
    }
    if let Some(lp) = layout_ph
        && let ShapeKind::AutoShape {
            text_body: Some(ref ltb),
        } = lp.kind
        && ltb.autofit != AutofitMode::None
    {
        return ltb.autofit;
    }
    if let Some(mp) = master_ph
        && let ShapeKind::AutoShape {
            text_body: Some(ref mtb),
        } = mp.kind
        && mtb.autofit != AutofitMode::None
    {
        return mtb.autofit;
    }
    AutofitMode::None
}

#[allow(clippy::too_many_lines)]
fn layout_text_body(
    tb: &TextBody,
    ph: Option<&PlaceholderRef>,
    layout_ph: Option<&SlideShape>,
    master_ph: Option<&SlideShape>,
    rect: Rect,
    shape_fill: &ShapeFill,
    ctx: &mut SlideRenderCtx<'_>,
) -> (Vec<Item>, f32) {
    if tb.paragraphs.is_empty() {
        return (Vec::new(), 0.0);
    }

    let inner_x = rect.x + tb.inset_left_pt;
    let inner_y = rect.y + tb.inset_top_pt;
    let inner_w = (rect.width - tb.inset_left_pt - tb.inset_right_pt).max(12.0);
    let inner_h = (rect.height - tb.inset_top_pt - tb.inset_bottom_pt).max(12.0);

    let autofit = resolve_autofit(tb, layout_ph, master_ph);
    let (mut scale, ln_spc_red) = match autofit {
        AutofitMode::Normal {
            font_scale_pct,
            line_space_reduction_pct,
        } => (
            (font_scale_pct / 100.0).clamp(0.5, 1.0),
            (line_space_reduction_pct / 100.0).clamp(0.0, 0.5),
        ),
        AutofitMode::None | AutofitMode::Shape => (1.0_f32, 0.0_f32),
    };

    // Determine default text contrast color when no run/style color is specified
    let default_text_color = match shape_fill {
        ShapeFill::Solid(c) => {
            let luma = 0.299 * f32::from(c.r) + 0.587 * f32::from(c.g) + 0.114 * f32::from(c.b);
            if luma < 115.0 {
                Rgba::WHITE
            } else {
                ctx.theme.colors.get("dk1").copied().unwrap_or(Rgba::BLACK)
            }
        }
        _ => ctx.theme.colors.get("dk1").copied().unwrap_or(Rgba::BLACK),
    };

    loop {
        let (candidate_items, total_h) = shape_text_body_at_scale(
            tb,
            ph,
            layout_ph,
            master_ph,
            inner_x,
            inner_y,
            inner_w,
            scale,
            ln_spc_red,
            default_text_color,
            ctx,
        );
        if matches!(autofit, AutofitMode::Normal { .. }) && total_h > inner_h && scale > 0.51 {
            scale = (scale - 0.05).max(0.50);
            continue;
        }

        let v_offset = match tb.vertical_anchor {
            TextVerticalAnchor::Top => 0.0,
            TextVerticalAnchor::Middle => ((inner_h - total_h) * 0.5).max(0.0),
            TextVerticalAnchor::Bottom => (inner_h - total_h).max(0.0),
        };
        let final_items = if v_offset > 0.25 {
            candidate_items
                .into_iter()
                .map(|it| translate_item(it, 0.0, v_offset))
                .collect()
        } else {
            candidate_items
        };
        let total_outer_h = total_h + tb.inset_top_pt + tb.inset_bottom_pt;
        return (final_items, total_outer_h);
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn shape_text_body_at_scale(
    tb: &TextBody,
    ph: Option<&PlaceholderRef>,
    layout_ph: Option<&SlideShape>,
    master_ph: Option<&SlideShape>,
    inner_x: f32,
    inner_y: f32,
    inner_w: f32,
    scale: f32,
    ln_spc_red: f32,
    default_text_color: Rgba,
    ctx: &mut SlideRenderCtx<'_>,
) -> (Vec<Item>, f32) {
    let mut items = Vec::new();
    let mut y_cursor = inner_y;
    let mut auto_num_counters = [0u32; 9];

    for para in &tb.paragraphs {
        let lvl = para.props.level.min(8);
        let resolved_lvl =
            resolve_paragraph_level_style(para, lvl, tb, ph, layout_ph, master_ph, ctx.master);

        let space_before = resolved_lvl.space_before_pt.unwrap_or(0.0) * scale;
        let space_after = resolved_lvl.space_after_pt.unwrap_or(2.0) * scale;
        y_cursor += space_before;

        let margin_left = resolved_lvl.margin_left_pt.unwrap_or(f32::from(lvl) * 18.0);
        let indent = resolved_lvl.indent_pt.unwrap_or(0.0);

        let mut spans: Vec<SpanSpec> = Vec::new();
        let mut hyperlinks: Vec<(usize, String)> = Vec::new();

        // Emit bullet prefix if paragraph has non-empty text and a bullet
        let has_nonempty_text = para.runs.iter().any(|r| !r.text.trim().is_empty());
        if has_nonempty_text {
            let bullet_prefix = match &resolved_lvl.bullet {
                BulletKind::Char(ch) => Some(format!("{ch} ")),
                BulletKind::AutoNum { start_at, .. } => {
                    let c = &mut auto_num_counters[usize::from(lvl)];
                    if *c == 0 {
                        *c = *start_at;
                    } else {
                        *c += 1;
                    }
                    Some(format!("{c}. "))
                }
                BulletKind::Inherited | BulletKind::None => None,
            };
            if let Some(bp) = bullet_prefix {
                let def_sz =
                    (resolved_lvl.default_run.font_size_pt.unwrap_or(18.0) * scale).max(4.0);
                let def_fam = resolved_lvl
                    .default_run
                    .font_family
                    .clone()
                    .unwrap_or_else(|| ctx.theme.minor_latin_font.clone());
                let def_col = resolved_lvl.default_run.color.unwrap_or(default_text_color);
                spans.push(SpanSpec {
                    text: bp,
                    family: def_fam,
                    size: def_sz,
                    color: def_col,
                    ..SpanSpec::default()
                });
            }
        }

        for (run_idx, run) in para.runs.iter().enumerate() {
            let text = if run.field_type.as_deref() == Some("slidenum") {
                ctx.slide.number.to_string()
            } else {
                run.text.clone()
            };
            if text.is_empty() {
                continue;
            }
            let r_style = merge_run_styles(&resolved_lvl.default_run, &run.style);
            let font_size = (r_style.font_size_pt.unwrap_or(18.0) * scale).max(4.0);
            let family = r_style
                .font_family
                .unwrap_or_else(|| ctx.theme.minor_latin_font.clone());
            let color = r_style.color.unwrap_or(default_text_color);
            let script = match r_style.baseline_pct {
                Some(b) if b > 5.0 => ScriptPosition::Superscript,
                Some(b) if b < -5.0 => ScriptPosition::Subscript,
                _ => ScriptPosition::Normal,
            };
            if let Some(ref hl) = r_style.hyperlink {
                hyperlinks.push((run_idx, hl.clone()));
            }
            #[allow(clippy::cast_possible_truncation)]
            spans.push(SpanSpec {
                text,
                family,
                size: font_size,
                bold: r_style.bold.unwrap_or(false),
                italic: r_style.italic.unwrap_or(false),
                underline: r_style.underline.unwrap_or(false),
                strikethrough: r_style.strikethrough.unwrap_or(false),
                color,
                highlight: None,
                script,
                run_index: run_idx as u32,
            });
        }

        if spans.is_empty() {
            let def_sz = (resolved_lvl.default_run.font_size_pt.unwrap_or(18.0) * scale).max(4.0);
            y_cursor += def_sz * 0.8 + space_after;
            continue;
        }

        ctx.block_counter = ctx.block_counter.wrapping_add(1);
        let align = match resolved_lvl.align.unwrap_or(PptTextAlign::Left) {
            PptTextAlign::Left => TextAlign::Left,
            PptTextAlign::Center => TextAlign::Center,
            PptTextAlign::Right => TextAlign::Right,
            PptTextAlign::Justify => TextAlign::Justify,
        };
        let line_spacing = match resolved_lvl.line_spacing_mult {
            Some(v) if v < 0.0 => LineSpacing::ExactPt((-v * (1.0 - ln_spc_red)).max(4.0)),
            Some(v) => LineSpacing::Multiple((v * (1.0 - ln_spc_red)).max(0.6)),
            None => LineSpacing::Multiple((1.12 * (1.0 - ln_spc_red)).max(0.7)),
        };

        let para_x = inner_x + margin_left.max(0.0);
        let para_w = (inner_w - margin_left.max(0.0)).max(12.0);
        let spec = ParagraphSpec {
            spans,
            align,
            line_spacing,
            max_width: if tb.word_wrap { Some(para_w) } else { None },
            first_line_indent: indent,
            origin: Point::new(para_x, y_cursor),
            page_index: ctx.page_index,
            block_index: ctx.block_counter,
        };
        let shaped = ctx.text_engine.layout_paragraph(&spec, ctx.font_sys);
        for run in shaped.runs {
            items.push(Item::Glyphs(run));
        }
        y_cursor += shaped.height + space_after;
    }

    (items, (y_cursor - inner_y).max(0.0))
}

fn resolve_paragraph_level_style(
    para: &TextParagraph,
    lvl: u8,
    tb: &TextBody,
    ph: Option<&PlaceholderRef>,
    layout_ph: Option<&SlideShape>,
    master_ph: Option<&SlideShape>,
    master: Option<&SlideMaster>,
) -> TextLevelStyle {
    let ph_type = ph.and_then(|p| p.ph_type.as_deref()).unwrap_or("");
    let is_title = matches!(ph_type, "title" | "ctrTitle");
    let is_body = matches!(ph_type, "body" | "subTitle" | "obj");

    // Base defaults according to placeholder type
    let mut resolved = TextLevelStyle {
        level: lvl,
        align: Some(if ph_type == "ctrTitle" {
            PptTextAlign::Center
        } else {
            PptTextAlign::Left
        }),
        margin_left_pt: None,
        indent_pt: None,
        line_spacing_mult: None,
        space_before_pt: None,
        space_after_pt: None,
        bullet: if is_body && ph_type != "subTitle" {
            BulletKind::Char("\u{2022}".to_string())
        } else {
            BulletKind::None
        },
        default_run: TextRunStyle {
            font_size_pt: Some(if is_title {
                36.0
            } else if ph_type == "subTitle" {
                22.0
            } else if is_body {
                20.0 - f32::from(lvl) * 2.0
            } else {
                16.0
            }),
            ..TextRunStyle::default()
        },
    };

    // Layer 1: Master txStyles (titleStyle / bodyStyle / otherStyle)
    if let Some(m) = master {
        let master_list = if is_title {
            &m.title_style
        } else if is_body {
            &m.body_style
        } else {
            &m.other_style
        };
        if let Some(ms) = master_list.levels.get(&lvl) {
            apply_level_override(&mut resolved, ms);
        }
    }

    // Layer 2: Master placeholder lstStyle
    if let Some(mp) = master_ph
        && let ShapeKind::AutoShape {
            text_body: Some(ref mtb),
        } = mp.kind
        && let Some(ms) = mtb.list_style.levels.get(&lvl)
    {
        apply_level_override(&mut resolved, ms);
    }

    // Layer 3: Layout placeholder lstStyle
    if let Some(lp) = layout_ph
        && let ShapeKind::AutoShape {
            text_body: Some(ref ltb),
        } = lp.kind
        && let Some(ls) = ltb.list_style.levels.get(&lvl)
    {
        apply_level_override(&mut resolved, ls);
    }

    // Layer 4: Shape's own lstStyle
    if let Some(ss) = tb.list_style.levels.get(&lvl) {
        apply_level_override(&mut resolved, ss);
    }

    // Layer 5: Paragraph's own pPr
    apply_level_override(&mut resolved, &para.props);

    resolved
}

fn apply_level_override(dest: &mut TextLevelStyle, src: &TextLevelStyle) {
    if src.align.is_some() {
        dest.align = src.align;
    }
    if src.margin_left_pt.is_some() {
        dest.margin_left_pt = src.margin_left_pt;
    }
    if src.indent_pt.is_some() {
        dest.indent_pt = src.indent_pt;
    }
    if src.line_spacing_mult.is_some() {
        dest.line_spacing_mult = src.line_spacing_mult;
    }
    if src.space_before_pt.is_some() {
        dest.space_before_pt = src.space_before_pt;
    }
    if src.space_after_pt.is_some() {
        dest.space_after_pt = src.space_after_pt;
    }
    if src.bullet != BulletKind::Inherited {
        dest.bullet.clone_from(&src.bullet);
    }
    dest.default_run = merge_run_styles(&dest.default_run, &src.default_run);
}

fn merge_run_styles(base: &TextRunStyle, over: &TextRunStyle) -> TextRunStyle {
    TextRunStyle {
        font_family: over
            .font_family
            .clone()
            .or_else(|| base.font_family.clone()),
        font_size_pt: over.font_size_pt.or(base.font_size_pt),
        bold: over.bold.or(base.bold),
        italic: over.italic.or(base.italic),
        underline: over.underline.or(base.underline),
        strikethrough: over.strikethrough.or(base.strikethrough),
        baseline_pct: over.baseline_pct.or(base.baseline_pct),
        color: over.color.or(base.color),
        hyperlink: over.hyperlink.clone().or_else(|| base.hyperlink.clone()),
    }
}

fn render_slide_table(
    table: &SlideTable,
    rect: Rect,
    ctx: &mut SlideRenderCtx<'_>,
    items: &mut Vec<Item>,
) {
    if table.rows.is_empty() {
        return;
    }
    let num_cols = table
        .col_widths_pt
        .len()
        .max(table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(1));
    #[allow(clippy::cast_precision_loss)]
    let col_widths: Vec<f32> = if table.col_widths_pt.is_empty() {
        vec![rect.width / (num_cols as f32); num_cols]
    } else {
        let sum: f32 = table.col_widths_pt.iter().sum::<f32>().max(1.0);
        table
            .col_widths_pt
            .iter()
            .map(|w| w * rect.width / sum)
            .collect()
    };

    let accent1 = ctx
        .theme
        .colors
        .get("accent1")
        .copied()
        .unwrap_or(Rgba::rgb(0x44, 0x72, 0xC4));

    let mut y = rect.y;
    for (r_idx, row) in table.rows.iter().enumerate() {
        let row_h = row.height_pt.max(18.0);
        let mut x = rect.x;
        for (c_idx, cell) in row.cells.iter().enumerate() {
            let col_w = col_widths.get(c_idx).copied().unwrap_or(48.0);
            if cell.h_merge || cell.v_merge {
                x += col_w;
                continue;
            }
            let span_cols = usize::from(cell.grid_span.max(1));
            let cell_w: f32 = col_widths
                .iter()
                .skip(c_idx)
                .take(span_cols)
                .sum::<f32>()
                .max(col_w);
            let span_rows = usize::from(cell.row_span.max(1));
            let cell_h: f32 = table
                .rows
                .iter()
                .skip(r_idx)
                .take(span_rows)
                .map(|r| r.height_pt.max(18.0))
                .sum::<f32>()
                .max(row_h);

            let cell_rect = Rect::new(x, y, cell_w, cell_h);

            let effective_fill = if cell.fill != ShapeFill::Inherited {
                cell.fill.clone()
            } else if table.first_row && r_idx == 0 {
                ShapeFill::Solid(accent1)
            } else if table.band_row && r_idx % 2 == 1 {
                ShapeFill::Solid(Rgba::rgb(0xD9, 0xE2, 0xF3))
            } else {
                ShapeFill::Solid(Rgba::rgb(0xF9, 0xF9, 0xF9))
            };

            if let ShapeFill::Solid(c) = effective_fill {
                items.push(Item::Path(PathItem {
                    commands: rect_commands(cell_rect),
                    fill: Some(FillPaint::Solid(c)),
                    fill_rule: FillRule::NonZero,
                    stroke: Some(StrokeStyle {
                        color: Rgba::rgb(0xBF, 0xBF, 0xBF),
                        width: 0.5,
                        line_cap: LineCap::Butt,
                        line_join: LineJoin::Miter,
                        dash_array: Vec::new(),
                        dash_offset: 0.0,
                    }),
                    transform: Affine::IDENTITY,
                }));
            }

            if let Some(ref tb) = cell.text_body {
                let (tb_items, _) =
                    layout_text_body(tb, None, None, None, cell_rect, &effective_fill, ctx);
                items.extend(tb_items);
            }

            x += col_w;
        }
        y += row_h;
    }
}

fn render_smartart_fallback(
    fallback_items: &[String],
    rect: Rect,
    ctx: &mut SlideRenderCtx<'_>,
    items: &mut Vec<Item>,
) {
    let count = fallback_items.len();
    if count == 0 {
        return;
    }
    let accent = ctx
        .theme
        .colors
        .get("accent1")
        .copied()
        .unwrap_or(Rgba::rgb(0x44, 0x72, 0xC4));
    #[allow(clippy::cast_precision_loss)]
    let box_w = ((rect.width - (count.saturating_sub(1) as f32) * 12.0) / (count as f32)).max(36.0);
    let box_h = (rect.height * 0.65).clamp(28.0, 120.0);
    let box_y = rect.y + (rect.height - box_h) * 0.5;

    for (idx, text) in fallback_items.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let bx = rect.x + (idx as f32) * (box_w + 12.0);
        let box_rect = Rect::new(bx, box_y, box_w, box_h);
        items.push(Item::Path(PathItem {
            commands: rect_commands(box_rect),
            fill: Some(FillPaint::Solid(accent)),
            fill_rule: FillRule::NonZero,
            stroke: None,
            transform: Affine::IDENTITY,
        }));
        ctx.block_counter = ctx.block_counter.wrapping_add(1);
        let spec = ParagraphSpec {
            spans: vec![SpanSpec {
                text: text.clone(),
                family: ctx.theme.minor_latin_font.clone(),
                size: 12.0,
                bold: true,
                color: Rgba::WHITE,
                ..SpanSpec::default()
            }],
            align: TextAlign::Center,
            max_width: Some((box_w - 8.0).max(12.0)),
            origin: Point::new(bx + 4.0, box_y + box_h * 0.35),
            page_index: ctx.page_index,
            block_index: ctx.block_counter,
            ..ParagraphSpec::default()
        };
        let shaped = ctx.text_engine.layout_paragraph(&spec, ctx.font_sys);
        for run in shaped.runs {
            items.push(Item::Glyphs(run));
        }
    }
}

fn render_media_poster(
    poster_resource_id: Option<&str>,
    label: &str,
    rect: Rect,
    ctx: &mut SlideRenderCtx<'_>,
    items: &mut Vec<Item>,
) {
    items.push(Item::Path(PathItem {
        commands: rect_commands(rect),
        fill: Some(FillPaint::Solid(Rgba::rgb(0x22, 0x22, 0x22))),
        fill_rule: FillRule::NonZero,
        stroke: Some(StrokeStyle {
            color: Rgba::rgb(0x66, 0x66, 0x66),
            width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            dash_array: Vec::new(),
            dash_offset: 0.0,
        }),
        transform: Affine::IDENTITY,
    }));
    if let Some(rid) = poster_resource_id
        && let Some(&img_id) = ctx.image_id_map.get(rid)
    {
        items.push(Item::Image(ImageItem {
            image: img_id,
            rect,
            transform: Affine::IDENTITY,
            opacity: 0.85,
            alt_text: Some(label.to_string()),
        }));
    }
    ctx.block_counter = ctx.block_counter.wrapping_add(1);
    let spec = ParagraphSpec {
        spans: vec![SpanSpec {
            text: format!("[{label}]"),
            family: ctx.theme.minor_latin_font.clone(),
            size: 11.0,
            bold: true,
            color: Rgba::WHITE,
            ..SpanSpec::default()
        }],
        align: TextAlign::Center,
        max_width: Some(rect.width),
        origin: Point::new(rect.x, rect.y + rect.height * 0.45),
        page_index: ctx.page_index,
        block_index: ctx.block_counter,
        ..ParagraphSpec::default()
    };
    let shaped = ctx.text_engine.layout_paragraph(&spec, ctx.font_sys);
    for run in shaped.runs {
        items.push(Item::Glyphs(run));
    }
}

fn render_slide_comments(
    comments: &[SlideComment],
    ctx: &mut SlideRenderCtx<'_>,
    items: &mut Vec<Item>,
) {
    for (idx, cm) in comments.iter().enumerate() {
        let bx = cm
            .x_pt
            .clamp(8.0, (ctx.pres.slide_width_pt - 160.0).max(8.0));
        let by = cm
            .y_pt
            .clamp(8.0, (ctx.pres.slide_height_pt - 48.0).max(8.0));
        let badge_rect = Rect::new(bx, by, 152.0, 36.0);
        items.push(Item::Anchor(AnchorItem {
            name: format!("comment:{}", idx + 1),
            rect: badge_rect,
        }));
        items.push(Item::Path(PathItem {
            commands: rect_commands(badge_rect),
            fill: Some(FillPaint::Solid(Rgba::rgb(0xFF, 0xF9, 0xE6))),
            fill_rule: FillRule::NonZero,
            stroke: Some(StrokeStyle {
                color: Rgba::rgb(0xE6, 0xA8, 0x00),
                width: 0.75,
                line_cap: LineCap::Butt,
                line_join: LineJoin::Miter,
                dash_array: Vec::new(),
                dash_offset: 0.0,
            }),
            transform: Affine::IDENTITY,
        }));
        ctx.block_counter = ctx.block_counter.wrapping_add(1);
        let spec = ParagraphSpec {
            spans: vec![
                SpanSpec {
                    text: format!("{}: ", cm.initials),
                    family: ctx.theme.minor_latin_font.clone(),
                    size: 8.5,
                    bold: true,
                    color: Rgba::rgb(0x80, 0x55, 0x00),
                    run_index: 0,
                    ..SpanSpec::default()
                },
                SpanSpec {
                    text: cm.text.clone(),
                    family: ctx.theme.minor_latin_font.clone(),
                    size: 8.5,
                    color: Rgba::rgb(0x22, 0x22, 0x22),
                    run_index: 1,
                    ..SpanSpec::default()
                },
            ],
            align: TextAlign::Left,
            max_width: Some(144.0),
            origin: Point::new(bx + 4.0, by + 3.0),
            page_index: ctx.page_index,
            block_index: ctx.block_counter,
            ..ParagraphSpec::default()
        };
        let shaped = ctx.text_engine.layout_paragraph(&spec, ctx.font_sys);
        for run in shaped.runs {
            items.push(Item::Glyphs(run));
        }
    }
}

fn render_speaker_notes_banner(
    notes_text: &str,
    slide_rect: Rect,
    ctx: &mut SlideRenderCtx<'_>,
    items: &mut Vec<Item>,
) {
    let strip_h = 32.0_f32;
    let strip_rect = Rect::new(0.0, slide_rect.height - strip_h, slide_rect.width, strip_h);
    items.push(Item::Path(PathItem {
        commands: rect_commands(strip_rect),
        fill: Some(FillPaint::Solid(Rgba::new(0xF5, 0xF5, 0xF5, 235))),
        fill_rule: FillRule::NonZero,
        stroke: Some(StrokeStyle {
            color: Rgba::rgb(0xCC, 0xCC, 0xCC),
            width: 0.5,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            dash_array: Vec::new(),
            dash_offset: 0.0,
        }),
        transform: Affine::IDENTITY,
    }));
    ctx.block_counter = ctx.block_counter.wrapping_add(1);
    let single_line = notes_text.replace(['\r', '\n'], " ");
    let spec = ParagraphSpec {
        spans: vec![
            SpanSpec {
                text: "Notes: ".to_string(),
                family: ctx.theme.minor_latin_font.clone(),
                size: 8.5,
                bold: true,
                color: Rgba::rgb(0x44, 0x44, 0x44),
                run_index: 0,
                ..SpanSpec::default()
            },
            SpanSpec {
                text: single_line,
                family: ctx.theme.minor_latin_font.clone(),
                size: 8.5,
                color: Rgba::rgb(0x33, 0x33, 0x33),
                run_index: 1,
                ..SpanSpec::default()
            },
        ],
        align: TextAlign::Left,
        max_width: Some((slide_rect.width - 16.0).max(36.0)),
        origin: Point::new(8.0, strip_rect.y + 4.0),
        page_index: ctx.page_index,
        block_index: ctx.block_counter,
        ..ParagraphSpec::default()
    };
    let shaped = ctx.text_engine.layout_paragraph(&spec, ctx.font_sys);
    for run in shaped.runs {
        items.push(Item::Glyphs(run));
    }
}

fn to_layout_stroke(st: &ShapeStroke) -> StrokeStyle {
    let w = st.width_pt.max(0.5);
    let dash_array = match st.dash.as_deref() {
        Some("dot" | "sysDot") => vec![w, w * 2.0],
        Some("dash" | "sysDash" | "lgDash") => vec![w * 3.0, w * 2.0],
        Some("dashDot" | "lgDashDot") => vec![w * 3.0, w * 2.0, w, w * 2.0],
        _ => Vec::new(),
    };
    StrokeStyle {
        color: st.color,
        width: w,
        line_cap: LineCap::Butt,
        line_join: LineJoin::Miter,
        dash_array,
        dash_offset: 0.0,
    }
}

fn build_linear_grad_paint(rect: Rect, angle_deg: f32, stops: &[(f32, Rgba)]) -> FillPaint {
    let rad = angle_deg.to_radians();
    let cx = rect.x + rect.width * 0.5;
    let cy = rect.y + rect.height * 0.5;
    let dx = rad.cos() * rect.width * 0.5;
    let dy = rad.sin() * rect.height * 0.5;
    FillPaint::LinearGradient(LinearGradient {
        start: Point::new(cx - dx, cy - dy),
        end: Point::new(cx + dx, cy + dy),
        stops: stops
            .iter()
            .map(|&(offset, color)| GradientStop { offset, color })
            .collect(),
    })
}

fn rect_commands(r: Rect) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(Point::new(r.x, r.y)),
        PathCommand::LineTo(Point::new(r.right(), r.y)),
        PathCommand::LineTo(Point::new(r.right(), r.bottom())),
        PathCommand::LineTo(Point::new(r.x, r.bottom())),
        PathCommand::Close,
    ]
}

fn translate_item(item: Item, dx: f32, dy: f32) -> Item {
    match item {
        Item::Glyphs(mut g) => {
            g.transform.coeffs[4] += dx;
            g.transform.coeffs[5] += dy;
            Item::Glyphs(g)
        }
        Item::Path(mut p) => {
            p.transform.coeffs[4] += dx;
            p.transform.coeffs[5] += dy;
            Item::Path(p)
        }
        Item::Image(mut img) => {
            img.rect.x += dx;
            img.rect.y += dy;
            Item::Image(img)
        }
        Item::Group(mut grp) => {
            grp.transform.coeffs[4] += dx;
            grp.transform.coeffs[5] += dy;
            Item::Group(grp)
        }
        Item::Link(mut lnk) => {
            lnk.rect.x += dx;
            lnk.rect.y += dy;
            Item::Link(lnk)
        }
        Item::Anchor(mut anc) => {
            anc.rect.x += dx;
            anc.rect.y += dy;
            Item::Anchor(anc)
        }
    }
}
