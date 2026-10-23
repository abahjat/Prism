// SPDX-License-Identifier: AGPL-3.0-only
//! Pure-Rust PDF page interpretation into [`LayoutDocument`] using `hayro-interpret` and `hayro-syntax`.
//!
//! Converts PDF pages directly into positioned [`LayoutPage`] items (`Item::Glyphs`, `Item::Path`,
//! `Item::Image`, `Item::Group`, `Item::Link`) so that SVG viewing, PNG/TIFF rasterization,
//! PDF export, reading-order text extraction, character bounding boxes, and search all share a
//! single deterministic representation.

use crate::fonts::{strip_subset_prefix, substitute_font_family};
use crate::model::{
    Affine, ClipPath, Cluster, Decorations, FillPaint, FillRule, FontId, FontReport,
    FontSourceKind, FontStore, GlyphRun, GroupItem, ImageItem, ImageStore, Item, LayoutDocument,
    LayoutOptions, LayoutPage, LineCap, LineJoin, PathCommand, PathItem, Point, PositionedGlyph,
    Rect, Rgba, Size, SourceRef, StrokeStyle, normalize_zero,
};
use hayro_interpret::font::{
    FontData as HayroFontData, FontQuery, Glyph, GlyphRun as HayroGlyphRun, OutlineGlyph,
    StandardFont,
};
use hayro_interpret::hayro_cmap::BfString;
use hayro_interpret::{
    BlendMode, ClipPath as HayroClipPath, Context, Device, DrawMode, DrawProps,
    FillRule as HayroFillRule, Image, ImageData, ImageDrawProps, InterpreterCache,
    InterpreterSettings, Paint, SoftMask, StrokeProps, TransformExt, interpret_page,
};
use hayro_syntax::object::dict::keys::{A, ANNOTS, RECT, SUBTYPE, URI};
use hayro_syntax::object::{Array, Dict, Name, Object, Rect as HayroRect};
use hayro_syntax::{LoadPdfError, Pdf};
use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};
use kurbo::{Affine as KurboAffine, BezPath, Cap, Join, PathEl, Point as KurboPoint};
use prism_core::error::{Error, Result};
use std::collections::BTreeMap;
use std::sync::Arc;

const LIBERATION_SANS_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");
const LIBERATION_SANS_BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf");
const LIBERATION_SANS_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSans-Italic.ttf");
const LIBERATION_SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSans-BoldItalic.ttf");
const LIBERATION_SERIF_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSerif-Regular.ttf");
const LIBERATION_SERIF_BOLD: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSerif-Bold.ttf");
const LIBERATION_SERIF_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSerif-Italic.ttf");
const LIBERATION_SERIF_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSerif-BoldItalic.ttf");
const LIBERATION_MONO_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationMono-Regular.ttf");
const LIBERATION_MONO_BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-Bold.ttf");
const LIBERATION_MONO_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationMono-Italic.ttf");
const LIBERATION_MONO_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationMono-BoldItalic.ttf");
const NOTO_SANS_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/NotoSans-Regular.ttf");

/// Interpret all pages of a PDF byte slice into a positioned [`LayoutDocument`].
///
/// # Errors
///
/// Returns [`Error::Encrypted`] if the PDF is encrypted with a non-empty password,
/// or [`Error::ParseError`] / [`Error::InvalidInput`] if the PDF structure is unreadable
/// or exceeds [`LayoutOptions`] resource caps.
pub fn interpret_pdf_bytes(pdf_bytes: &[u8], options: &LayoutOptions) -> Result<LayoutDocument> {
    interpret_pdf_pages(pdf_bytes, options, None)
}

/// Interpret a subset of pages (or all pages when `page_indices` is `None`) of a PDF byte slice
/// into a [`LayoutDocument`].
///
/// # Errors
///
/// Returns an error if the PDF cannot be parsed, requires a password, or exceeds resource limits.
pub fn interpret_pdf_pages(
    pdf_bytes: &[u8],
    options: &LayoutOptions,
    page_indices: Option<&[u32]>,
) -> Result<LayoutDocument> {
    let pdf = Pdf::new(pdf_bytes.to_vec()).map_err(|err| match err {
        LoadPdfError::Decryption(e) => Error::Encrypted(format!("PDF requires a password: {e:?}")),
        LoadPdfError::Invalid => {
            Error::ParseError("Invalid or corrupted PDF structure".to_string())
        }
    })?;

    let settings = build_interpreter_settings();
    let cache = InterpreterCache::new();

    let mut fonts = FontStore::new();
    let mut font_report = FontReport::default();
    let mut images = ImageStore::new();
    let mut font_cache_map: BTreeMap<u128, FontId> = BTreeMap::new();
    let mut fallback_font_id: Option<FontId> = None;

    let pdf_pages = pdf.pages();
    // Reject before interpreting anything, so an oversized PDF costs no layout work
    if pdf_pages.len() > options.max_pages {
        return Err(Error::InvalidInput(format!(
            "PDF page count ({}) exceeds max_pages ({})",
            pdf_pages.len(),
            options.max_pages
        )));
    }
    let mut out_pages = Vec::new();

    for (idx, pdf_page) in pdf_pages.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let page_index = idx as u32;
        if let Some(filter) = page_indices
            && !filter.contains(&page_index)
        {
            continue;
        }

        let (width, height) = pdf_page.render_dimensions();
        let page_size = Size::new(width.max(1.0), height.max(1.0));
        let init_transform = pdf_page.initial_transform(true).to_kurbo();
        let page_bbox = kurbo::Rect::new(
            0.0,
            0.0,
            f64::from(page_size.width),
            f64::from(page_size.height),
        );

        let mut context = Context::new(
            init_transform,
            page_bbox,
            &cache,
            pdf.xref(),
            settings.clone(),
        );

        let mut device = PdfLayoutDevice {
            page_index,
            options,
            fonts: &mut fonts,
            font_report: &mut font_report,
            images: &mut images,
            font_cache_map: &mut font_cache_map,
            fallback_font_id: &mut fallback_font_id,
            root_items: Vec::new(),
            group_stack: Vec::new(),
            run_counter: 0,
            glyph_count: 0,
            limit_error: None,
        };

        interpret_page(pdf_page, &mut context, &mut device);

        if let Some(err) = device.limit_error.take() {
            return Err(err);
        }

        device.flush_unclosed_groups();
        extract_pdf_link_annotations(pdf_page, init_transform, &mut device.root_items);

        out_pages.push(LayoutPage {
            index: page_index,
            size: page_size,
            items: device.root_items,
            label: Some(format!("Page {}", page_index + 1)),
        });
    }

    Ok(LayoutDocument {
        pages: out_pages,
        fonts,
        images,
        font_report,
    })
}

fn build_interpreter_settings() -> InterpreterSettings {
    let default_settings = InterpreterSettings::default();
    let default_resolver = default_settings.font_resolver.clone();

    InterpreterSettings {
        font_resolver: Arc::new(move |query| {
            let static_bytes: &'static [u8] = match query {
                FontQuery::Standard(std_font) => standard_font_to_bundled(*std_font),
                FontQuery::Fallback(fb) => standard_font_to_bundled(fb.pick_standard_font()),
            };
            if !static_bytes.is_empty() {
                let data: HayroFontData = Arc::new(static_bytes);
                return Some((data, 0));
            }
            (default_resolver)(query)
        }),
        ..default_settings
    }
}

fn standard_font_to_bundled(std_font: StandardFont) -> &'static [u8] {
    match std_font {
        StandardFont::Helvetica => LIBERATION_SANS_REGULAR,
        StandardFont::HelveticaBold => LIBERATION_SANS_BOLD,
        StandardFont::HelveticaOblique => LIBERATION_SANS_ITALIC,
        StandardFont::HelveticaBoldOblique => LIBERATION_SANS_BOLD_ITALIC,
        StandardFont::TimesRoman => LIBERATION_SERIF_REGULAR,
        StandardFont::TimesBold => LIBERATION_SERIF_BOLD,
        StandardFont::TimesItalic => LIBERATION_SERIF_ITALIC,
        StandardFont::TimesBoldItalic => LIBERATION_SERIF_BOLD_ITALIC,
        StandardFont::Courier => LIBERATION_MONO_REGULAR,
        StandardFont::CourierBold => LIBERATION_MONO_BOLD,
        StandardFont::CourierOblique => LIBERATION_MONO_ITALIC,
        StandardFont::CourierBoldOblique => LIBERATION_MONO_BOLD_ITALIC,
        // Use Hayro's embedded Foxit Symbol/ZapfDingbats CFF fonts for exact symbol glyph mapping
        StandardFont::Symbol | StandardFont::ZapfDingBats => &[],
    }
}

struct GroupFrame {
    clip: Option<ClipPath>,
    opacity: f32,
    items: Vec<Item>,
}

struct PdfLayoutDevice<'b> {
    page_index: u32,
    options: &'b LayoutOptions,
    fonts: &'b mut FontStore,
    font_report: &'b mut FontReport,
    images: &'b mut ImageStore,
    font_cache_map: &'b mut BTreeMap<u128, FontId>,
    fallback_font_id: &'b mut Option<FontId>,
    root_items: Vec<Item>,
    group_stack: Vec<GroupFrame>,
    run_counter: u32,
    glyph_count: usize,
    limit_error: Option<Error>,
}

impl PdfLayoutDevice<'_> {
    fn current_items_mut(&mut self) -> &mut Vec<Item> {
        if let Some(top) = self.group_stack.last_mut() {
            &mut top.items
        } else {
            &mut self.root_items
        }
    }

    fn flush_unclosed_groups(&mut self) {
        while let Some(frame) = self.group_stack.pop() {
            if !frame.items.is_empty() {
                self.current_items_mut().push(Item::Group(GroupItem {
                    transform: Affine::IDENTITY,
                    clip: frame.clip,
                    opacity: frame.opacity,
                    children: frame.items,
                }));
            }
        }
    }

    fn resolve_outline_font(&mut self, glyph: &OutlineGlyph) -> FontId {
        let key = glyph.font_cache_key();
        if let Some(&existing) = self.font_cache_map.get(&key) {
            return existing;
        }

        if let Some(font_data) = glyph.font_data() {
            let raw_name = font_data
                .postscript_name
                .as_deref()
                .unwrap_or("PDF-EmbeddedFont");
            let clean_name = strip_subset_prefix(raw_name);
            let weight =
                u16::try_from(font_data.weight.unwrap_or(400).clamp(100, 900)).unwrap_or(400);
            let italic = font_data.is_italic;
            let bytes = (*font_data.data).as_ref().to_vec();

            // Determine whether this font came from our bundled standard-font resolver or was embedded in the PDF
            let (sub_name, was_sub) = substitute_font_family(clean_name);
            let is_bundled_sub = was_sub
                && (bytes == LIBERATION_SANS_REGULAR
                    || bytes == LIBERATION_SANS_BOLD
                    || bytes == LIBERATION_SANS_ITALIC
                    || bytes == LIBERATION_SANS_BOLD_ITALIC
                    || bytes == LIBERATION_SERIF_REGULAR
                    || bytes == LIBERATION_SERIF_BOLD
                    || bytes == LIBERATION_SERIF_ITALIC
                    || bytes == LIBERATION_SERIF_BOLD_ITALIC
                    || bytes == LIBERATION_MONO_REGULAR
                    || bytes == LIBERATION_MONO_BOLD
                    || bytes == LIBERATION_MONO_ITALIC
                    || bytes == LIBERATION_MONO_BOLD_ITALIC);

            let (resolved_family, source_kind) = if is_bundled_sub {
                (sub_name.to_string(), FontSourceKind::Bundled)
            } else {
                (clean_name.to_string(), FontSourceKind::Embedded)
            };

            self.font_report
                .record(clean_name.to_string(), resolved_family.clone(), source_kind);

            let id =
                self.fonts
                    .register_ttf(resolved_family, weight, italic, source_kind, bytes, 0);
            self.font_cache_map.insert(key, id);
            return id;
        }

        // Type1 or CFF font without raw TrueType table bytes: register an outline-backed FontEntry
        let family_name = format!("PDF-Type1Font-{}", self.font_cache_map.len() + 1);
        self.font_report.record(
            family_name.clone(),
            family_name.clone(),
            FontSourceKind::SynthesizedOutlines,
        );
        let id = self.fonts.register_ttf(
            family_name,
            400,
            false,
            FontSourceKind::SynthesizedOutlines,
            Vec::new(),
            0,
        );
        self.font_cache_map.insert(key, id);
        id
    }

    fn ensure_fallback_font(&mut self) -> FontId {
        if let Some(id) = *self.fallback_font_id {
            return id;
        }
        let id = self.fonts.register_ttf(
            "Noto Sans",
            400,
            false,
            FontSourceKind::Bundled,
            NOTO_SANS_REGULAR.to_vec(),
            0,
        );
        *self.fallback_font_id = Some(id);
        id
    }
}

impl<'a> Device<'a> for PdfLayoutDevice<'_> {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        if self.limit_error.is_some() {
            return;
        }
        let commands = convert_bezpath(path, false);
        if commands.is_empty() {
            return;
        }

        let paint_color = extract_paint_color(&props.paint);
        let (fill, fill_rule, stroke) = match draw_mode {
            DrawMode::Fill(rule) => (
                Some(FillPaint::Solid(paint_color)),
                convert_fill_rule(*rule),
                None,
            ),
            DrawMode::Stroke(sp) => (
                None,
                FillRule::NonZero,
                Some(convert_stroke_props(sp, paint_color)),
            ),
            DrawMode::FillAndStroke(rule, sp) => (
                Some(FillPaint::Solid(paint_color)),
                convert_fill_rule(*rule),
                Some(convert_stroke_props(sp, paint_color)),
            ),
            DrawMode::Invisible => return,
        };

        if fill.is_none() && stroke.is_none() {
            return;
        }

        let transform = convert_affine(props.transform);
        self.current_items_mut().push(Item::Path(PathItem {
            commands,
            fill,
            fill_rule,
            stroke,
            transform,
        }));
    }

    fn push_clip_path(&mut self, clip_path: &HayroClipPath) {
        if self.limit_error.is_some() {
            return;
        }
        if self.group_stack.len() >= self.options.max_recursion_depth as usize {
            self.limit_error = Some(Error::InvalidInput(format!(
                "PDF clip nesting depth exceeds max_recursion_depth ({})",
                self.options.max_recursion_depth
            )));
            return;
        }
        let commands = convert_bezpath(&clip_path.path, false);
        let clip = (!commands.is_empty()).then(|| ClipPath {
            commands,
            fill_rule: convert_fill_rule(clip_path.fill),
        });
        self.group_stack.push(GroupFrame {
            clip,
            opacity: 1.0,
            items: Vec::new(),
        });
    }

    fn push_transparency_group(
        &mut self,
        opacity: f32,
        _mask: Option<SoftMask<'a>>,
        _blend_mode: BlendMode,
    ) {
        if self.limit_error.is_some() {
            return;
        }
        if self.group_stack.len() >= self.options.max_recursion_depth as usize {
            self.limit_error = Some(Error::InvalidInput(format!(
                "PDF transparency group nesting depth exceeds max_recursion_depth ({})",
                self.options.max_recursion_depth
            )));
            return;
        }
        self.group_stack.push(GroupFrame {
            clip: None,
            opacity: normalize_zero(opacity.clamp(0.0, 1.0)),
            items: Vec::new(),
        });
    }

    fn pop_clip(&mut self) {
        if let Some(frame) = self.group_stack.pop()
            && !frame.items.is_empty()
        {
            self.current_items_mut().push(Item::Group(GroupItem {
                transform: Affine::IDENTITY,
                clip: frame.clip,
                opacity: frame.opacity,
                children: frame.items,
            }));
        }
    }

    fn pop_transparency_group(&mut self) {
        self.pop_clip();
    }

    #[allow(clippy::too_many_lines)]
    fn draw_glyph_run(
        &mut self,
        glyph_run: &HayroGlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        draw_mode: &DrawMode,
    ) {
        if self.limit_error.is_some() {
            return;
        }
        let glyphs = glyph_run.glyphs();
        if glyphs.is_empty() {
            return;
        }

        self.glyph_count = self.glyph_count.saturating_add(glyphs.len());
        if self.glyph_count > self.options.max_glyphs_per_page {
            self.limit_error = Some(Error::InvalidInput(format!(
                "PDF page glyph count ({}) exceeds max_glyphs_per_page ({})",
                self.glyph_count, self.options.max_glyphs_per_page
            )));
            return;
        }

        let base_color = if matches!(draw_mode, DrawMode::Invisible) {
            Rgba::TRANSPARENT
        } else {
            extract_paint_color(&props.paint)
        };

        // Accumulate segments of OutlineGlyphs that share a font and continuous baseline
        let mut active_segment: Option<ActiveGlyphSegment> = None;

        for pos_glyph in glyphs {
            match &**pos_glyph {
                Glyph::Outline(outline_glyph) => {
                    let font_id = self.resolve_outline_font(outline_glyph);
                    let raw_gid = outline_glyph.glyph_id().to_u32();
                    let gid = u16::try_from(raw_gid).unwrap_or(0);

                    // Cache 1000-upem outline (with +y down) so Type1/CFF/subset fonts render identically
                    if self.fonts.glyph_outline(font_id, gid).is_none() {
                        let bez = outline_glyph.outline();
                        let cmds = convert_bezpath(&bez, true);
                        self.fonts.insert_precomputed_outline(font_id, gid, cmds);
                    }

                    // Combined transform mapping 1000-upem font coords (+y up) -> page coords (+y down)
                    let full_m = props.transform * pos_glyph.transform();
                    let coeffs = full_m.as_coeffs();
                    let sx = coeffs[0].hypot(coeffs[1]) * 1000.0;
                    let sy = coeffs[2].hypot(coeffs[3]) * 1000.0;
                    #[allow(clippy::cast_possible_truncation)]
                    let font_size = normalize_zero((sy.max(sx)).max(0.1) as f32);
                    let scale = f64::from(font_size) / 1000.0;

                    // Convert full_m into our LayoutPage run transform where glyph outlines already have -y
                    // and are scaled by `font_size / 1000.0` at render time.
                    let glyph_scale_m = KurboAffine::new([scale, 0.0, 0.0, -scale, 0.0, 0.0]);
                    let glyph_run_m = full_m * glyph_scale_m.inverse();

                    let adv_upem = outline_glyph.advance_width().unwrap_or(500.0);
                    let adv_pt = normalize_zero(adv_upem * (font_size / 1000.0));
                    let unicode_str = extract_unicode_string(pos_glyph.as_unicode());

                    // Check if we can append to the active segment or should start a new GlyphRun
                    let should_split = if let Some(ref seg) = active_segment {
                        if seg.font_id != font_id || (seg.font_size - font_size).abs() > 0.25 {
                            true
                        } else {
                            let local_pt = seg.inv_run_m * (full_m * KurboPoint::ZERO);
                            #[allow(clippy::cast_possible_truncation)]
                            let lx = local_pt.x as f32;
                            #[allow(clippy::cast_possible_truncation)]
                            let ly = local_pt.y as f32;
                            let gap = lx - seg.expected_next_x;
                            // Split on baseline change, backward jump, or word-space gap in TJ arrays
                            ly.abs() > font_size * 0.35
                                || gap < -font_size * 0.4
                                || gap > font_size * 0.16
                        }
                    } else {
                        true
                    };

                    if should_split {
                        if let Some(finished) = active_segment.take() {
                            self.emit_glyph_segment(finished, base_color);
                        }
                        let inv_run_m = glyph_run_m.inverse();
                        active_segment = Some(ActiveGlyphSegment {
                            font_id,
                            font_size,
                            run_m: glyph_run_m,
                            inv_run_m,
                            glyphs: Vec::new(),
                            text: String::new(),
                            clusters: Vec::new(),
                            expected_next_x: 0.0,
                        });
                    }

                    if let Some(ref mut seg) = active_segment {
                        let local_pt = seg.inv_run_m * (full_m * KurboPoint::ZERO);
                        #[allow(clippy::cast_possible_truncation)]
                        let gx = normalize_zero(local_pt.x as f32);
                        #[allow(clippy::cast_possible_truncation)]
                        let gy = normalize_zero(local_pt.y as f32);

                        // Update previous glyph's advance to match actual delta-x if on same baseline
                        if let Some(prev_g) = seg.glyphs.last_mut()
                            && gx > prev_g.x
                        {
                            let actual_adv = normalize_zero(gx - prev_g.x);
                            prev_g.advance = actual_adv;
                            if let Some(prev_c) = seg.clusters.last_mut() {
                                prev_c.advance = actual_adv;
                            }
                        }

                        let glyph_idx = seg.glyphs.len();
                        seg.glyphs.push(PositionedGlyph {
                            glyph_id: gid,
                            x: gx,
                            y: gy,
                            advance: adv_pt,
                        });

                        if !unicode_str.is_empty() {
                            let start_byte = seg.text.len();
                            seg.text.push_str(&unicode_str);
                            let end_byte = seg.text.len();
                            #[allow(clippy::cast_possible_truncation)]
                            seg.clusters.push(Cluster {
                                text_range: (start_byte as u32)..(end_byte as u32),
                                glyph_range: (glyph_idx as u32)..((glyph_idx + 1) as u32),
                                x: gx,
                                advance: adv_pt,
                            });
                        }

                        seg.expected_next_x = gx + adv_pt;
                    }
                }
                Glyph::Type3(t3_glyph) => {
                    if let Some(finished) = active_segment.take() {
                        self.emit_glyph_segment(finished, base_color);
                    }
                    // Interpret the Type3 glyph's drawing operations into this device
                    t3_glyph.interpret(self, props.transform, pos_glyph.transform(), &props.paint);

                    // Also emit an invisible searchable GlyphRun if the Type3 glyph has a Unicode mapping
                    let unicode_str = extract_unicode_string(t3_glyph.as_unicode());
                    if !unicode_str.is_empty() {
                        let fallback_font = self.ensure_fallback_font();
                        let full_m = props.transform * pos_glyph.transform();
                        let origin = full_m * KurboPoint::ZERO;
                        let coeffs = full_m.as_coeffs();
                        let sy = coeffs[2].hypot(coeffs[3]) * 1000.0;
                        #[allow(clippy::cast_possible_truncation)]
                        let font_size = normalize_zero(sy.max(6.0) as f32);
                        let adv = font_size * 0.5;
                        let run_idx = self.run_counter;
                        self.run_counter += 1;
                        #[allow(clippy::cast_possible_truncation)]
                        let text_len = unicode_str.len() as u32;
                        #[allow(clippy::cast_possible_truncation)]
                        let ox = normalize_zero(origin.x as f32);
                        #[allow(clippy::cast_possible_truncation)]
                        let oy = normalize_zero(origin.y as f32);
                        let page_index = self.page_index;
                        self.current_items_mut().push(Item::Glyphs(GlyphRun {
                            font: fallback_font,
                            size: font_size,
                            color: Rgba::TRANSPARENT,
                            transform: Affine::translate(ox, oy),
                            glyphs: Vec::new(),
                            text: unicode_str,
                            clusters: vec![Cluster {
                                text_range: 0..text_len,
                                glyph_range: 0..0,
                                x: 0.0,
                                advance: adv,
                            }],
                            decorations: Decorations::default(),
                            source: SourceRef::new(page_index, 0, run_idx),
                        }));
                    }
                }
            }
        }

        if let Some(finished) = active_segment.take() {
            self.emit_glyph_segment(finished, base_color);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        if self.limit_error.is_some() {
            return;
        }

        let mut encoded_png: Option<(u32, u32, Vec<u8>)> = None;
        let mut limit_err: Option<Error> = None;
        let max_pixels = self.options.max_image_pixels;

        match image {
            Image::Stencil(stencil_img) => {
                stencil_img.with_stencil(
                    |stencil, paint| {
                        let pixels = u64::from(stencil.width) * u64::from(stencil.height);
                        if pixels > max_pixels {
                            limit_err = Some(Error::InvalidInput(format!(
                                "PDF stencil image dimensions ({}x{} = {pixels} px) exceed max_image_pixels ({max_pixels})",
                                stencil.width, stencil.height
                            )));
                            return;
                        }
                        let tint = extract_paint_color(paint);
                        let mut rgba = Vec::with_capacity(stencil.data.len() * 4);
                        for &alpha in &stencil.data {
                            let eff_a = u16::from(alpha) * u16::from(tint.a) / 255;
                            #[allow(clippy::cast_possible_truncation)]
                            rgba.extend_from_slice(&[tint.r, tint.g, tint.b, eff_a as u8]);
                        }
                        if let Some(png_bytes) =
                            encode_png_bytes(stencil.width, stencil.height, &rgba, ColorType::Rgba8)
                        {
                            encoded_png = Some((stencil.width, stencil.height, png_bytes));
                        }
                    },
                    None,
                );
            }
            Image::Raster(raster_img) => {
                raster_img.with_rgba(
                    |img_data, alpha_opt| {
                        let w = img_data.width();
                        let h = img_data.height();
                        let pixels = u64::from(w) * u64::from(h);
                        if pixels > max_pixels {
                            limit_err = Some(Error::InvalidInput(format!(
                                "PDF raster image dimensions ({w}x{h} = {pixels} px) exceed max_image_pixels ({max_pixels})"
                            )));
                            return;
                        }

                        match (img_data, alpha_opt) {
                            (ImageData::Rgb(rgb), Some(alpha))
                                if alpha.width == w && alpha.height == h =>
                            {
                                let mut rgba = Vec::with_capacity(alpha.data.len() * 4);
                                for (&px, &a) in rgb.data.as_chunks::<3>().0.iter().zip(&alpha.data)
                                {
                                    rgba.extend_from_slice(&[px[0], px[1], px[2], a]);
                                }
                                if let Some(png) = encode_png_bytes(w, h, &rgba, ColorType::Rgba8) {
                                    encoded_png = Some((w, h, png));
                                }
                            }
                            (ImageData::Rgb(rgb), _) => {
                                if let Some(png) = encode_png_bytes(w, h, &rgb.data, ColorType::Rgb8)
                                {
                                    encoded_png = Some((w, h, png));
                                }
                            }
                            (ImageData::Luma(luma), Some(alpha))
                                if alpha.width == w && alpha.height == h =>
                            {
                                let mut rgba = Vec::with_capacity(luma.data.len() * 4);
                                for (&g, &a) in luma.data.iter().zip(&alpha.data) {
                                    rgba.extend_from_slice(&[g, g, g, a]);
                                }
                                if let Some(png) = encode_png_bytes(w, h, &rgba, ColorType::Rgba8) {
                                    encoded_png = Some((w, h, png));
                                }
                            }
                            (ImageData::Luma(luma), _) => {
                                if let Some(png) =
                                    encode_png_bytes(w, h, &luma.data, ColorType::L8)
                                {
                                    encoded_png = Some((w, h, png));
                                }
                            }
                        }
                    },
                    None,
                );
            }
        }

        if let Some(err) = limit_err {
            self.limit_error = Some(err);
            return;
        }

        let Some((width, height, png_bytes)) = encoded_png else {
            return;
        };

        let Ok(image_id) =
            self.images
                .register("image/png", width, height, png_bytes, self.options)
        else {
            return;
        };

        // Decompose pure axis-aligned scale+translation into `rect` + `Affine::IDENTITY` when possible
        let m = convert_affine(props.transform);
        let [ma, mb, mc, md, me, mf] = m.coeffs;
        #[allow(clippy::cast_precision_loss)]
        let w_f32 = width as f32;
        #[allow(clippy::cast_precision_loss)]
        let h_f32 = height as f32;

        let (rect, transform) = if mb.abs() <= 1e-5 && mc.abs() <= 1e-5 && ma > 0.0 && md > 0.0 {
            (
                Rect::new(
                    normalize_zero(me),
                    normalize_zero(mf),
                    normalize_zero(ma * w_f32),
                    normalize_zero(md * h_f32),
                ),
                Affine::IDENTITY,
            )
        } else {
            (Rect::new(0.0, 0.0, w_f32, h_f32), m)
        };

        self.current_items_mut().push(Item::Image(ImageItem {
            image: image_id,
            rect,
            transform,
            opacity: 1.0,
            alt_text: None,
        }));
    }
}

struct ActiveGlyphSegment {
    font_id: FontId,
    font_size: f32,
    run_m: KurboAffine,
    inv_run_m: KurboAffine,
    glyphs: Vec<PositionedGlyph>,
    text: String,
    clusters: Vec<Cluster>,
    expected_next_x: f32,
}

impl PdfLayoutDevice<'_> {
    fn emit_glyph_segment(&mut self, seg: ActiveGlyphSegment, color: Rgba) {
        if seg.glyphs.is_empty() && seg.text.is_empty() {
            return;
        }
        let run_idx = self.run_counter;
        self.run_counter += 1;
        let page_index = self.page_index;

        self.current_items_mut().push(Item::Glyphs(GlyphRun {
            font: seg.font_id,
            size: seg.font_size,
            color,
            transform: convert_affine(seg.run_m),
            glyphs: seg.glyphs,
            text: seg.text,
            clusters: seg.clusters,
            decorations: Decorations::default(),
            source: SourceRef::new(page_index, 0, run_idx),
        }));
    }
}

fn extract_unicode_string(bf: Option<BfString>) -> String {
    match bf {
        Some(BfString::Char('\0')) | None => String::new(),
        Some(BfString::Char(c)) => c.to_string(),
        Some(BfString::String(s)) => s.replace('\0', ""),
    }
}

fn encode_png_bytes(width: u32, height: u32, raw: &[u8], color_type: ColorType) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let encoder = PngEncoder::new(&mut buf);
    encoder
        .write_image(raw, width, height, color_type.into())
        .ok()?;
    Some(buf)
}

fn extract_paint_color(paint: &Paint<'_>) -> Rgba {
    match paint {
        Paint::Color(col) => {
            let [r, g, b, a] = col.to_rgba().to_rgba8();
            Rgba::new(r, g, b, a)
        }
        Paint::Pattern(_) => Rgba::rgb(128, 128, 128),
    }
}

fn convert_fill_rule(rule: HayroFillRule) -> FillRule {
    match rule {
        HayroFillRule::NonZero => FillRule::NonZero,
        HayroFillRule::EvenOdd => FillRule::EvenOdd,
    }
}

fn convert_stroke_props(sp: &StrokeProps, color: Rgba) -> StrokeStyle {
    StrokeStyle {
        color,
        width: normalize_zero(sp.line_width.max(0.1)),
        line_cap: match sp.line_cap {
            Cap::Butt => LineCap::Butt,
            Cap::Round => LineCap::Round,
            Cap::Square => LineCap::Square,
        },
        line_join: match sp.line_join {
            Join::Bevel => LineJoin::Bevel,
            Join::Miter => LineJoin::Miter,
            Join::Round => LineJoin::Round,
        },
        dash_array: sp.dash_array.iter().copied().map(normalize_zero).collect(),
        dash_offset: normalize_zero(sp.dash_offset),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn convert_affine(a: KurboAffine) -> Affine {
    let c = a.as_coeffs();
    Affine::new([
        normalize_zero(c[0] as f32),
        normalize_zero(c[1] as f32),
        normalize_zero(c[2] as f32),
        normalize_zero(c[3] as f32),
        normalize_zero(c[4] as f32),
        normalize_zero(c[5] as f32),
    ])
}

#[allow(clippy::cast_possible_truncation)]
fn convert_bezpath(path: &BezPath, flip_y: bool) -> Vec<PathCommand> {
    let y_sign = if flip_y { -1.0_f32 } else { 1.0_f32 };
    let map_pt = |p: KurboPoint| -> Point {
        Point::new(
            normalize_zero(p.x as f32),
            normalize_zero((p.y as f32) * y_sign),
        )
    };

    let mut out = Vec::with_capacity(path.elements().len());
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => out.push(PathCommand::MoveTo(map_pt(p))),
            PathEl::LineTo(p) => out.push(PathCommand::LineTo(map_pt(p))),
            PathEl::QuadTo(c, p) => out.push(PathCommand::QuadTo {
                ctrl: map_pt(c),
                to: map_pt(p),
            }),
            PathEl::CurveTo(c1, c2, p) => out.push(PathCommand::CurveTo {
                ctrl1: map_pt(c1),
                ctrl2: map_pt(c2),
                to: map_pt(p),
            }),
            PathEl::ClosePath => out.push(PathCommand::Close),
        }
    }
    out
}

fn extract_pdf_link_annotations(
    page: &hayro_syntax::page::Page<'_>,
    init_transform: KurboAffine,
    out_items: &mut Vec<Item>,
) {
    let Some(annot_arr) = page.raw().get::<Array<'_>>(ANNOTS) else {
        return;
    };

    for annot in annot_arr.iter::<Dict<'_>>() {
        let Some(subtype) = annot.get::<Name<'_>>(SUBTYPE) else {
            continue;
        };
        if subtype.as_ref() != b"Link" {
            continue;
        }
        let Some(raw_rect) = annot.get::<HayroRect>(RECT) else {
            continue;
        };
        let Some(action) = annot.get::<Dict<'_>>(A) else {
            continue;
        };
        let Some(uri_obj) = action.get::<Object<'_>>(URI) else {
            continue;
        };
        let Object::String(uri_str) = uri_obj else {
            continue;
        };
        let target = String::from_utf8_lossy(uri_str.as_bytes()).into_owned();
        if target.is_empty() {
            continue;
        }

        let p0 = init_transform * KurboPoint::new(raw_rect.x0, raw_rect.y0);
        let p1 = init_transform * KurboPoint::new(raw_rect.x1, raw_rect.y1);
        #[allow(clippy::cast_possible_truncation)]
        let min_x = normalize_zero(p0.x.min(p1.x) as f32);
        #[allow(clippy::cast_possible_truncation)]
        let min_y = normalize_zero(p0.y.min(p1.y) as f32);
        #[allow(clippy::cast_possible_truncation)]
        let width = normalize_zero((p0.x - p1.x).abs() as f32);
        #[allow(clippy::cast_possible_truncation)]
        let height = normalize_zero((p0.y - p1.y).abs() as f32);

        out_items.push(Item::Link(crate::model::LinkItem {
            rect: Rect::new(min_x, min_y, width, height),
            target,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpret_corpus_pdf_text_and_char_boxes() {
        let pdf_bytes = std::fs::read("../../test-files/testComment.pdf")
            .expect("read test-files/testComment.pdf");
        let doc = interpret_pdf_bytes(&pdf_bytes, &LayoutOptions::default())
            .expect("interpret testComment.pdf");

        assert_eq!(doc.pages.len(), 1);
        let page = &doc.pages[0];
        assert!((page.size.width - 612.0).abs() < 1.0);
        assert!((page.size.height - 792.0).abs() < 1.0);

        let text = page.text();
        assert!(
            text.contains("Here is some text"),
            "expected 'Here is some text' in {text:?}"
        );
        assert!(
            !text.contains("H \nere"),
            "unexpected split 'H \\nere' in {text:?}"
        );

        let boxes = page.char_boxes();
        let reconstructed: String = boxes.iter().map(|b| b.ch).collect();
        assert_eq!(text, reconstructed);

        let hits = doc.find("Here is some", crate::text_ops::SearchOptions::default());
        assert!(!hits.is_empty(), "expected search hit for 'Here is some'");
        assert!(!hits[0].rects.is_empty(), "expected non-empty hit rects");
    }

    /// Build a minimal PDF with `count` empty 200x200 pages.
    fn empty_pages_pdf(count: usize) -> Vec<u8> {
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {count} >>",
                (0..count)
                    .map(|i| format!("{} 0 R", i + 3))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ];
        objects.extend(
            (0..count).map(|_| "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".into()),
        );
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn test_interpret_enforces_max_pages() {
        let pdf = empty_pages_pdf(6);
        let options = LayoutOptions {
            max_pages: 5,
            ..LayoutOptions::default()
        };
        let err = interpret_pdf_bytes(&pdf, &options).expect_err("6 pages > max_pages 5");
        assert!(err.to_string().contains("max_pages"), "{err}");

        let within = LayoutOptions {
            max_pages: 6,
            ..LayoutOptions::default()
        };
        let doc = interpret_pdf_bytes(&pdf, &within).expect("6 pages <= max_pages 6");
        assert_eq!(doc.pages.len(), 6);
    }

    #[test]
    fn test_interpret_corrupted_pdf_returns_err() {
        let corrupted = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 999 0 R >>\ntrailer\n<< /Root 1 0 R >>\n%%EOF";
        let res = interpret_pdf_bytes(corrupted, &LayoutOptions::default());
        assert!(res.is_err());
    }
}
