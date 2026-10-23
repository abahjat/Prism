// SPDX-License-Identifier: AGPL-3.0-only
//! Pure-Rust CPU rasterizer (`tiny-skia`) and deterministic PNG encoder for [`LayoutPage`].

use prism_core::error::{Error, Result};
use prism_layout::{
    Affine, ClipPath, FillPaint, FillRule, FontStore, GlyphRun, GroupItem, ImageItem, ImageStore,
    Item, LayoutDocument, LayoutOptions, LayoutPage, LineCap, LineDecoration, LineJoin,
    NORMALIZED_UPEM, PathCommand, PathItem, Rect, Rgba,
};
use tiff::encoder::{Compression as TiffCompression, DeflateLevel, TiffEncoder, colortype};
use tiny_skia::{
    BlendMode, Color, FillRule as SkiaFillRule, FilterQuality, GradientStop as SkiaGradientStop,
    LineCap as SkiaLineCap, LineJoin as SkiaLineJoin, LinearGradient as SkiaLinearGradient, Mask,
    Paint, Path, PathBuilder, Pixmap, PixmapPaint, Point as SkiaPoint,
    RadialGradient as SkiaRadialGradient, SpreadMode, Stroke, StrokeDash, Transform,
};

/// Configuration options for rasterizing a [`LayoutPage`].
#[derive(Debug, Clone)]
pub struct RasterOptions {
    /// Target resolution in dots per inch (default `144.0`, i.e. `2×` typographic points).
    pub dpi: f32,
    /// Page background fill color (default [`Rgba::WHITE`]).
    pub background: Rgba,
    /// Enable subpixel anti-aliasing on vector paths and glyph outlines (default `true`).
    pub anti_alias: bool,
    /// Resource limits enforced during rasterization (including `max_image_pixels`).
    pub layout_options: LayoutOptions,
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            dpi: 144.0,
            background: Rgba::WHITE,
            anti_alias: true,
            layout_options: LayoutOptions::default(),
        }
    }
}

/// Unpremultiplied 8-bit RGBA raster buffer produced by [`render_page_rgba`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major unpremultiplied `RGBA8` pixel bytes (`width * height * 4`).
    pub data: Vec<u8>,
}

struct RasterState<'a> {
    fonts: &'a FontStore,
    images: &'a ImageStore,
    options: &'a RasterOptions,
    glyph_count: usize,
}

/// Rasterize a [`LayoutPage`] into an unpremultiplied `RGBA8` [`RasterImage`].
///
/// # Errors
///
/// Returns an [`Error`] if the target pixel dimensions exceed `options.layout_options.max_image_pixels`
/// or if per-page glyph/recursion limits are exceeded.
pub fn render_page_rgba(
    page: &LayoutPage,
    fonts: &FontStore,
    images: &ImageStore,
    options: &RasterOptions,
) -> Result<RasterImage> {
    let dpi = options.dpi.clamp(9.0, 2400.0);
    let scale = dpi / 72.0;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let width = (page.size.width.max(1.0) * scale).ceil() as u32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let height = (page.size.height.max(1.0) * scale).ceil() as u32;

    let total_pixels = u64::from(width) * u64::from(height);
    if total_pixels > options.layout_options.max_image_pixels {
        return Err(Error::MemoryLimitExceeded {
            used: usize::try_from(total_pixels).unwrap_or(usize::MAX),
            limit: usize::try_from(options.layout_options.max_image_pixels).unwrap_or(usize::MAX),
        });
    }

    let mut pixmap = Pixmap::new(width.max(1), height.max(1))
        .ok_or_else(|| Error::RenderError(format!("Failed to allocate {width}x{height} Pixmap")))?;

    let bg = options.background;
    pixmap.fill(Color::from_rgba8(bg.r, bg.g, bg.b, bg.a));

    let root_transform = Affine::scale(scale);
    let mut state = RasterState {
        fonts,
        images,
        options,
        glyph_count: 0,
    };

    draw_items(
        &page.items,
        root_transform,
        1.0,
        None,
        0,
        &mut state,
        &mut pixmap,
    )?;

    // Convert tiny-skia's premultiplied RGBA8 buffer to straight/unpremultiplied RGBA8
    let mut rgba = Vec::with_capacity((pixmap.width() * pixmap.height() * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }

    Ok(RasterImage {
        width: pixmap.width(),
        height: pixmap.height(),
        data: rgba,
    })
}

/// Rasterize a [`LayoutPage`] and encode it as a deterministic PNG byte vector.
///
/// # Errors
///
/// Returns an [`Error`] if rasterization or PNG encoding fails.
pub fn render_page_png(
    page: &LayoutPage,
    fonts: &FontStore,
    images: &ImageStore,
    options: &RasterOptions,
) -> Result<Vec<u8>> {
    let raster = render_page_rgba(page, fonts, images, options)?;
    encode_deterministic_png(&raster)
}

/// Encode a [`RasterImage`] into a deterministic PNG byte vector (no timestamps or platform metadata).
///
/// # Errors
///
/// Returns [`Error::RenderError`] if PNG encoding fails.
pub fn encode_deterministic_png(raster: &RasterImage) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, raster.width, raster.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::default());
        let mut writer = encoder
            .write_header()
            .map_err(|e| Error::RenderError(format!("PNG header error: {e}")))?;
        writer
            .write_image_data(&raster.data)
            .map_err(|e| Error::RenderError(format!("PNG write error: {e}")))?;
    }
    Ok(buf)
}

/// Rasterize all pages of a [`LayoutDocument`] into a deterministic multi-page TIFF (`RGBA8` with Deflate compression).
///
/// # Errors
///
/// Returns an [`Error`] if page count exceeds `options.layout_options.max_pages`, if rasterization
/// fails, or if TIFF encoding fails.
pub fn export_document_tiff(doc: &LayoutDocument, options: &RasterOptions) -> Result<Vec<u8>> {
    if doc.pages.len() > options.layout_options.max_pages {
        return Err(Error::InvalidInput(format!(
            "Document page count ({}) exceeds max_pages ({})",
            doc.pages.len(),
            options.layout_options.max_pages
        )));
    }

    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut tiff = TiffEncoder::new(&mut cursor)
            .map_err(|e| Error::RenderError(format!("TIFF header error: {e}")))?
            .with_compression(TiffCompression::Deflate(DeflateLevel::Balanced));

        for page in &doc.pages {
            let raster = render_page_rgba(page, &doc.fonts, &doc.images, options)?;
            tiff.write_image::<colortype::RGBA8>(raster.width, raster.height, &raster.data)
                .map_err(|e| Error::RenderError(format!("TIFF page write error: {e}")))?;
        }
    }

    Ok(cursor.into_inner())
}

fn draw_items(
    items: &[Item],
    parent_transform: Affine,
    parent_opacity: f32,
    clip_mask: Option<&Mask>,
    depth: u32,
    state: &mut RasterState<'_>,
    pixmap: &mut Pixmap,
) -> Result<()> {
    if depth > state.options.layout_options.max_recursion_depth {
        return Err(Error::InvalidInput(format!(
            "Group nesting depth ({depth}) exceeds max_recursion_depth ({})",
            state.options.layout_options.max_recursion_depth
        )));
    }

    for item in items {
        match item {
            Item::Glyphs(run) => {
                draw_glyph_run(
                    run,
                    parent_transform,
                    parent_opacity,
                    clip_mask,
                    state,
                    pixmap,
                )?;
            }
            Item::Path(path) => {
                draw_path_item(
                    path,
                    parent_transform,
                    parent_opacity,
                    clip_mask,
                    state,
                    pixmap,
                );
            }
            Item::Image(img) => {
                draw_image_item(
                    img,
                    parent_transform,
                    parent_opacity,
                    clip_mask,
                    state,
                    pixmap,
                );
            }
            Item::Group(group) => {
                draw_group_item(
                    group,
                    parent_transform,
                    parent_opacity,
                    clip_mask,
                    depth,
                    state,
                    pixmap,
                )?;
            }
            Item::Link(_) | Item::Anchor(_) => {}
        }
    }
    Ok(())
}

fn draw_glyph_run(
    run: &GlyphRun,
    parent_transform: Affine,
    parent_opacity: f32,
    clip_mask: Option<&Mask>,
    state: &mut RasterState<'_>,
    pixmap: &mut Pixmap,
) -> Result<()> {
    state.glyph_count = state.glyph_count.saturating_add(run.glyphs.len());
    if state.glyph_count > state.options.layout_options.max_glyphs_per_page {
        return Err(Error::InvalidInput(format!(
            "Page glyph count ({}) exceeds max_glyphs_per_page ({})",
            state.glyph_count, state.options.layout_options.max_glyphs_per_page
        )));
    }

    let run_world = parent_transform.then(run.transform);
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
    {
        let ascent = run.size * 0.8;
        fill_local_rect(
            Rect::new(0.0, -ascent, run_advance, run.size),
            hl,
            parent_opacity,
            run_world,
            clip_mask,
            state.options.anti_alias,
            pixmap,
        );
    }

    let mut paint = Paint {
        anti_alias: state.options.anti_alias,
        ..Paint::default()
    };
    let color = apply_opacity(run.color, parent_opacity);
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);

    let glyph_scale = run.size.max(0.1) / NORMALIZED_UPEM;
    for glyph in &run.glyphs {
        let Some(commands) = state.fonts.glyph_outline(run.font, glyph.glyph_id) else {
            continue;
        };
        let Some(skia_path) = build_skia_path(&commands) else {
            continue;
        };
        let glyph_transform = run_world
            .then(Affine::translate(glyph.x, glyph.y))
            .then(Affine::scale(glyph_scale));
        pixmap.fill_path(
            &skia_path,
            &paint,
            SkiaFillRule::Winding,
            to_skia_transform(glyph_transform),
            clip_mask,
        );
    }

    if let Some(ul) = run.decorations.underline
        && run_advance > 0.0
    {
        draw_line_decoration(
            ul,
            run_advance,
            parent_opacity,
            run_world,
            clip_mask,
            state.options.anti_alias,
            pixmap,
        );
    }
    if let Some(st) = run.decorations.strikethrough
        && run_advance > 0.0
    {
        draw_line_decoration(
            st,
            run_advance,
            parent_opacity,
            run_world,
            clip_mask,
            state.options.anti_alias,
            pixmap,
        );
    }

    Ok(())
}

fn draw_line_decoration(
    dec: LineDecoration,
    width: f32,
    parent_opacity: f32,
    run_world: Affine,
    clip_mask: Option<&Mask>,
    anti_alias: bool,
    pixmap: &mut Pixmap,
) {
    let rect = Rect::new(
        0.0,
        dec.offset - dec.thickness * 0.5,
        width,
        dec.thickness.max(0.25),
    );
    fill_local_rect(
        rect,
        dec.color,
        parent_opacity,
        run_world,
        clip_mask,
        anti_alias,
        pixmap,
    );
}

fn fill_local_rect(
    rect: Rect,
    color: Rgba,
    parent_opacity: f32,
    world: Affine,
    clip_mask: Option<&Mask>,
    anti_alias: bool,
    pixmap: &mut Pixmap,
) {
    let cmds = [
        PathCommand::MoveTo(prism_layout::Point::new(rect.x, rect.y)),
        PathCommand::LineTo(prism_layout::Point::new(rect.right(), rect.y)),
        PathCommand::LineTo(prism_layout::Point::new(rect.right(), rect.bottom())),
        PathCommand::LineTo(prism_layout::Point::new(rect.x, rect.bottom())),
        PathCommand::Close,
    ];
    let Some(path) = build_skia_path(&cmds) else {
        return;
    };
    let mut paint = Paint {
        anti_alias,
        ..Paint::default()
    };
    let c = apply_opacity(color, parent_opacity);
    paint.set_color_rgba8(c.r, c.g, c.b, c.a);
    pixmap.fill_path(
        &path,
        &paint,
        SkiaFillRule::Winding,
        to_skia_transform(world),
        clip_mask,
    );
}

fn draw_path_item(
    path_item: &PathItem,
    parent_transform: Affine,
    parent_opacity: f32,
    clip_mask: Option<&Mask>,
    state: &RasterState<'_>,
    pixmap: &mut Pixmap,
) {
    let Some(skia_path) = build_skia_path(&path_item.commands) else {
        return;
    };
    let world = parent_transform.then(path_item.transform);
    let skia_transform = to_skia_transform(world);

    if let Some(fill) = &path_item.fill {
        let mut paint = Paint {
            anti_alias: state.options.anti_alias,
            ..Paint::default()
        };
        let configured = match fill {
            FillPaint::Solid(color) => {
                let c = apply_opacity(*color, parent_opacity);
                paint.set_color_rgba8(c.r, c.g, c.b, c.a);
                true
            }
            FillPaint::LinearGradient(grad) => {
                let stops = build_skia_stops(&grad.stops, parent_opacity);
                if let Some(shader) = SkiaLinearGradient::new(
                    SkiaPoint::from_xy(grad.start.x, grad.start.y),
                    SkiaPoint::from_xy(grad.end.x, grad.end.y),
                    stops,
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    paint.shader = shader;
                    true
                } else {
                    false
                }
            }
            FillPaint::RadialGradient(grad) => {
                let stops = build_skia_stops(&grad.stops, parent_opacity);
                if let Some(shader) = SkiaRadialGradient::new(
                    SkiaPoint::from_xy(grad.center.x, grad.center.y),
                    0.0,
                    SkiaPoint::from_xy(grad.center.x, grad.center.y),
                    grad.radius.max(0.1),
                    stops,
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    paint.shader = shader;
                    true
                } else {
                    false
                }
            }
        };

        if configured {
            let rule = match path_item.fill_rule {
                FillRule::NonZero => SkiaFillRule::Winding,
                FillRule::EvenOdd => SkiaFillRule::EvenOdd,
            };
            pixmap.fill_path(&skia_path, &paint, rule, skia_transform, clip_mask);
        }
    }

    if let Some(stroke_style) = &path_item.stroke
        && stroke_style.width > 0.0
    {
        let mut paint = Paint {
            anti_alias: state.options.anti_alias,
            ..Paint::default()
        };
        let c = apply_opacity(stroke_style.color, parent_opacity);
        paint.set_color_rgba8(c.r, c.g, c.b, c.a);

        let mut stroke = Stroke {
            width: stroke_style.width,
            line_cap: match stroke_style.line_cap {
                LineCap::Butt => SkiaLineCap::Butt,
                LineCap::Round => SkiaLineCap::Round,
                LineCap::Square => SkiaLineCap::Square,
            },
            line_join: match stroke_style.line_join {
                LineJoin::Miter => SkiaLineJoin::Miter,
                LineJoin::Round => SkiaLineJoin::Round,
                LineJoin::Bevel => SkiaLineJoin::Bevel,
            },
            ..Stroke::default()
        };
        if !stroke_style.dash_array.is_empty() {
            stroke.dash =
                StrokeDash::new(stroke_style.dash_array.clone(), stroke_style.dash_offset);
        }
        pixmap.stroke_path(&skia_path, &paint, &stroke, skia_transform, clip_mask);
    }
}

fn build_skia_stops(
    stops: &[prism_layout::GradientStop],
    parent_opacity: f32,
) -> Vec<SkiaGradientStop> {
    if stops.is_empty() {
        return vec![
            SkiaGradientStop::new(0.0, Color::BLACK),
            SkiaGradientStop::new(1.0, Color::BLACK),
        ];
    }
    stops
        .iter()
        .map(|s| {
            let c = apply_opacity(s.color, parent_opacity);
            SkiaGradientStop::new(
                s.offset.clamp(0.0, 1.0),
                Color::from_rgba8(c.r, c.g, c.b, c.a),
            )
        })
        .collect()
}

fn draw_image_item(
    img_item: &ImageItem,
    parent_transform: Affine,
    parent_opacity: f32,
    clip_mask: Option<&Mask>,
    state: &RasterState<'_>,
    pixmap: &mut Pixmap,
) {
    if img_item.rect.width <= 0.0 || img_item.rect.height <= 0.0 {
        return;
    }
    let Some(entry) = state.images.get(img_item.image) else {
        return;
    };
    let Some(src_pixmap) = decode_image_to_pixmap(entry) else {
        return;
    };

    #[allow(clippy::cast_precision_loss)]
    let sx = img_item.rect.width / (src_pixmap.width().max(1) as f32);
    #[allow(clippy::cast_precision_loss)]
    let sy = img_item.rect.height / (src_pixmap.height().max(1) as f32);

    let img_world = parent_transform
        .then(img_item.transform)
        .then(Affine::translate(img_item.rect.x, img_item.rect.y))
        .then(Affine::scale_non_uniform(sx, sy));

    let paint = PixmapPaint {
        opacity: (parent_opacity * img_item.opacity).clamp(0.0, 1.0),
        blend_mode: BlendMode::SourceOver,
        quality: FilterQuality::Bilinear,
    };

    pixmap.draw_pixmap(
        0,
        0,
        src_pixmap.as_ref(),
        &paint,
        to_skia_transform(img_world),
        clip_mask,
    );
}

fn decode_image_to_pixmap(entry: &prism_layout::ImageEntry) -> Option<Pixmap> {
    if entry.mime_type == "image/x-rgba8" {
        let expected = (entry.width as usize)
            .checked_mul(entry.height as usize)?
            .checked_mul(4)?;
        if entry.data.len() != expected {
            return None;
        }
        let mut pm = Pixmap::new(entry.width, entry.height)?;
        for (dst, src) in pm
            .pixels_mut()
            .iter_mut()
            .zip(entry.data.as_chunks::<4>().0)
        {
            *dst = Color::from_rgba8(src[0], src[1], src[2], src[3])
                .premultiply()
                .to_color_u8();
        }
        return Some(pm);
    }

    let decoded = image::load_from_memory(&entry.data).ok()?.into_rgba8();
    let (w, h) = decoded.dimensions();
    let mut pm = Pixmap::new(w, h)?;
    for (dst, src) in pm
        .pixels_mut()
        .iter_mut()
        .zip(decoded.as_raw().as_chunks::<4>().0)
    {
        *dst = Color::from_rgba8(src[0], src[1], src[2], src[3])
            .premultiply()
            .to_color_u8();
    }
    Some(pm)
}

fn draw_group_item(
    group: &GroupItem,
    parent_transform: Affine,
    parent_opacity: f32,
    parent_clip: Option<&Mask>,
    depth: u32,
    state: &mut RasterState<'_>,
    pixmap: &mut Pixmap,
) -> Result<()> {
    if group.children.is_empty() {
        return Ok(());
    }
    let group_world = parent_transform.then(group.transform);
    let group_opacity = if group.opacity <= 0.0 {
        parent_opacity
    } else {
        (parent_opacity * group.opacity).clamp(0.0, 1.0)
    };

    if let Some(ClipPath {
        commands,
        fill_rule,
    }) = &group.clip
        && let Some(clip_path) = build_skia_path(commands)
    {
        let mut local_mask = Mask::new(pixmap.width(), pixmap.height())
            .ok_or_else(|| Error::RenderError("Failed to allocate clip mask".to_string()))?;
        let rule = match fill_rule {
            FillRule::NonZero => SkiaFillRule::Winding,
            FillRule::EvenOdd => SkiaFillRule::EvenOdd,
        };
        local_mask.fill_path(
            &clip_path,
            rule,
            state.options.anti_alias,
            to_skia_transform(group_world),
        );
        if let Some(existing) = parent_clip {
            for (dst, &src) in local_mask.data_mut().iter_mut().zip(existing.data()) {
                *dst = u8::try_from((u16::from(*dst) * u16::from(src)) / 255).unwrap_or(0);
            }
        }
        draw_items(
            &group.children,
            group_world,
            group_opacity,
            Some(&local_mask),
            depth + 1,
            state,
            pixmap,
        )
    } else {
        draw_items(
            &group.children,
            group_world,
            group_opacity,
            parent_clip,
            depth + 1,
            state,
            pixmap,
        )
    }
}

fn build_skia_path(commands: &[PathCommand]) -> Option<Path> {
    if commands.is_empty() {
        return None;
    }
    let mut pb = PathBuilder::new();
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

fn to_skia_transform(affine: Affine) -> Transform {
    let [a, b, c, d, tx, ty] = affine.coeffs;
    Transform::from_row(a, b, c, d, tx, ty)
}

fn apply_opacity(color: Rgba, opacity: f32) -> Rgba {
    if (opacity - 1.0).abs() <= 1e-5 {
        return color;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let a = (f32::from(color.a) * opacity.clamp(0.0, 1.0)).round() as u8;
    Rgba::new(color.r, color.g, color.b, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_layout::{
        FontSystem, LineSpacing, ParagraphSpec, Point, Size, SpanSpec, StrokeStyle, TextAlign,
        TextLayoutEngine,
    };

    #[test]
    fn test_raster_png_determinism_and_non_blank_rendering() {
        let mut font_sys = FontSystem::new(&LayoutOptions::default());
        let mut engine = TextLayoutEngine::new();

        let spec = ParagraphSpec {
            spans: vec![SpanSpec {
                text: "Deterministic PNG Raster".to_string(),
                family: "Arial".to_string(),
                size: 18.0,
                bold: true,
                color: Rgba::rgb(0x20, 0x40, 0x80),
                ..SpanSpec::default()
            }],
            align: TextAlign::Left,
            line_spacing: LineSpacing::Multiple(1.2),
            max_width: Some(300.0),
            first_line_indent: 0.0,
            origin: Point::new(20.0, 20.0),
            page_index: 0,
            block_index: 0,
        };

        let shaped = engine.layout_paragraph(&spec, &mut font_sys);
        let mut page = LayoutPage::new(0, Size::new(260.0, 100.0));
        page.items.push(Item::Path(PathItem {
            commands: vec![
                PathCommand::MoveTo(Point::new(10.0, 10.0)),
                PathCommand::LineTo(Point::new(250.0, 10.0)),
                PathCommand::LineTo(Point::new(250.0, 90.0)),
                PathCommand::LineTo(Point::new(10.0, 90.0)),
                PathCommand::Close,
            ],
            fill: Some(FillPaint::Solid(Rgba::rgb(240, 245, 255))),
            fill_rule: FillRule::NonZero,
            stroke: Some(StrokeStyle {
                color: Rgba::rgb(0x20, 0x40, 0x80),
                width: 1.5,
                ..StrokeStyle::default()
            }),
            transform: Affine::IDENTITY,
        }));
        for run in shaped.runs {
            page.items.push(Item::Glyphs(run));
        }

        let images = ImageStore::new();
        let opts = RasterOptions {
            dpi: 72.0,
            ..RasterOptions::default()
        };

        let png1 = render_page_png(&page, &font_sys.store, &images, &opts).expect("png1");
        let png2 = render_page_png(&page, &font_sys.store, &images, &opts).expect("png2");
        assert_eq!(png1, png2, "PNG output must be byte-identical across runs");
        assert!(png1.starts_with(b"\x89PNG\r\n\x1a\n"));

        let rgba = render_page_rgba(&page, &font_sys.store, &images, &opts).expect("rgba");
        assert_eq!(rgba.width, 260);
        assert_eq!(rgba.height, 100);
        // Ensure non-white pixels were drawn
        let non_white = rgba
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] != 255 || px[1] != 255 || px[2] != 255)
            .count();
        assert!(
            non_white > 500,
            "Expected non-white drawn pixels, got {non_white}"
        );
    }

    #[test]
    fn test_raster_enforces_max_image_pixels() {
        let page = LayoutPage::new(0, Size::new(5000.0, 5000.0));
        let fonts = FontStore::new();
        let images = ImageStore::new();
        let opts = RasterOptions {
            dpi: 72.0,
            layout_options: LayoutOptions {
                max_image_pixels: 1_000_000,
                ..LayoutOptions::default()
            },
            ..RasterOptions::default()
        };

        assert!(render_page_rgba(&page, &fonts, &images, &opts).is_err());
    }

    #[test]
    fn test_multi_page_tiff_export_and_determinism() {
        let mut doc = LayoutDocument::new();
        doc.pages.push(LayoutPage::new(0, Size::new(144.0, 72.0)));
        doc.pages.push(LayoutPage::new(1, Size::new(144.0, 72.0)));

        let opts = RasterOptions {
            dpi: 72.0,
            ..RasterOptions::default()
        };
        let tiff1 = export_document_tiff(&doc, &opts).expect("export tiff 1");
        let tiff2 = export_document_tiff(&doc, &opts).expect("export tiff 2");
        assert_eq!(tiff1, tiff2, "Multi-page TIFF must be byte-identical");

        // Decode with `tiff::decoder::Decoder` and verify both IFD pages exist
        let mut decoder =
            tiff::decoder::Decoder::new(std::io::Cursor::new(&tiff1)).expect("open tiff");
        assert_eq!(decoder.dimensions().expect("dims page 1"), (144, 72));
        assert!(
            decoder.more_images(),
            "expected second page in multi-page TIFF"
        );
        decoder.next_image().expect("advance to page 2");
        assert_eq!(decoder.dimensions().expect("dims page 2"), (144, 72));
        assert!(!decoder.more_images());
    }
}
