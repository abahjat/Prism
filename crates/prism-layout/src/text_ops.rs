// SPDX-License-Identifier: AGPL-3.0-only
//! Reading-order text extraction, character bounding boxes, and search over [`LayoutDocument`].

use crate::model::{
    Affine, FillPaint, GlyphRun, Item, LayoutDocument, LayoutPage, PathCommand, Point, Rect, Rgba,
    SourceRef,
};
use prism_core::review::{Finding, FindingKind, Location, ReviewRect, Severity};
use serde::{Deserialize, Serialize};

/// A positioned character on a [`LayoutPage`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharBox {
    /// The Unicode scalar value.
    pub ch: char,
    /// UTF-8 byte offset of `ch` within the string returned by [`LayoutPage::text`].
    pub byte_offset: usize,
    /// Bounding rectangle in page coordinates (points, top-left origin).
    pub rect: Rect,
    /// Originating UDM source reference.
    pub source: SourceRef,
    /// Font size in points.
    pub font_size: f32,
}

/// Options for [`LayoutDocument::find`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOptions {
    /// Match character case exactly when `true` (default `false`).
    pub case_sensitive: bool,
    /// Ignore diacritical marks / accents when `true` (e.g. `"cafe"` matches `"Café"`).
    pub ignore_diacritics: bool,
    /// Require word boundaries before and after each match when `true`.
    pub whole_word: bool,
}

/// A search match produced by [`LayoutDocument::find`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Zero-based page index where the match occurred.
    pub page: u32,
    /// Matched substring from [`LayoutPage::text`].
    pub text: String,
    /// Highlight rectangles in page coordinates (one rectangle per line spanned by the match).
    pub rects: Vec<Rect>,
}

const MAX_GROUP_TRAVERSAL_DEPTH: u32 = 64;

impl LayoutPage {
    /// Extract the page's plain text in reading order.
    ///
    /// Text is reconstructed exclusively from [`Item::Glyphs`] runs and their clusters.
    #[must_use]
    pub fn text(&self) -> String {
        self.char_boxes().into_iter().map(|cb| cb.ch).collect()
    }

    /// Return positioned bounding boxes for every character in [`LayoutPage::text`].
    #[must_use]
    pub fn char_boxes(&self) -> Vec<CharBox> {
        let mut flat_runs = Vec::new();
        collect_glyph_runs(&self.items, Affine::IDENTITY, 0, &mut flat_runs);

        let mut out: Vec<CharBox> = Vec::new();
        let mut byte_offset = 0usize;
        let mut prev_meta: Option<(f32, f32, f32, SourceRef, char)> = None;

        for (world_transform, run) in flat_runs {
            if run.text.is_empty() {
                continue;
            }
            let run_chars = build_run_char_boxes(run, world_transform);
            if run_chars.is_empty() {
                continue;
            }

            let first_ch = run_chars[0].0;
            let first_rect = run_chars[0].1;
            let baseline_pt = world_transform.transform_point(Point::ZERO);
            let run_size = run.size.max(1.0);

            if let Some((prev_end_x, prev_base_y, prev_size, prev_source, prev_last_ch)) = prev_meta
                && !prev_last_ch.is_whitespace()
                && !first_ch.is_whitespace()
            {
                let dy = (baseline_pt.y - prev_base_y).abs();
                let line_threshold = prev_size.max(run_size) * 0.45;
                if dy > line_threshold || run.source.block != prev_source.block {
                    let nl_rect =
                        Rect::new(prev_end_x, prev_base_y - prev_size * 0.8, 0.0, prev_size);
                    out.push(CharBox {
                        ch: '\n',
                        byte_offset,
                        rect: nl_rect,
                        source: prev_source,
                        font_size: prev_size,
                    });
                    byte_offset += 1;
                } else {
                    let dx = first_rect.x - prev_end_x;
                    let space_threshold = prev_size.min(run_size) * 0.15;
                    if dx > space_threshold {
                        let sp_rect = Rect::new(prev_end_x, first_rect.y, dx, first_rect.height);
                        out.push(CharBox {
                            ch: ' ',
                            byte_offset,
                            rect: sp_rect,
                            source: prev_source,
                            font_size: prev_size,
                        });
                        byte_offset += 1;
                    }
                }
            }

            let mut last_rect = first_rect;
            let mut last_ch = first_ch;
            for (ch, rect) in run_chars {
                last_rect = rect;
                last_ch = ch;
                out.push(CharBox {
                    ch,
                    byte_offset,
                    rect,
                    source: run.source,
                    font_size: run_size,
                });
                byte_offset += ch.len_utf8();
            }

            prev_meta = Some((
                last_rect.right(),
                baseline_pt.y,
                run_size,
                run.source,
                last_ch,
            ));
        }

        out
    }
}

fn collect_glyph_runs<'a>(
    items: &'a [Item],
    parent_transform: Affine,
    depth: u32,
    out: &mut Vec<(Affine, &'a GlyphRun)>,
) {
    if depth > MAX_GROUP_TRAVERSAL_DEPTH {
        return;
    }
    for item in items {
        match item {
            Item::Glyphs(run) => {
                out.push((parent_transform.then(run.transform), run));
            }
            Item::Group(group) => {
                collect_glyph_runs(
                    &group.children,
                    parent_transform.then(group.transform),
                    depth + 1,
                    out,
                );
            }
            Item::Path(_) | Item::Image(_) | Item::Link(_) | Item::Anchor(_) => {}
        }
    }
}

fn build_run_char_boxes(run: &GlyphRun, world_transform: Affine) -> Vec<(char, Rect)> {
    let font_size = run.size.max(1.0);
    let ascent = font_size * 0.8;
    let height = font_size;
    let mut result = Vec::new();

    if !run.clusters.is_empty() {
        let mut sorted_clusters: Vec<_> = run.clusters.iter().collect();
        sorted_clusters.sort_by_key(|c| c.text_range.start);

        for cluster in sorted_clusters {
            let start = cluster.text_range.start as usize;
            let end = (cluster.text_range.end as usize).min(run.text.len());
            if start >= end || !run.text.is_char_boundary(start) || !run.text.is_char_boundary(end)
            {
                continue;
            }
            let slice = &run.text[start..end];
            let chars: Vec<char> = slice.chars().collect();
            if chars.is_empty() {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let count_f = chars.len() as f32;
            let char_adv = cluster.advance / count_f;
            for (idx, ch) in chars.into_iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let local_x = cluster.x + (idx as f32) * char_adv;
                let local_rect = Rect::new(local_x, -ascent, char_adv.max(0.0), height);
                result.push((ch, world_transform.transform_rect_bbox(local_rect)));
            }
        }
        if !result.is_empty() {
            return result;
        }
    }

    // Fallback when `clusters` is empty: distribute total glyph advance across `run.text` chars
    let chars: Vec<char> = run.text.chars().collect();
    if chars.is_empty() {
        return result;
    }
    let total_advance: f32 = if run.glyphs.is_empty() {
        #[allow(clippy::cast_precision_loss)]
        {
            font_size * 0.5 * (chars.len() as f32)
        }
    } else {
        run.glyphs
            .iter()
            .fold(0.0_f32, |acc, g| acc.max(g.x + g.advance))
    };
    #[allow(clippy::cast_precision_loss)]
    let char_adv = total_advance / (chars.len() as f32);
    for (idx, ch) in chars.into_iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let local_x = (idx as f32) * char_adv;
        let local_rect = Rect::new(local_x, -ascent, char_adv.max(0.0), height);
        result.push((ch, world_transform.transform_rect_bbox(local_rect)));
    }
    result
}

impl LayoutDocument {
    /// Search all pages in the document for `query`, returning highlighted [`SearchHit`]s.
    #[must_use]
    pub fn find(&self, query: &str, options: SearchOptions) -> Vec<SearchHit> {
        if query.is_empty() {
            return Vec::new();
        }
        let norm_query = normalize_search_text(query, options);
        if norm_query.is_empty() {
            return Vec::new();
        }
        let query_chars: Vec<char> = norm_query.chars().collect();
        let mut hits = Vec::new();

        for page in &self.pages {
            let char_boxes = page.char_boxes();
            if char_boxes.is_empty() {
                continue;
            }

            // Build normalized character stream with mapping back to `char_boxes` index
            let mut norm_chars: Vec<char> = Vec::with_capacity(char_boxes.len());
            let mut norm_to_box_idx: Vec<usize> = Vec::with_capacity(char_boxes.len());

            for (box_idx, cb) in char_boxes.iter().enumerate() {
                for nc in normalize_char(cb.ch, options) {
                    norm_chars.push(nc);
                    norm_to_box_idx.push(box_idx);
                }
            }

            if norm_chars.len() < query_chars.len() {
                continue;
            }

            let mut i = 0usize;
            while i + query_chars.len() <= norm_chars.len() {
                let window = &norm_chars[i..i + query_chars.len()];
                let matched = window
                    .iter()
                    .zip(&query_chars)
                    .all(|(&a, &b)| chars_match(a, b));

                if matched {
                    let word_ok = if options.whole_word {
                        let before_ok = i == 0 || !is_word_char(norm_chars[i - 1]);
                        let after_idx = i + query_chars.len();
                        let after_ok =
                            after_idx >= norm_chars.len() || !is_word_char(norm_chars[after_idx]);
                        before_ok && after_ok
                    } else {
                        true
                    };

                    if word_ok {
                        let first_box = norm_to_box_idx[i];
                        let last_box = norm_to_box_idx[i + query_chars.len() - 1];
                        let matched_boxes = &char_boxes[first_box..=last_box];
                        let matched_text: String = matched_boxes.iter().map(|b| b.ch).collect();
                        let rects = coalesce_line_rects(matched_boxes);
                        hits.push(SearchHit {
                            page: page.index,
                            text: matched_text,
                            rects,
                        });
                        i += query_chars.len();
                        continue;
                    }
                }
                i += 1;
            }
        }

        hits
    }

    /// Return the 0-based page index and bounding rectangles for all glyph runs matching `source`.
    #[must_use]
    pub fn rects_for_source(&self, source: SourceRef) -> Option<(u32, Vec<Rect>)> {
        let page = self.pages.iter().find(|p| p.index == source.page)?;
        let mut flat_runs = Vec::new();
        collect_glyph_runs(&page.items, Affine::IDENTITY, 0, &mut flat_runs);
        let mut rects = Vec::new();
        for (world_transform, run) in flat_runs {
            if run.source == source
                && let Some(bbox) = run_bounding_box(run, world_transform)
            {
                rects.push(bbox);
            }
        }
        (!rects.is_empty()).then_some((page.index, rects))
    }

    /// Inspect all pages in this [`LayoutDocument`] for visually concealed text:
    /// - [`FindingKind::WhiteOrLowContrastText`] (white or background-colored text)
    /// - [`FindingKind::TinyText`] (`< 2 pt` effective font size)
    /// - [`FindingKind::OffPageOrOffSlideText`] (text outside page/slide bounds)
    /// - [`FindingKind::CoveredText`] (text occluded by a subsequent opaque shape or image)
    /// - [`FindingKind::ZeroWidthCharacters`] (`U+200B`, `U+200C`, `U+200D`, `U+FEFF`, `U+2060`)
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn inspect_concealed_text(&self, is_slide: bool) -> Vec<Finding> {
        let mut findings = Vec::new();

        for page in &self.pages {
            let mut flat_items = Vec::new();
            collect_flat_z_items(&page.items, Affine::IDENTITY, 1.0, 0, &mut flat_items);

            for (z_idx, entry) in flat_items.iter().enumerate() {
                let FlatZItem::Glyphs {
                    transform: world_transform,
                    opacity,
                    run,
                } = entry
                else {
                    continue;
                };

                if run.text.is_empty() {
                    continue;
                }

                let Some(bbox) = run_bounding_box(run, *world_transform) else {
                    continue;
                };
                let review_rect = ReviewRect::new(bbox.x, bbox.y, bbox.width, bbox.height);
                let make_loc = || {
                    if is_slide {
                        Location::Slide {
                            slide_index: page.index,
                            rects: vec![review_rect],
                        }
                    } else {
                        Location::PageRects {
                            page_index: page.index,
                            rects: vec![review_rect],
                        }
                    }
                };

                // 1. Zero-width characters
                if run.text.chars().any(|c| {
                    matches!(
                        c,
                        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}'
                    )
                }) {
                    findings.push(Finding::new(
                        FindingKind::ZeroWidthCharacters,
                        Severity::Notable,
                        make_loc(),
                        Some(run.source),
                        Some(run.text.clone()),
                        serde_json::json!({
                            "page": page.index + 1,
                        }),
                    ));
                }

                if run.text.trim().is_empty() {
                    continue;
                }

                // 2. Tiny text (< 2 pt)
                let [a, b, c, d, _, _] = world_transform.coeffs;
                let scale = a.hypot(b).max(c.hypot(d)).max(1e-4);
                let effective_size_pt = run.size * scale;
                if effective_size_pt < 2.0 {
                    findings.push(Finding::new(
                        FindingKind::TinyText,
                        Severity::High,
                        make_loc(),
                        Some(run.source),
                        Some(run.text.clone()),
                        serde_json::json!({
                            "font_size_pt": effective_size_pt,
                            "page": page.index + 1,
                        }),
                    ));
                }

                // 3. Off-page / off-slide text
                if bbox.right() < -1.0
                    || bbox.bottom() < -1.0
                    || bbox.x > page.size.width + 1.0
                    || bbox.y > page.size.height + 1.0
                {
                    findings.push(Finding::new(
                        FindingKind::OffPageOrOffSlideText,
                        Severity::High,
                        make_loc(),
                        Some(run.source),
                        Some(run.text.clone()),
                        serde_json::json!({
                            "x": bbox.x,
                            "y": bbox.y,
                            "page_width": page.size.width,
                            "page_height": page.size.height,
                            "page": page.index + 1,
                        }),
                    ));
                }

                // 4. White or low-contrast text (skip intentionally invisible Type3 search overlay runs with a == 0)
                if run.color.a > 0 && *opacity > 0.05 {
                    let center = Point::new(bbox.x + bbox.width * 0.5, bbox.y + bbox.height * 0.5);
                    let mut bg_color = run.decorations.highlight.unwrap_or(Rgba::WHITE);
                    if run.decorations.highlight.is_none() {
                        for earlier in &flat_items[..z_idx] {
                            if let FlatZItem::SolidRect { rect, color } = earlier
                                && color.a >= 200
                                && rect.width >= 4.0
                                && rect.height >= 4.0
                                && rect.contains(center)
                            {
                                bg_color = *color;
                            }
                        }
                    }
                    let dist = (i32::from(run.color.r) - i32::from(bg_color.r)).abs()
                        + (i32::from(run.color.g) - i32::from(bg_color.g)).abs()
                        + (i32::from(run.color.b) - i32::from(bg_color.b)).abs();
                    if dist <= 24 {
                        findings.push(Finding::new(
                            FindingKind::WhiteOrLowContrastText,
                            Severity::High,
                            make_loc(),
                            Some(run.source),
                            Some(run.text.clone()),
                            serde_json::json!({
                                "text_color": run.color.to_hex_rgb(),
                                "background_color": bg_color.to_hex_rgb(),
                                "page": page.index + 1,
                            }),
                        ));
                    }
                }

                // 5. Covered / occluded text by a subsequent opaque shape or image in Z-order
                if bbox.width > 1.0 && bbox.height > 1.0 {
                    let run_area = bbox.width * bbox.height;
                    for later in &flat_items[z_idx + 1..] {
                        match later {
                            FlatZItem::SolidRect { rect, color }
                                if color.a >= 240 && rect.width >= 4.0 && rect.height >= 4.0 =>
                            {
                                if rect_intersection_area(bbox, *rect) >= run_area * 0.70 {
                                    findings.push(Finding::new(
                                        FindingKind::CoveredText,
                                        Severity::High,
                                        make_loc(),
                                        Some(run.source),
                                        Some(run.text.clone()),
                                        serde_json::json!({
                                            "covered_by": "shape",
                                            "fill_color": color.to_hex_rgb(),
                                            "page": page.index + 1,
                                        }),
                                    ));
                                    break;
                                }
                            }
                            FlatZItem::ImageRect {
                                rect,
                                opacity: img_op,
                            } if *img_op >= 0.90
                                && rect.width >= 4.0
                                && rect.height >= 4.0
                                && rect_intersection_area(bbox, *rect) >= run_area * 0.70 =>
                            {
                                findings.push(Finding::new(
                                    FindingKind::CoveredText,
                                    Severity::High,
                                    make_loc(),
                                    Some(run.source),
                                    Some(run.text.clone()),
                                    serde_json::json!({
                                        "covered_by": "image",
                                        "page": page.index + 1,
                                    }),
                                ));
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        findings
    }
}

enum FlatZItem<'a> {
    Glyphs {
        transform: Affine,
        opacity: f32,
        run: &'a GlyphRun,
    },
    SolidRect {
        rect: Rect,
        color: Rgba,
    },
    ImageRect {
        rect: Rect,
        opacity: f32,
    },
}

fn collect_flat_z_items<'a>(
    items: &'a [Item],
    parent_transform: Affine,
    parent_opacity: f32,
    depth: u32,
    out: &mut Vec<FlatZItem<'a>>,
) {
    if depth > MAX_GROUP_TRAVERSAL_DEPTH {
        return;
    }
    for item in items {
        match item {
            Item::Glyphs(run) => {
                out.push(FlatZItem::Glyphs {
                    transform: parent_transform.then(run.transform),
                    opacity: parent_opacity,
                    run,
                });
            }
            Item::Path(path) => {
                if let Some(FillPaint::Solid(mut color)) = path.fill
                    && let Some(local_bbox) = path_commands_bbox(&path.commands)
                {
                    let world_m = parent_transform.then(path.transform);
                    let rect = world_m.transform_rect_bbox(local_bbox);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        color.a = (f32::from(color.a) * parent_opacity).round() as u8;
                    }
                    out.push(FlatZItem::SolidRect { rect, color });
                }
            }
            Item::Image(img) => {
                let world_m = parent_transform.then(img.transform);
                let rect = world_m.transform_rect_bbox(img.rect);
                out.push(FlatZItem::ImageRect {
                    rect,
                    opacity: parent_opacity * img.opacity,
                });
            }
            Item::Group(group) => {
                collect_flat_z_items(
                    &group.children,
                    parent_transform.then(group.transform),
                    parent_opacity * group.opacity,
                    depth + 1,
                    out,
                );
            }
            Item::Link(_) | Item::Anchor(_) => {}
        }
    }
}

fn path_commands_bbox(commands: &[PathCommand]) -> Option<Rect> {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut any = false;
    for cmd in commands {
        let pt = match cmd {
            PathCommand::MoveTo(p)
            | PathCommand::LineTo(p)
            | PathCommand::QuadTo { to: p, .. }
            | PathCommand::CurveTo { to: p, .. } => Some(*p),
            PathCommand::Close => None,
        };
        if let Some(p) = pt {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
            any = true;
        }
    }
    any.then(|| {
        Rect::new(
            min_x,
            min_y,
            (max_x - min_x).max(0.0),
            (max_y - min_y).max(0.0),
        )
    })
}

fn run_bounding_box(run: &GlyphRun, world_transform: Affine) -> Option<Rect> {
    let char_boxes = build_run_char_boxes(run, world_transform);
    let mut iter = char_boxes.into_iter().map(|(_, r)| r);
    let first = iter.next()?;
    Some(iter.fold(first, |acc, r| acc.union(r)))
}

fn rect_intersection_area(a: Rect, b: Rect) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = a.right().min(b.right());
    let y2 = a.bottom().min(b.bottom());
    if x2 > x1 && y2 > y1 {
        (x2 - x1) * (y2 - y1)
    } else {
        0.0
    }
}

fn coalesce_line_rects(boxes: &[CharBox]) -> Vec<Rect> {
    let mut rects: Vec<Rect> = Vec::new();
    for cb in boxes {
        if cb.ch == '\n' || cb.rect.width <= 0.0 {
            continue;
        }
        if let Some(last) = rects.last_mut() {
            let same_line = (last.y - cb.rect.y).abs() <= cb.font_size.max(1.0) * 0.45;
            if same_line {
                *last = last.union(cb.rect);
                continue;
            }
        }
        rects.push(cb.rect);
    }
    rects
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn chars_match(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    a.is_whitespace() && b.is_whitespace()
}

fn normalize_search_text(input: &str, options: SearchOptions) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        for nc in normalize_char(c, options) {
            out.push(nc);
        }
    }
    out
}

fn normalize_char(c: char, options: SearchOptions) -> Vec<char> {
    if is_combining_mark(c) && options.ignore_diacritics {
        return Vec::new();
    }
    let base = if options.ignore_diacritics {
        strip_diacritic(c)
    } else {
        c
    };
    if options.case_sensitive {
        vec![base]
    } else {
        base.to_lowercase().collect()
    }
}

fn is_combining_mark(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x064B..=0x065F
            | 0x0670
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
    )
}

#[allow(clippy::too_many_lines)]
fn strip_diacritic(c: char) -> char {
    match c {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' | 'Ǎ' => 'A',
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' => 'a',
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => 'C',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'Ď' | 'Đ' => 'D',
        'ď' | 'đ' => 'd',
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => 'E',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => 'G',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'Ĥ' | 'Ħ' => 'H',
        'ĥ' | 'ħ' => 'h',
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' | 'Ǐ' => 'I',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' => 'i',
        'Ĵ' => 'J',
        'ĵ' => 'j',
        'Ķ' => 'K',
        'ķ' => 'k',
        'Ĺ' | 'Ļ' | 'Ľ' | 'Ŀ' | 'Ł' => 'L',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' => 'N',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' | 'Ǒ' => 'O',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ǒ' => 'o',
        'Ŕ' | 'Ŗ' | 'Ř' => 'R',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'Ś' | 'Ŝ' | 'Ş' | 'Š' | 'Ș' => 'S',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => 's',
        'Ţ' | 'Ť' | 'Ŧ' | 'Ț' => 'T',
        'ţ' | 'ť' | 'ŧ' | 'ț' => 't',
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' | 'Ǔ' => 'U',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ǔ' => 'u',
        'Ŵ' => 'W',
        'ŵ' => 'w',
        'Ý' | 'Ÿ' | 'Ŷ' => 'Y',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'Ź' | 'Ż' | 'Ž' => 'Z',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cluster, Decorations, FontId, PositionedGlyph, Rgba, Size};

    fn sample_doc() -> LayoutDocument {
        let mut doc = LayoutDocument::new();
        let mut page = LayoutPage::new(0, Size::LETTER);

        let run1 = GlyphRun {
            font: FontId(0),
            size: 12.0,
            color: Rgba::BLACK,
            transform: Affine::translate(72.0, 100.0),
            glyphs: vec![PositionedGlyph {
                glyph_id: 1,
                x: 0.0,
                y: 0.0,
                advance: 30.0,
            }],
            text: "Café résumé".to_string(),
            clusters: vec![
                Cluster {
                    text_range: 0..5, // "Café" (5 UTF-8 bytes)
                    glyph_range: 0..1,
                    x: 0.0,
                    advance: 24.0,
                },
                Cluster {
                    text_range: 5..6, // " "
                    glyph_range: 0..1,
                    x: 24.0,
                    advance: 6.0,
                },
                Cluster {
                    text_range: 6..14, // "résumé" (8 UTF-8 bytes)
                    glyph_range: 0..1,
                    x: 30.0,
                    advance: 36.0,
                },
            ],
            decorations: Decorations::default(),
            source: SourceRef::new(0, 0, 0),
        };

        let run2 = GlyphRun {
            font: FontId(0),
            size: 12.0,
            color: Rgba::BLACK,
            transform: Affine::translate(72.0, 120.0),
            glyphs: vec![],
            text: "Second line of text".to_string(),
            clusters: vec![],
            decorations: Decorations::default(),
            source: SourceRef::new(0, 1, 0),
        };

        page.items.push(Item::Glyphs(run1));
        page.items.push(Item::Glyphs(run2));
        doc.pages.push(page);
        doc
    }

    #[test]
    fn test_page_text_and_char_boxes_stay_in_sync() {
        let doc = sample_doc();
        let page = &doc.pages[0];
        let text = page.text();
        let boxes = page.char_boxes();

        assert_eq!(text, "Café résumé\nSecond line of text");
        let reconstructed: String = boxes.iter().map(|b| b.ch).collect();
        assert_eq!(reconstructed, text);

        for cb in &boxes {
            assert_eq!(text[cb.byte_offset..].chars().next(), Some(cb.ch));
        }
    }

    #[test]
    fn test_document_find_case_diacritics_and_whole_word() {
        let doc = sample_doc();

        // Case-insensitive + diacritic-insensitive search
        let hits = doc.find(
            "cafe",
            SearchOptions {
                case_sensitive: false,
                ignore_diacritics: true,
                whole_word: true,
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "Café");
        assert_eq!(hits[0].rects.len(), 1);
        assert!((hits[0].rects[0].x - 72.0).abs() < 1e-3);
        assert!((hits[0].rects[0].width - 24.0).abs() < 1e-3);

        // Case-sensitive search for "cafe" should not match "Café"
        let hits_cs = doc.find(
            "cafe",
            SearchOptions {
                case_sensitive: true,
                ignore_diacritics: true,
                whole_word: false,
            },
        );
        assert_eq!(hits_cs.len(), 0);

        // Whole-word search for "Caf" should not match "Café"
        let hits_ww = doc.find(
            "Caf",
            SearchOptions {
                case_sensitive: false,
                ignore_diacritics: true,
                whole_word: true,
            },
        );
        assert_eq!(hits_ww.len(), 0);
    }
}
