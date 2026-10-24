// SPDX-License-Identifier: AGPL-3.0-only
//! Native Word print-layout engine (`WordDocument` → `LayoutDocument`).
//!
//! Implements Milestones M1–M6 of Phase 2:
//! - **M1**: Styles, paragraphs, tabs & tab leaders, multi-level numbering, paragraph borders/shading,
//!   and character transforms (`caps`, `smallCaps`, `dstrike`, symbols).
//! - **M2**: Tables (`tblGrid`, `Fixed` vs `AutoFit` column sizing, `gridSpan`, `vMerge`, table style
//!   conditional formatting, ECMA-376 §17.4.26 border conflict resolution, row splitting, repeating headers).
//! - **M3**: Multi-section documents (`NextPage`, `Continuous`, `EvenPage`, `OddPage`), multi-column flow,
//!   mirror margins + gutter, headers/footers (`First`, `Even`, `Default` with inheritance), keep/widow rules,
//!   and two-pass `PAGE` / `NUMPAGES` / `SECTIONPAGES` field resolution.
//! - **M4**: Same-page footnotes with separator rule, end-of-document endnotes, stored field results,
//!   hyperlinks (`LinkItem`), and bookmarks (`AnchorItem`).
//! - **M5**: Inline and floating `DrawingML` / VML objects (`Square`, `Tight`, `TopAndBottom`, `None`
//!   behind/in-front), text boxes (`w:txbxContent`), image cropping, and native WMF/EMF rendering.
//! - **M6**: Tracked changes (`AllMarkup`, `SimpleMarkup`, `NoMarkup`, `Original`), margin change bars,
//!   right-margin comment balloons, and dotted-underline hidden text (`w:vanish`).

use crate::WordDocument;
use crate::emf::{is_metafile, render_metafile};
use crate::model::{
    DrawingPayload, FieldKind, HeaderFooterKind, NumberFormat, PageVerticalAlign, ParagraphContent,
    ParagraphProperties, ResolvedParagraphStyle, ResolvedRunStyle, RevisionKind, RunItem,
    RunProperties, SectionBreakKind, SectionProperties, TabLeader, TabStop, TabStopKind,
    TableLayoutMode, TableStyleRegion, TableWidth, UnderlineKind, VerticalMerge, VerticalRunAlign,
    WordAlignment, WordBlock, WordBorder, WordBorderStyle, WordBorders, WordDrawing, WordField,
    WordLineSpacing, WordParagraph, WordRun, WordSection, WordTable, WordTableCell, WordTableRow,
    WrapMode,
};
use crate::numbering::{FormattedListMarker, NumberingTracker, format_number, map_symbol_char};
use prism_core::error::{Error, Result};
use prism_layout::{
    Affine, AnchorItem, FillPaint, FillRule, FontSystem, GlyphRun, ImageId, ImageItem, ImageStore,
    Item, LayoutDocument, LayoutOptions, LayoutPage, LineCap, LineJoin, LineSpacing, LinkItem,
    MarkupMode, ParagraphSpec, PathCommand, PathItem, Point, Rect, Rgba, ScriptPosition,
    ShapedParagraph, Size, SpanSpec, StrokeStyle, TextAlign, TextLayoutEngine, normalize_zero,
};
use std::collections::{BTreeMap, BTreeSet};

/// Compute the horizontal `[min_x, max_x]` exclusion interval carved by an ECMA-376
/// `wp:wrapPolygon` (normalized `0..=21600` coordinates inside `rect`) across the vertical
/// scanband `[y_top, y_bottom]`.
#[must_use]
pub fn carve_wrap_polygon_interval(
    rect: Rect,
    polygon: &[(f32, f32)],
    y_top: f32,
    y_bottom: f32,
) -> Option<(f32, f32)> {
    if y_bottom <= rect.y || y_top >= rect.bottom() {
        return None;
    }
    if polygon.len() < 3 || rect.width <= 0.0 || rect.height <= 0.0 {
        return Some((rect.x, rect.right()));
    }

    let pts: Vec<(f32, f32)> = polygon
        .iter()
        .map(|&(px, py)| {
            (
                rect.x + (px / 21_600.0).clamp(0.0, 1.0) * rect.width,
                rect.y + (py / 21_600.0).clamp(0.0, 1.0) * rect.height,
            )
        })
        .collect();

    let band_top = y_top.max(rect.y);
    let band_bot = y_bottom.min(rect.bottom());
    let sample_ys = [band_top, f32::midpoint(band_top, band_bot), band_bot];

    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;

    for &(vx, vy) in &pts {
        if vy >= band_top && vy <= band_bot {
            min_x = min_x.min(vx);
            max_x = max_x.max(vx);
        }
    }

    for &sy in &sample_ys {
        for i in 0..pts.len() {
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[(i + 1) % pts.len()];
            if (y0 <= sy && y1 >= sy) || (y1 <= sy && y0 >= sy) {
                let dy = y1 - y0;
                let ix = if dy.abs() < 1e-5 {
                    x0.min(x1)
                } else {
                    x0 + ((sy - y0) / dy) * (x1 - x0)
                };
                min_x = min_x.min(ix);
                max_x = max_x.max(ix);
            }
        }
    }

    if min_x.is_finite() && max_x.is_finite() && max_x >= min_x {
        Some((
            min_x.clamp(rect.x, rect.right()),
            max_x.clamp(rect.x, rect.right()),
        ))
    } else {
        Some((rect.x, rect.right()))
    }
}

fn adjust_band_for_exclusions(
    exclusions: &[ExclusionZone],
    mut x: f32,
    mut w: f32,
    y: f32,
    line_h: f32,
) -> (f32, f32) {
    for zone in exclusions {
        if y + line_h <= zone.rect.y || y >= zone.rect.bottom() {
            continue;
        }
        let interval = match zone.wrap_mode {
            WrapMode::Square => Some((zone.rect.x, zone.rect.right())),
            WrapMode::Tight { ref polygon } | WrapMode::Through { ref polygon } => {
                carve_wrap_polygon_interval(zone.rect, polygon, y, y + line_h)
            }
            WrapMode::Inline | WrapMode::None | WrapMode::TopAndBottom => None,
        };
        if let Some((ex_left, ex_right)) = interval {
            let right = x + w;
            if ex_right > x && ex_left < right {
                let left_space = (ex_left - x).max(0.0);
                let right_space = (right - ex_right).max(0.0);
                if right_space >= left_space && right_space > 36.0 {
                    x = ex_right + 6.0;
                    w = right_space - 6.0;
                } else if left_space > 36.0 {
                    w = left_space - 6.0;
                }
            }
        }
    }
    (x, w.max(18.0))
}

///Default footnote area separator spacing in points.
const FOOTNOTE_SEP_HEIGHT_PT: f32 = 10.0;

/// Author palette for tracked-change revisions (deterministic by author name).
const AUTHOR_PALETTE: [Rgba; 6] = [
    Rgba::rgb(0xB5, 0x08, 0x2E), // Crimson
    Rgba::rgb(0x00, 0x5A, 0x9E), // Royal Blue
    Rgba::rgb(0x10, 0x7C, 0x41), // Forest Green
    Rgba::rgb(0x88, 0x17, 0x98), // Plum
    Rgba::rgb(0xC5, 0x5A, 0x11), // Rust Orange
    Rgba::rgb(0x00, 0x82, 0x72), // Teal
];

/// Lay out a [`WordDocument`] into a positioned [`LayoutDocument`].
///
/// Performs a two-pass layout when dynamic pagination fields (`NUMPAGES`, `SECTIONPAGES`)
/// are present so page totals are exact.
///
/// # Errors
///
/// Returns an [`Error`] if resource limits (`max_pages`, `max_glyphs_per_page`,
/// `max_image_pixels`, `max_recursion_depth`) are exceeded.
pub fn layout_word_document(doc: &WordDocument, options: &LayoutOptions) -> Result<LayoutDocument> {
    let needs_second_pass = document_has_page_count_fields(doc);

    let first_pass = run_layout_pass(doc, options, 1, &[], None)?;
    if !needs_second_pass {
        return Ok(first_pass.document);
    }

    let total_pages = first_pass.document.pages.len().max(1);
    let second_pass = run_layout_pass(
        doc,
        options,
        total_pages,
        &first_pass.section_page_counts,
        None,
    )?;
    Ok(second_pass.document)
}

/// Lay out only the requested 0-based `page_range` (`start_page..end_page`) of a [`WordDocument`].
///
/// Stops flow pagination as soon as `page_range.end` pages have been produced and discards
/// item buffers for pages preceding `page_range.start`, reducing time-to-first-page (`0..1`)
/// on large multi-hundred-page documents to a fraction of full-document layout time.
/// Documents with `NUMPAGES` / `SECTIONPAGES` fields are laid out in full once first, so those
/// fields show the real totals.
///
/// # Errors
///
/// Returns an [`Error`] if resource limits in `options` are exceeded.
pub fn layout_word_document_range(
    doc: &WordDocument,
    options: &LayoutOptions,
    page_range: std::ops::Range<usize>,
) -> Result<LayoutDocument> {
    if page_range.start >= page_range.end {
        return Ok(LayoutDocument::new());
    }
    // `NUMPAGES` / `SECTIONPAGES` fields need the real totals, so documents that have them
    // take a full first pass; the second pass then keeps only the requested pages.
    if document_has_page_count_fields(doc) {
        let first_pass = run_layout_pass(doc, options, 1, &[], None)?;
        let total_pages = first_pass.document.pages.len().max(1);
        let pass = run_layout_pass(
            doc,
            options,
            total_pages,
            &first_pass.section_page_counts,
            Some(page_range),
        )?;
        return Ok(pass.document);
    }
    let pass = run_layout_pass(doc, options, 1, &[], Some(page_range))?;
    Ok(pass.document)
}

struct PassOutput {
    document: LayoutDocument,
    section_page_counts: Vec<usize>,
}

fn document_has_page_count_fields(doc: &WordDocument) -> bool {
    let check_blocks =
        |blocks: &[WordBlock]| -> bool { blocks.iter().any(block_has_page_count_field) };
    if doc.sections.iter().any(|s| check_blocks(&s.blocks)) {
        return true;
    }
    doc.headers
        .values()
        .any(|story| check_blocks(&story.blocks))
        || doc
            .footers
            .values()
            .any(|story| check_blocks(&story.blocks))
}

fn block_has_page_count_field(block: &WordBlock) -> bool {
    match block {
        WordBlock::Paragraph(p) => p.content.iter().any(content_has_page_count_field),
        WordBlock::Table(t) => t.rows.iter().any(|r| {
            r.cells
                .iter()
                .any(|c| c.blocks.iter().any(block_has_page_count_field))
        }),
    }
}

fn content_has_page_count_field(content: &ParagraphContent) -> bool {
    match content {
        ParagraphContent::Field(f) => {
            matches!(f.kind, FieldKind::NumPages | FieldKind::SectionPages)
                || f.cached_result.iter().any(content_has_page_count_field)
        }
        ParagraphContent::Hyperlink(h) => h.content.iter().any(content_has_page_count_field),
        _ => false,
    }
}

/// Floating exclusion zone on the active page.
#[derive(Debug, Clone)]
struct ExclusionZone {
    rect: Rect,
    wrap_mode: WrapMode,
}

/// Active page state during flow pagination.
struct ActivePage {
    page: LayoutPage,
    background_items: Vec<Item>,
    foreground_items: Vec<Item>,
    footnote_ids: Vec<i32>,
    footnote_height_pt: f32,
    comment_ids: Vec<String>,
    exclusions: Vec<ExclusionZone>,
    section_index: usize,
    section_page_offset: usize,
    body_top: f32,
    body_bottom: f32,
    body_left: f32,
    body_right: f32,
}

struct WordLayoutEngine<'a> {
    doc: &'a WordDocument,
    options: &'a LayoutOptions,
    font_sys: FontSystem,
    text_engine: TextLayoutEngine,
    images: ImageStore,
    image_map: BTreeMap<String, (ImageId, u32, u32)>,
    raw_media: BTreeMap<String, Vec<u8>>,
    numbering: NumberingTracker,
    known_total_pages: usize,
    known_section_pages: &'a [usize],
    finished_pages: Vec<ActivePage>,
    current_page: Option<ActivePage>,
    current_col: usize,
    cursor_y: f32,
    at_column_top: bool,
    prev_space_after: f32,
    prev_style_id: Option<String>,
    prev_borders: Option<WordBorders>,
    referenced_endnotes: Vec<i32>,
    seen_endnotes: BTreeSet<i32>,
    footnote_display_numbers: BTreeMap<i32, usize>,
    endnote_display_numbers: BTreeMap<i32, usize>,
    block_counter: u32,
    page_range: Option<std::ops::Range<usize>>,
}

fn run_layout_pass(
    doc: &WordDocument,
    options: &LayoutOptions,
    known_total_pages: usize,
    known_section_pages: &[usize],
    page_range: Option<std::ops::Range<usize>>,
) -> Result<PassOutput> {
    let mut font_sys = FontSystem::new(options);
    font_sys.register_document_fonts(&doc.fonts);

    let mut images = ImageStore::new();
    let mut image_map = BTreeMap::new();
    let mut raw_media = BTreeMap::new();
    for res in &doc.images {
        if let Some(ref data) = res.data {
            raw_media.insert(res.id.clone(), data.clone());
            if is_metafile(data) {
                continue;
            }
            let (w, h) = if res.width > 0 && res.height > 0 {
                (res.width, res.height)
            } else {
                probe_image_dimensions(data).unwrap_or((100, 100))
            };
            if let Ok(img_id) = images.register(res.mime_type.clone(), w, h, data.clone(), options)
            {
                image_map.insert(res.id.clone(), (img_id, w, h));
            }
        }
    }

    let mut engine = WordLayoutEngine {
        doc,
        options,
        font_sys,
        text_engine: TextLayoutEngine::new(),
        images,
        image_map,
        raw_media,
        numbering: NumberingTracker::new(),
        known_total_pages,
        known_section_pages,
        finished_pages: Vec::new(),
        current_page: None,
        current_col: 0,
        cursor_y: 0.0,
        at_column_top: true,
        prev_space_after: 0.0,
        prev_style_id: None,
        prev_borders: None,
        referenced_endnotes: Vec::new(),
        seen_endnotes: BTreeSet::new(),
        footnote_display_numbers: BTreeMap::new(),
        endnote_display_numbers: BTreeMap::new(),
        block_counter: 0,
        page_range,
    };

    engine.layout_all_sections()?;
    engine.finish()
}

impl WordLayoutEngine<'_> {
    fn reached_page_limit(&self) -> bool {
        self.page_range
            .as_ref()
            .is_some_and(|r| self.finished_pages.len() >= r.end)
    }

    fn layout_all_sections(&mut self) -> Result<()> {
        if self.doc.sections.is_empty() {
            let default_sec = WordSection::default();
            self.start_new_page(0, &default_sec.props, 0)?;
            return Ok(());
        }

        for (sec_idx, section) in self.doc.sections.iter().enumerate() {
            if self.reached_page_limit() {
                return Ok(());
            }
            let break_kind = section.props.break_kind;
            if sec_idx == 0 {
                self.start_new_page(sec_idx, &section.props, 0)?;
            } else {
                match break_kind {
                    SectionBreakKind::Continuous => {
                        self.current_col = 0;
                        if let Some(ref mut page) = self.current_page {
                            page.section_index = sec_idx;
                        }
                    }
                    SectionBreakKind::EvenPage => {
                        let next_page_1based = self.finished_pages.len() + 2;
                        self.start_new_page(sec_idx, &section.props, 0)?;
                        if !next_page_1based.is_multiple_of(2) {
                            self.start_new_page(sec_idx, &section.props, 1)?;
                        }
                    }
                    SectionBreakKind::OddPage => {
                        let next_page_1based = self.finished_pages.len() + 2;
                        self.start_new_page(sec_idx, &section.props, 0)?;
                        if next_page_1based.is_multiple_of(2) {
                            self.start_new_page(sec_idx, &section.props, 1)?;
                        }
                    }
                    SectionBreakKind::NextPage => {
                        self.start_new_page(sec_idx, &section.props, 0)?;
                    }
                }
            }

            self.layout_block_slice(&section.blocks, sec_idx, &section.props, 0)?;
        }

        if !self.referenced_endnotes.is_empty() && !self.reached_page_limit() {
            let last_sec_idx = self.doc.sections.len().saturating_sub(1);
            let last_props = self
                .doc
                .sections
                .last()
                .map(|s| s.props.clone())
                .unwrap_or_default();
            self.layout_endnotes(last_sec_idx, &last_props)?;
        }

        Ok(())
    }

    fn start_new_page(
        &mut self,
        sec_idx: usize,
        sec_props: &SectionProperties,
        sec_page_offset: usize,
    ) -> Result<()> {
        if let Some(mut prev_page) = self.current_page.take() {
            if let Some(ref r) = self.page_range
                && (prev_page.page.index as usize) < r.start
            {
                prev_page.page.items.clear();
                prev_page.background_items.clear();
                prev_page.foreground_items.clear();
                prev_page.footnote_ids.clear();
                prev_page.comment_ids.clear();
            }
            self.finished_pages.push(prev_page);
        }
        if self.finished_pages.len() >= self.options.max_pages {
            return Err(Error::InvalidInput(format!(
                "Word document page count exceeds max_pages ({})",
                self.options.max_pages
            )));
        }

        #[allow(clippy::cast_possible_truncation)]
        let page_idx = self.finished_pages.len() as u32;
        let page_1based = self.finished_pages.len() + 1;
        let is_even = page_1based.is_multiple_of(2);

        let (mut left_m, mut right_m) = if self.doc.settings.mirror_margins && is_even {
            (sec_props.margin_right_pt, sec_props.margin_left_pt)
        } else {
            (sec_props.margin_left_pt, sec_props.margin_right_pt)
        };
        if sec_props.gutter_pt > 0.0 {
            if self.doc.settings.mirror_margins && is_even {
                right_m += sec_props.gutter_pt;
            } else {
                left_m += sec_props.gutter_pt;
            }
        }

        let width = sec_props.page_width_pt.max(72.0);
        let height = sec_props.page_height_pt.max(72.0);
        let body_left = left_m.clamp(0.0, width * 0.45);
        let body_right = (width - right_m.clamp(0.0, width * 0.45)).max(body_left + 36.0);
        let body_top = sec_props.margin_top_pt.clamp(0.0, height * 0.45);
        let body_bottom =
            (height - sec_props.margin_bottom_pt.clamp(0.0, height * 0.45)).max(body_top + 36.0);

        let active = ActivePage {
            page: LayoutPage::new(page_idx, Size::new(width, height)),
            background_items: Vec::new(),
            foreground_items: Vec::new(),
            footnote_ids: Vec::new(),
            footnote_height_pt: 0.0,
            comment_ids: Vec::new(),
            exclusions: Vec::new(),
            section_index: sec_idx,
            section_page_offset: sec_page_offset,
            body_top,
            body_bottom,
            body_left,
            body_right,
        };

        self.current_page = Some(active);
        self.current_col = 0;
        self.cursor_y = body_top;
        self.at_column_top = true;
        self.prev_space_after = 0.0;
        self.prev_style_id = None;
        self.prev_borders = None;

        self.layout_page_header_and_footer(sec_idx, sec_props)?;
        if let Some(ref page) = self.current_page {
            self.cursor_y = page.body_top;
        }

        Ok(())
    }

    fn advance_column_or_page(
        &mut self,
        sec_idx: usize,
        sec_props: &SectionProperties,
    ) -> Result<()> {
        let col_count = usize::from(sec_props.column_count.max(1));
        if self.current_col + 1 < col_count {
            self.current_col += 1;
            if let Some(ref page) = self.current_page {
                self.cursor_y = page.body_top;
            }
            self.at_column_top = true;
            self.prev_space_after = 0.0;
            self.prev_borders = None;
            Ok(())
        } else {
            let next_offset = self
                .current_page
                .as_ref()
                .map_or(0, |p| p.section_page_offset + 1);
            self.start_new_page(sec_idx, sec_props, next_offset)
        }
    }

    fn column_geometry(&self, sec_props: &SectionProperties, col_idx: usize) -> (f32, f32) {
        let Some(ref page) = self.current_page else {
            return (72.0, 468.0);
        };
        let avail_w = (page.body_right - page.body_left).max(36.0);
        let col_count = usize::from(sec_props.column_count.max(1));
        if col_count <= 1 {
            return (page.body_left, avail_w);
        }

        if sec_props.custom_columns.len() == col_count {
            let mut x = page.body_left;
            for (i, col_def) in sec_props.custom_columns.iter().enumerate() {
                if i == col_idx {
                    return (x, col_def.width_pt.max(18.0));
                }
                x += col_def.width_pt.max(18.0) + col_def.space_pt.max(0.0);
            }
        }

        let space = sec_props.column_space_pt.max(0.0);
        #[allow(clippy::cast_precision_loss)]
        let total_space = space * (col_count.saturating_sub(1) as f32);
        #[allow(clippy::cast_precision_loss)]
        let col_w = ((avail_w - total_space) / (col_count as f32)).max(18.0);
        #[allow(clippy::cast_precision_loss)]
        let col_x = page.body_left + (col_idx as f32) * (col_w + space);
        (col_x, col_w)
    }

    fn effective_column_bottom(&self) -> f32 {
        let Some(ref page) = self.current_page else {
            return 720.0;
        };
        if page.footnote_height_pt > 0.0 {
            (page.body_bottom - page.footnote_height_pt - FOOTNOTE_SEP_HEIGHT_PT)
                .max(page.body_top + 36.0)
        } else {
            page.body_bottom
        }
    }

    fn layout_block_slice(
        &mut self,
        blocks: &[WordBlock],
        sec_idx: usize,
        sec_props: &SectionProperties,
        depth: u32,
    ) -> Result<()> {
        if depth > self.options.max_recursion_depth {
            return Err(Error::InvalidInput(format!(
                "Block recursion depth exceeds max_recursion_depth ({})",
                self.options.max_recursion_depth
            )));
        }

        for (idx, block) in blocks.iter().enumerate() {
            if self.reached_page_limit() {
                break;
            }
            let next_block = blocks.get(idx + 1);
            match block {
                WordBlock::Paragraph(para) => {
                    self.layout_paragraph_block(para, next_block, sec_idx, sec_props)?;
                }
                WordBlock::Table(table) => {
                    self.prev_borders = None;
                    self.layout_table_block(table, sec_idx, sec_props, depth)?;
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn layout_paragraph_block(
        &mut self,
        para: &WordParagraph,
        next_block: Option<&WordBlock>,
        sec_idx: usize,
        sec_props: &SectionProperties,
    ) -> Result<()> {
        self.block_counter = self.block_counter.wrapping_add(1);
        let block_id = self.block_counter;

        let p_style = self.doc.styles.resolve_paragraph_style(
            para.props.style_id.as_deref(),
            Some(&self.doc.numbering),
            &para.props,
        );
        let default_r_style = self.doc.styles.resolve_run_style(
            p_style.style_id.as_deref(),
            None,
            &RunProperties::default(),
            &self.doc.theme,
        );

        let list_marker = p_style
            .numbering
            .and_then(|num_ref| self.numbering.next_marker(num_ref, &self.doc.numbering));

        if p_style.page_break_before && !self.at_column_top {
            let next_offset = self
                .current_page
                .as_ref()
                .map_or(0, |p| p.section_page_offset + 1);
            self.start_new_page(sec_idx, sec_props, next_offset)?;
        }

        let same_contextual_style = p_style.contextual_spacing
            && p_style.style_id.is_some()
            && self.prev_style_id == p_style.style_id;
        // Space before is dropped at the top of a column. Word keeps it there after a hard
        // break or at the start of the document, which this does not distinguish yet.
        let space_before = if self.at_column_top || same_contextual_style {
            0.0
        } else {
            p_style.space_before_pt.max(self.prev_space_after)
        };
        self.cursor_y += space_before;

        self.advance_past_top_bottom_exclusions();

        let (col_x, col_w) = self.column_geometry(sec_props, self.current_col);
        let (eff_x, eff_w) = self.adjust_horizontal_for_exclusions(
            col_x + p_style.indent_left_pt,
            (col_w - p_style.indent_left_pt - p_style.indent_right_pt).max(18.0),
            self.cursor_y,
            16.0,
        );

        let page_num = self.finished_pages.len() + 1;
        let sec_pages = self
            .known_section_pages
            .get(sec_idx)
            .copied()
            .unwrap_or(1)
            .max(1);

        let extracted =
            self.extract_paragraph_items(para, &p_style, eff_x, eff_w, page_num, sec_pages)?;

        for &fn_id in &extracted.footnote_refs {
            self.register_footnote_on_page(fn_id, sec_props);
        }
        for &en_id in &extracted.endnote_refs {
            if self.seen_endnotes.insert(en_id) {
                let next_num = self.endnote_display_numbers.len() + 1;
                self.endnote_display_numbers.insert(en_id, next_num);
                self.referenced_endnotes.push(en_id);
            }
        }
        for cid in &extracted.comment_ids {
            if let Some(ref mut page) = self.current_page
                && !page.comment_ids.contains(cid)
            {
                page.comment_ids.push(cid.clone());
            }
        }

        for drawing in &extracted.floating_drawings {
            self.place_floating_drawing(drawing, col_x, col_w, self.cursor_y, sec_props)?;
        }

        if extracted.has_page_break
            && extracted.spans.is_empty()
            && extracted.inline_drawings.is_empty()
        {
            let next_offset = self
                .current_page
                .as_ref()
                .map_or(0, |p| p.section_page_offset + 1);
            self.start_new_page(sec_idx, sec_props, next_offset)?;
            return Ok(());
        }
        if extracted.has_column_break
            && extracted.spans.is_empty()
            && extracted.inline_drawings.is_empty()
        {
            self.advance_column_or_page(sec_idx, sec_props)?;
            return Ok(());
        }

        let first_line_indent = if list_marker.is_some() {
            0.0
        } else {
            p_style.indent_first_line_pt
        };

        #[allow(clippy::cast_possible_truncation)]
        let page_idx = self.finished_pages.len() as u32;

        let spec = ParagraphSpec {
            spans: extracted.spans.clone(),
            align: map_alignment(p_style.alignment),
            line_spacing: map_line_spacing(p_style.line_spacing),
            max_width: Some(eff_w),
            first_line_indent,
            origin: Point::ZERO,
            page_index: page_idx,
            block_index: block_id,
        };

        let shaped = self.text_engine.layout_paragraph(&spec, &mut self.font_sys);
        let default_line_h = default_r_style.font_size_pt * 1.2;
        let para_height = if shaped.lines.is_empty() {
            default_line_h
        } else {
            shaped.height
        };
        let inline_drawing_h = extracted
            .inline_drawings
            .iter()
            .map(|d| d.height_pt)
            .fold(0.0_f32, f32::max);
        let total_para_h = para_height.max(inline_drawing_h);

        let col_bottom = self.effective_column_bottom();
        let next_min_h = if p_style.keep_next && next_block.is_some() {
            18.0
        } else {
            0.0
        };
        if !self.at_column_top
            && ((p_style.keep_lines && self.cursor_y + total_para_h > col_bottom)
                || (p_style.keep_next && self.cursor_y + total_para_h + next_min_h > col_bottom))
        {
            self.advance_column_or_page(sec_idx, sec_props)?;
        }

        let line_count = shaped.lines.len();
        let col_bottom = self.effective_column_bottom();
        if !self.at_column_top && self.cursor_y + total_para_h > col_bottom && line_count > 0 {
            let avail_h = (col_bottom - self.cursor_y).max(0.0);
            let mut fit_lines = shaped
                .lines
                .iter()
                .take_while(|l| l.bottom <= avail_h)
                .count();

            if p_style.widow_control && line_count >= 2 {
                if fit_lines == 1 {
                    fit_lines = 0;
                } else if fit_lines + 1 == line_count && fit_lines >= 2 {
                    fit_lines -= 1;
                }
            }

            if fit_lines == 0 {
                self.advance_column_or_page(sec_idx, sec_props)?;
                self.emit_shaped_paragraph_range(
                    &shaped,
                    0..line_count,
                    &p_style,
                    &default_r_style,
                    list_marker.as_ref(),
                    &extracted,
                    sec_idx,
                    sec_props,
                    block_id,
                )?;
            } else {
                self.emit_shaped_paragraph_range(
                    &shaped,
                    0..fit_lines,
                    &p_style,
                    &default_r_style,
                    list_marker.as_ref(),
                    &extracted,
                    sec_idx,
                    sec_props,
                    block_id,
                )?;
                self.advance_column_or_page(sec_idx, sec_props)?;
                self.emit_shaped_paragraph_range(
                    &shaped,
                    fit_lines..line_count,
                    &p_style,
                    &default_r_style,
                    None,
                    &ExtractedParagraphItems {
                        inline_drawings: Vec::new(),
                        bookmarks: Vec::new(),
                        ..extracted
                    },
                    sec_idx,
                    sec_props,
                    block_id,
                )?;
            }
        } else {
            self.emit_shaped_paragraph_range(
                &shaped,
                0..line_count,
                &p_style,
                &default_r_style,
                list_marker.as_ref(),
                &extracted,
                sec_idx,
                sec_props,
                block_id,
            )?;
        }

        if extracted.has_page_break {
            let next_offset = self
                .current_page
                .as_ref()
                .map_or(0, |p| p.section_page_offset + 1);
            self.start_new_page(sec_idx, sec_props, next_offset)?;
        } else if extracted.has_column_break {
            self.advance_column_or_page(sec_idx, sec_props)?;
        } else {
            self.prev_space_after = p_style.space_after_pt;
            self.prev_style_id.clone_from(&p_style.style_id);
            self.prev_borders = Some(p_style.borders);
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn emit_shaped_paragraph_range(
        &mut self,
        shaped: &ShapedParagraph,
        line_range: std::ops::Range<usize>,
        p_style: &ResolvedParagraphStyle,
        default_r_style: &ResolvedRunStyle,
        list_marker: Option<&FormattedListMarker>,
        extracted: &ExtractedParagraphItems,
        sec_idx: usize,
        sec_props: &SectionProperties,
        block_id: u32,
    ) -> Result<()> {
        let (col_x, col_w) = self.column_geometry(sec_props, self.current_col);
        let para_x = col_x + p_style.indent_left_pt;
        let para_w = (col_w - p_style.indent_left_pt - p_style.indent_right_pt).max(18.0);
        let start_y = self.cursor_y;

        let slice_lines = shaped.lines.get(line_range.clone()).unwrap_or(&[]);
        let y_shift = slice_lines.first().map_or(0.0, |l| l.top);
        let text_h = slice_lines
            .last()
            .map_or(default_r_style.font_size_pt * 1.2, |l| l.bottom - y_shift);
        let inline_h = extracted
            .inline_drawings
            .iter()
            .map(|d| d.height_pt)
            .fold(0.0_f32, f32::max);
        let box_h = text_h.max(inline_h);

        let box_rect = Rect::new(para_x, start_y, para_w, box_h);

        self.emit_paragraph_box_decorations(box_rect, p_style);

        // Revision change bar in the left margin
        if extracted.has_revision
            && matches!(
                self.options.markup_mode,
                MarkupMode::AllMarkup | MarkupMode::SimpleMarkup
            )
            && let Some(ref mut page) = self.current_page
        {
            let bar_x = (page.body_left - 8.0).max(6.0);
            page.page.items.push(Item::Path(PathItem {
                commands: vec![
                    PathCommand::MoveTo(Point::new(bar_x, start_y)),
                    PathCommand::LineTo(Point::new(bar_x, start_y + box_h)),
                ],
                fill: None,
                stroke: Some(StrokeStyle {
                    color: Rgba::rgb(0xB5, 0x08, 0x2E),
                    width: 1.5,
                    line_cap: LineCap::Butt,
                    line_join: LineJoin::Miter,
                    dash_array: Vec::new(),
                    dash_offset: 0.0,
                }),
                fill_rule: FillRule::NonZero,
                transform: Affine::IDENTITY,
            }));
        }

        // Bookmarks
        if let Some(ref mut page) = self.current_page {
            for bm in &extracted.bookmarks {
                page.page.items.push(Item::Anchor(AnchorItem {
                    name: bm.clone(),
                    rect: Rect::new(para_x, start_y, 0.0, box_h),
                }));
            }
        }

        // Numbering marker on first line
        if let Some(marker) = list_marker
            && !marker.text.is_empty()
        {
            let marker_r = self.doc.styles.resolve_run_style(
                p_style.style_id.as_deref(),
                marker.run_props.style_id.as_deref(),
                &marker.run_props,
                &self.doc.theme,
            );
            let marker_color = parse_hex_color(&marker_r.color).unwrap_or(Rgba::BLACK);
            let marker_x = (para_x + p_style.indent_first_line_pt).max(col_x);
            #[allow(clippy::cast_possible_truncation)]
            let page_idx = self.finished_pages.len() as u32;
            let marker_spec = ParagraphSpec {
                spans: vec![SpanSpec {
                    text: marker.text.clone(),
                    family: marker_r.font_family,
                    size: marker_r.font_size_pt,
                    bold: marker_r.bold,
                    italic: marker_r.italic,
                    color: marker_color,
                    run_index: 0,
                    ..SpanSpec::default()
                }],
                align: TextAlign::Left,
                line_spacing: map_line_spacing(p_style.line_spacing),
                max_width: None,
                first_line_indent: 0.0,
                origin: Point::new(marker_x, start_y),
                page_index: page_idx,
                block_index: block_id,
            };
            let shaped_marker = self
                .text_engine
                .layout_paragraph(&marker_spec, &mut self.font_sys);
            if let Some(ref mut page) = self.current_page {
                for run in shaped_marker.runs {
                    page.page.items.push(Item::Glyphs(run));
                }
            }
        }

        self.emit_tab_stop_decorations(
            p_style,
            default_r_style,
            para_x,
            start_y,
            box_h,
            extracted.has_tab,
        );

        let mut hyperlink_bounds: BTreeMap<usize, Rect> = BTreeMap::new();
        if let Some(ref mut page) = self.current_page {
            for line in slice_lines {
                let line_y_top = start_y + (line.top - y_shift);
                let line_h = (line.bottom - line.top).max(10.0);
                let (shifted_x, _) = adjust_band_for_exclusions(
                    &page.exclusions,
                    para_x,
                    para_w,
                    line_y_top,
                    line_h,
                );
                // Lines were broken at the full paragraph width, so a line is only moved past a
                // wrap polygon when it still fits; otherwise it would run off the margin.
                let line_right = shaped
                    .runs
                    .get(line.run_range.clone())
                    .unwrap_or_default()
                    .iter()
                    .map(|r| {
                        r.transform.coeffs[4] + r.glyphs.iter().map(|g| g.advance).sum::<f32>()
                    })
                    .fold(0.0_f32, f32::max);
                let line_para_x = if shifted_x + line_right <= para_x + para_w + 0.5 {
                    shifted_x
                } else {
                    para_x
                };
                for run_idx in line.run_range.clone() {
                    if let Some(mut run) = shaped.runs.get(run_idx).cloned() {
                        run.transform.coeffs[4] =
                            normalize_zero(run.transform.coeffs[4] + line_para_x);
                        run.transform.coeffs[5] =
                            normalize_zero(run.transform.coeffs[5] + start_y - y_shift);

                        let span_id = run.source.run as usize;
                        if let Some(&link_idx) = extracted.span_to_hyperlink.get(&span_id) {
                            let run_w: f32 = run.glyphs.iter().map(|g| g.advance).sum();
                            let r_rect = Rect::new(
                                run.transform.coeffs[4],
                                run.transform.coeffs[5] - run.size * 0.85,
                                run_w.max(4.0),
                                run.size * 1.15,
                            );
                            hyperlink_bounds
                                .entry(link_idx)
                                .and_modify(|prev| *prev = prev.union(r_rect))
                                .or_insert(r_rect);
                        }

                        let extra_items =
                            build_extra_run_decorations(&run, extracted.span_flags.get(span_id));
                        page.page.items.push(Item::Glyphs(run));
                        page.page.items.extend(extra_items);
                    }
                }
            }

            for (link_idx, rect) in hyperlink_bounds {
                if let Some(target) = extracted.hyperlink_targets.get(link_idx) {
                    page.page.items.push(Item::Link(LinkItem {
                        rect,
                        target: target.clone(),
                    }));
                }
            }
        }

        let mut inline_x = para_x;
        let inline_y = if slice_lines.is_empty() {
            start_y
        } else {
            start_y + text_h - inline_h.min(text_h)
        };
        for drawing in &extracted.inline_drawings {
            let dest = Rect::new(
                inline_x,
                inline_y,
                drawing.width_pt.min(para_w),
                drawing.height_pt,
            );
            self.render_drawing_into_rect(drawing, dest, sec_idx, sec_props)?;
            inline_x += drawing.width_pt + 4.0;
        }

        self.cursor_y = start_y + box_h;
        self.at_column_top = false;
        Ok(())
    }

    fn emit_paragraph_box_decorations(&mut self, rect: Rect, p_style: &ResolvedParagraphStyle) {
        let Some(ref mut page) = self.current_page else {
            return;
        };

        if let Some(fill_color) = p_style.shading_fill.as_deref().and_then(parse_hex_color) {
            page.page.items.push(Item::Path(PathItem {
                commands: rect_path_commands(rect),
                fill: Some(FillPaint::Solid(fill_color)),
                stroke: None,
                fill_rule: FillRule::NonZero,
                transform: Affine::IDENTITY,
            }));
        }

        let b = &p_style.borders;
        let merge_with_prev = self
            .prev_borders
            .as_ref()
            .is_some_and(|prev| prev == b && b.between.is_some());

        if merge_with_prev {
            if let Some(ref btw) = b.between {
                emit_border_line(
                    &mut page.page.items,
                    btw,
                    Point::new(rect.x, rect.y),
                    Point::new(rect.right(), rect.y),
                );
            }
        } else if let Some(ref top) = b.top {
            emit_border_line(
                &mut page.page.items,
                top,
                Point::new(rect.x, rect.y),
                Point::new(rect.right(), rect.y),
            );
        }

        if let Some(ref bottom) = b.bottom {
            emit_border_line(
                &mut page.page.items,
                bottom,
                Point::new(rect.x, rect.bottom()),
                Point::new(rect.right(), rect.bottom()),
            );
        }
        if let Some(ref left) = b.left {
            emit_border_line(
                &mut page.page.items,
                left,
                Point::new(rect.x, rect.y),
                Point::new(rect.x, rect.bottom()),
            );
        }
        if let Some(ref right) = b.right {
            emit_border_line(
                &mut page.page.items,
                right,
                Point::new(rect.right(), rect.y),
                Point::new(rect.right(), rect.bottom()),
            );
        }
    }

    fn emit_tab_stop_decorations(
        &mut self,
        p_style: &ResolvedParagraphStyle,
        default_r_style: &ResolvedRunStyle,
        para_x: f32,
        start_y: f32,
        box_h: f32,
        has_tab: bool,
    ) {
        let Some(ref mut page) = self.current_page else {
            return;
        };
        let color = parse_hex_color(&default_r_style.color).unwrap_or(Rgba::BLACK);
        for stop in &p_style.tab_stops {
            let x = para_x + stop.position_pt;
            if stop.kind == TabStopKind::Bar {
                page.page.items.push(Item::Path(PathItem {
                    commands: vec![
                        PathCommand::MoveTo(Point::new(x, start_y)),
                        PathCommand::LineTo(Point::new(x, start_y + box_h)),
                    ],
                    fill: None,
                    stroke: Some(StrokeStyle {
                        color,
                        width: 0.75,
                        line_cap: LineCap::Butt,
                        line_join: LineJoin::Miter,
                        dash_array: Vec::new(),
                        dash_offset: 0.0,
                    }),
                    fill_rule: FillRule::NonZero,
                    transform: Affine::IDENTITY,
                }));
            } else if has_tab && stop.leader != TabLeader::None {
                let leader_y = start_y + default_r_style.font_size_pt * 0.85;
                let leader_start = para_x + 24.0;
                let leader_end = (x - 6.0).max(leader_start);
                if leader_end > leader_start {
                    let dash_array = match stop.leader {
                        TabLeader::Dot => vec![1.0, 3.0],
                        TabLeader::Hyphen => vec![3.0, 3.0],
                        TabLeader::Underscore => Vec::new(),
                        TabLeader::None => continue,
                    };
                    page.page.items.push(Item::Path(PathItem {
                        commands: vec![
                            PathCommand::MoveTo(Point::new(leader_start, leader_y)),
                            PathCommand::LineTo(Point::new(leader_end, leader_y)),
                        ],
                        fill: None,
                        stroke: Some(StrokeStyle {
                            color,
                            width: 0.75,
                            line_cap: LineCap::Round,
                            line_join: LineJoin::Miter,
                            dash_array,
                            dash_offset: 0.0,
                        }),
                        fill_rule: FillRule::NonZero,
                        transform: Affine::IDENTITY,
                    }));
                }
            }
        }
    }

    fn extract_paragraph_items(
        &mut self,
        para: &WordParagraph,
        p_style: &ResolvedParagraphStyle,
        para_x: f32,
        para_w: f32,
        page_num: usize,
        sec_pages: usize,
    ) -> Result<ExtractedParagraphItems> {
        let mut out = ExtractedParagraphItems::default();
        let mut cursor_x_est = 0.0_f32;
        let default_tab_step = self.doc.settings.default_tab_stop_pt.max(18.0);

        self.collect_contents(
            &para.content,
            p_style,
            para_x,
            para_w,
            default_tab_step,
            &mut cursor_x_est,
            page_num,
            sec_pages,
            None,
            &mut out,
            0,
        )?;

        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_contents(
        &mut self,
        contents: &[ParagraphContent],
        p_style: &ResolvedParagraphStyle,
        para_x: f32,
        para_w: f32,
        default_tab_step: f32,
        cursor_x_est: &mut f32,
        page_num: usize,
        sec_pages: usize,
        active_hyperlink: Option<usize>,
        out: &mut ExtractedParagraphItems,
        depth: u32,
    ) -> Result<()> {
        if depth > self.options.max_recursion_depth {
            return Ok(());
        }

        for content in contents {
            match content {
                ParagraphContent::Run(run) => {
                    self.collect_run(
                        run,
                        p_style,
                        default_tab_step,
                        cursor_x_est,
                        active_hyperlink,
                        out,
                    );
                }
                ParagraphContent::Field(field) => {
                    self.collect_field(
                        field,
                        p_style,
                        para_x,
                        para_w,
                        default_tab_step,
                        cursor_x_est,
                        page_num,
                        sec_pages,
                        active_hyperlink,
                        out,
                        depth + 1,
                    )?;
                }
                ParagraphContent::Hyperlink(link) => {
                    let link_idx = out.hyperlink_targets.len();
                    out.hyperlink_targets.push(link.target.clone());
                    self.collect_contents(
                        &link.content,
                        p_style,
                        para_x,
                        para_w,
                        default_tab_step,
                        cursor_x_est,
                        page_num,
                        sec_pages,
                        Some(link_idx),
                        out,
                        depth + 1,
                    )?;
                }
                ParagraphContent::BookmarkStart { name, .. } => {
                    if !name.is_empty() && !name.starts_with('_') {
                        out.bookmarks.push(name.clone());
                    }
                }
                ParagraphContent::BookmarkEnd { .. } | ParagraphContent::CommentRangeEnd(_) => {}
                ParagraphContent::CommentRangeStart(id) => {
                    if matches!(
                        self.options.markup_mode,
                        MarkupMode::AllMarkup | MarkupMode::SimpleMarkup
                    ) {
                        out.comment_ids.push(id.clone());
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn collect_run(
        &mut self,
        run: &WordRun,
        p_style: &ResolvedParagraphStyle,
        default_tab_step: f32,
        cursor_x_est: &mut f32,
        active_hyperlink: Option<usize>,
        out: &mut ExtractedParagraphItems,
    ) {
        if let Some(ref rev) = run.revision {
            out.has_revision = true;
            match (self.options.markup_mode, rev.kind) {
                (
                    MarkupMode::SimpleMarkup | MarkupMode::NoMarkup,
                    RevisionKind::Deleted | RevisionKind::MoveFrom,
                )
                | (MarkupMode::Original, RevisionKind::Inserted | RevisionKind::MoveTo) => {
                    return;
                }
                _ => {}
            }
        }

        let r_style = self.doc.styles.resolve_run_style(
            p_style.style_id.as_deref(),
            run.props.style_id.as_deref(),
            &run.props,
            &self.doc.theme,
        );
        if r_style.vanish
            && (!self.options.show_hidden_text || self.options.markup_mode == MarkupMode::NoMarkup)
        {
            return;
        }

        let mut color = parse_hex_color(&r_style.color).unwrap_or(Rgba::BLACK);
        let mut underline = r_style.underline != UnderlineKind::None;
        let mut strikethrough = r_style.strike || r_style.double_strike;
        let highlight = r_style.highlight_color.as_deref().and_then(parse_hex_color);

        if self.options.markup_mode == MarkupMode::AllMarkup
            && let Some(ref rev) = run.revision
        {
            let author = rev.author.as_deref().unwrap_or("Reviewer");
            color = author_color(author);
            match rev.kind {
                RevisionKind::Inserted | RevisionKind::MoveTo => underline = true,
                RevisionKind::Deleted | RevisionKind::MoveFrom => strikethrough = true,
            }
        }

        let script = match r_style.vert_align {
            VerticalRunAlign::Baseline => ScriptPosition::Normal,
            VerticalRunAlign::Superscript => ScriptPosition::Superscript,
            VerticalRunAlign::Subscript => ScriptPosition::Subscript,
        };

        let mut text_buf = String::new();
        for item in &run.items {
            match item {
                RunItem::Text(t) => text_buf.push_str(t),
                RunItem::Tab => {
                    out.has_tab = true;
                    let target_x =
                        next_tab_stop_x(&p_style.tab_stops, *cursor_x_est, default_tab_step);
                    let space_w = (r_style.font_size_pt * 0.30).max(2.0);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let n_spaces = (((target_x - *cursor_x_est).max(space_w) / space_w).round()
                        as usize)
                        .clamp(1, 32);
                    for _ in 0..n_spaces {
                        text_buf.push(' ');
                    }
                    *cursor_x_est = target_x;
                }
                RunItem::LineBreak => text_buf.push('\n'),
                RunItem::ColumnBreak => out.has_column_break = true,
                RunItem::PageBreak => out.has_page_break = true,
                RunItem::Symbol { font, char_code } => {
                    let raw_ch = char::from_u32(u32::from(*char_code)).unwrap_or('•');
                    text_buf.push(map_symbol_char(font, raw_ch));
                }
                RunItem::FootnoteRef(id) => {
                    out.footnote_refs.push(*id);
                    let num = self
                        .footnote_display_numbers
                        .get(id)
                        .copied()
                        .unwrap_or_else(|| self.footnote_display_numbers.len() + 1);
                    self.footnote_display_numbers.entry(*id).or_insert(num);
                    self.push_styled_span(
                        &num.to_string(),
                        &r_style,
                        color,
                        underline,
                        strikethrough,
                        highlight,
                        ScriptPosition::Superscript,
                        active_hyperlink,
                        out,
                    );
                }
                RunItem::EndnoteRef(id) => {
                    out.endnote_refs.push(*id);
                    let num = self
                        .endnote_display_numbers
                        .get(id)
                        .copied()
                        .unwrap_or_else(|| self.endnote_display_numbers.len() + 1);
                    self.endnote_display_numbers.entry(*id).or_insert(num);
                    let roman =
                        format_number(i32::try_from(num).unwrap_or(1), &NumberFormat::LowerRoman);
                    self.push_styled_span(
                        &roman,
                        &r_style,
                        color,
                        underline,
                        strikethrough,
                        highlight,
                        ScriptPosition::Superscript,
                        active_hyperlink,
                        out,
                    );
                }
                RunItem::Drawing(drawing) => {
                    if drawing.anchor.is_some() {
                        out.floating_drawings.push(drawing.clone());
                    } else {
                        out.inline_drawings.push(drawing.clone());
                    }
                }
                RunItem::NoBreakHyphen => text_buf.push('\u{2011}'),
                RunItem::SoftHyphen => {}
            }
        }

        if !text_buf.is_empty() {
            #[allow(clippy::cast_precision_loss)]
            {
                *cursor_x_est += (text_buf.chars().count() as f32) * r_style.font_size_pt * 0.5;
            }
            self.push_styled_span(
                &text_buf,
                &r_style,
                color,
                underline,
                strikethrough,
                highlight,
                script,
                active_hyperlink,
                out,
            );
        }
    }

    #[allow(clippy::too_many_arguments, clippy::unused_self)]
    fn push_styled_span(
        &self,
        raw_text: &str,
        r_style: &ResolvedRunStyle,
        color: Rgba,
        underline: bool,
        strikethrough: bool,
        highlight: Option<Rgba>,
        script: ScriptPosition,
        active_hyperlink: Option<usize>,
        out: &mut ExtractedParagraphItems,
    ) {
        let family = pick_font_family(raw_text, r_style);

        if r_style.small_caps && !r_style.caps {
            let mut seg = String::new();
            let mut seg_is_lower: Option<bool> = None;
            for ch in raw_text.chars() {
                let is_low = ch.is_lowercase();
                if seg_is_lower.is_some_and(|prev| prev != is_low) && !seg.is_empty() {
                    let size = if seg_is_lower == Some(true) {
                        (r_style.font_size_pt * 0.78).max(4.0)
                    } else {
                        r_style.font_size_pt
                    };
                    Self::record_span(
                        std::mem::take(&mut seg),
                        family.clone(),
                        size,
                        r_style,
                        color,
                        underline,
                        strikethrough,
                        highlight,
                        script,
                        active_hyperlink,
                        out,
                    );
                }
                seg_is_lower = Some(is_low);
                if is_low {
                    for up in ch.to_uppercase() {
                        seg.push(up);
                    }
                } else {
                    seg.push(ch);
                }
            }
            if !seg.is_empty() {
                let size = if seg_is_lower == Some(true) {
                    (r_style.font_size_pt * 0.78).max(4.0)
                } else {
                    r_style.font_size_pt
                };
                Self::record_span(
                    seg,
                    family,
                    size,
                    r_style,
                    color,
                    underline,
                    strikethrough,
                    highlight,
                    script,
                    active_hyperlink,
                    out,
                );
            }
            return;
        }

        let final_text = if r_style.caps {
            raw_text.to_uppercase()
        } else {
            raw_text.to_string()
        };

        Self::record_span(
            final_text,
            family,
            r_style.font_size_pt,
            r_style,
            color,
            underline,
            strikethrough,
            highlight,
            script,
            active_hyperlink,
            out,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn record_span(
        text: String,
        family: String,
        size: f32,
        r_style: &ResolvedRunStyle,
        color: Rgba,
        underline: bool,
        strikethrough: bool,
        highlight: Option<Rgba>,
        script: ScriptPosition,
        active_hyperlink: Option<usize>,
        out: &mut ExtractedParagraphItems,
    ) {
        #[allow(clippy::cast_possible_truncation)]
        let run_index = out.spans.len() as u32;
        if let Some(link_idx) = active_hyperlink {
            out.span_to_hyperlink.insert(out.spans.len(), link_idx);
        }
        out.span_flags.push(SpanExtraFlags {
            hidden_dotted: r_style.vanish,
            double_strike: r_style.double_strike,
            underline_kind: r_style.underline,
        });
        out.spans.push(SpanSpec {
            text,
            family,
            size,
            bold: r_style.bold,
            italic: r_style.italic,
            color,
            underline,
            strikethrough,
            highlight,
            script,
            run_index,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_field(
        &mut self,
        field: &WordField,
        p_style: &ResolvedParagraphStyle,
        para_x: f32,
        para_w: f32,
        default_tab_step: f32,
        cursor_x_est: &mut f32,
        page_num: usize,
        sec_pages: usize,
        active_hyperlink: Option<usize>,
        out: &mut ExtractedParagraphItems,
        depth: u32,
    ) -> Result<()> {
        let dynamic_value = match field.kind {
            FieldKind::Page => Some(page_num.to_string()),
            FieldKind::NumPages => Some(self.known_total_pages.max(page_num).to_string()),
            FieldKind::SectionPages => Some(sec_pages.to_string()),
            _ => None,
        };

        if let Some(val) = dynamic_value {
            let (template_rpr, template_rev) = field
                .cached_result
                .iter()
                .find_map(|c| {
                    if let ParagraphContent::Run(r) = c {
                        Some((r.props.clone(), r.revision.clone()))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let synth_run = WordRun {
                props: template_rpr,
                items: vec![RunItem::Text(val)],
                revision: template_rev,
            };
            self.collect_run(
                &synth_run,
                p_style,
                default_tab_step,
                cursor_x_est,
                active_hyperlink,
                out,
            );
            return Ok(());
        }

        self.collect_contents(
            &field.cached_result,
            p_style,
            para_x,
            para_w,
            default_tab_step,
            cursor_x_est,
            page_num,
            sec_pages,
            active_hyperlink,
            out,
            depth,
        )
    }

    // =========================================================================
    // M2 — Table Layout Engine
    // =========================================================================

    #[allow(clippy::too_many_lines)]
    fn layout_table_block(
        &mut self,
        table: &WordTable,
        sec_idx: usize,
        sec_props: &SectionProperties,
        depth: u32,
    ) -> Result<()> {
        if table.rows.is_empty() {
            return Ok(());
        }

        let (col_x, col_w) = self.column_geometry(sec_props, self.current_col);
        let col_widths = self.solve_table_column_widths(table, col_w);
        let total_table_w: f32 = col_widths.iter().sum();

        let table_x = match table.props.alignment {
            WordAlignment::Center => col_x + ((col_w - total_table_w) * 0.5).max(0.0),
            WordAlignment::Right => col_x + (col_w - total_table_w).max(0.0),
            _ => col_x + table.props.indent_left_pt,
        };

        let num_rows = table.rows.len();
        let num_cols = col_widths.len().max(1);
        let mut measured_rows = Vec::with_capacity(num_rows);

        for (r_idx, row) in table.rows.iter().enumerate() {
            measured_rows.push(self.measure_table_row(
                table,
                row,
                r_idx,
                num_rows,
                num_cols,
                &col_widths,
                depth,
            )?);
        }

        let vmerge_spans = compute_vmerge_row_spans(&table.rows, num_cols);
        let header_row_count = table.rows.iter().take_while(|r| r.is_header).count();

        for (r_idx, m_row) in measured_rows.iter().enumerate() {
            let row_h = m_row.height;
            let col_bottom = self.effective_column_bottom();

            if !self.at_column_top && self.cursor_y + row_h > col_bottom {
                self.advance_column_or_page(sec_idx, sec_props)?;
                if r_idx >= header_row_count && header_row_count > 0 {
                    for hdr_idx in 0..header_row_count {
                        let hdr_row = &measured_rows[hdr_idx];
                        self.emit_table_row(
                            table,
                            hdr_row,
                            hdr_idx,
                            num_rows,
                            table_x,
                            &col_widths,
                            &vmerge_spans,
                            &measured_rows,
                        );
                        self.cursor_y += hdr_row.height;
                        self.at_column_top = false;
                    }
                }
            }

            self.emit_table_row(
                table,
                m_row,
                r_idx,
                num_rows,
                table_x,
                &col_widths,
                &vmerge_spans,
                &measured_rows,
            );
            self.cursor_y += row_h;
            self.at_column_top = false;
        }

        self.prev_space_after = 4.0;
        Ok(())
    }

    #[allow(clippy::unused_self)]
    fn solve_table_column_widths(&self, table: &WordTable, avail_w: f32) -> Vec<f32> {
        let max_cols = table
            .rows
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| usize::from(c.grid_span.max(1)))
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(1)
            .max(table.grid_col_widths_pt.len())
            .max(1);

        let target_total_w = match table.props.preferred_width {
            TableWidth::Pt(pt) if pt > 0.0 => pt.min(avail_w),
            TableWidth::Percent(pct) if pct > 0.0 => (avail_w * (pct / 100.0)).min(avail_w),
            _ => avail_w,
        };

        #[allow(clippy::cast_precision_loss)]
        let mut widths = if table.grid_col_widths_pt.len() == max_cols {
            table.grid_col_widths_pt.clone()
        } else {
            vec![target_total_w / (max_cols as f32); max_cols]
        };

        for row in &table.rows {
            let mut col_cursor = 0usize;
            for cell in &row.cells {
                let span = usize::from(cell.grid_span.max(1));
                if span == 1
                    && col_cursor < widths.len()
                    && let Some(pw) = cell.preferred_width
                {
                    match pw {
                        TableWidth::Pt(w) if w > 0.0 => widths[col_cursor] = w,
                        TableWidth::Percent(pct) if pct > 0.0 => {
                            widths[col_cursor] = target_total_w * (pct / 100.0);
                        }
                        _ => {}
                    }
                }
                col_cursor += span;
            }
        }

        if table.props.layout_mode == TableLayoutMode::AutoFit {
            for row in &table.rows {
                let mut col_cursor = 0usize;
                for cell in &row.cells {
                    let span = usize::from(cell.grid_span.max(1));
                    if span == 1 && col_cursor < widths.len() {
                        let min_w = estimate_cell_min_width(cell);
                        if widths[col_cursor] < min_w {
                            widths[col_cursor] = min_w;
                        }
                    }
                    col_cursor += span;
                }
            }
        }

        let sum: f32 = widths.iter().sum();
        if sum > avail_w && sum > 0.0 {
            let scale = avail_w / sum;
            for w in &mut widths {
                *w = (*w * scale).max(12.0);
            }
        } else if matches!(
            table.props.preferred_width,
            TableWidth::Percent(_) | TableWidth::Pt(_)
        ) && sum > 0.0
            && (sum - target_total_w).abs() > 1.0
        {
            let scale = target_total_w / sum;
            for w in &mut widths {
                *w = (*w * scale).max(12.0);
            }
        }

        widths
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn measure_table_row(
        &mut self,
        table: &WordTable,
        row: &WordTableRow,
        r_idx: usize,
        num_rows: usize,
        num_cols: usize,
        col_widths: &[f32],
        depth: u32,
    ) -> Result<MeasuredTableRow> {
        let mut col_cursor = 0usize;
        let mut cells = Vec::with_capacity(row.cells.len());
        let mut max_cell_h = 16.0_f32;

        for cell in &row.cells {
            let span = usize::from(cell.grid_span.max(1));
            let cell_w: f32 = col_widths
                .iter()
                .skip(col_cursor)
                .take(span)
                .sum::<f32>()
                .max(18.0);
            let m_cell = self.measure_table_cell(
                table, cell, r_idx, col_cursor, num_rows, num_cols, cell_w, depth,
            )?;
            if cell.v_merge != VerticalMerge::Continue {
                max_cell_h = max_cell_h.max(m_cell.content_height);
            }
            cells.push(m_cell);
            col_cursor += span;
        }

        let height = match row.height_pt {
            Some(h) if row.height_exact => h.max(8.0),
            Some(h) => max_cell_h.max(h),
            None => max_cell_h,
        };

        Ok(MeasuredTableRow { cells, height })
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn measure_table_cell(
        &mut self,
        table: &WordTable,
        cell: &WordTableCell,
        r_idx: usize,
        c_idx: usize,
        num_rows: usize,
        num_cols: usize,
        cell_w: f32,
        depth: u32,
    ) -> Result<MeasuredTableCell> {
        let margins = cell.margins.unwrap_or(table.props.cell_margins);
        let inner_w = (cell_w - margins.left_pt - margins.right_pt).max(6.0);

        let cond_props =
            self.resolve_table_cell_conditional_style(table, r_idx, c_idx, num_rows, num_cols);

        let shading_fill = cell
            .shading_fill
            .clone()
            .or(cond_props.shading_fill)
            .or_else(|| table.props.shading_fill.clone());

        let mut items = Vec::new();
        let mut y_cursor = 0.0_f32;
        #[allow(clippy::cast_possible_truncation)]
        let page_idx = self.finished_pages.len() as u32;
        let page_num = self.finished_pages.len() + 1;

        for block in &cell.blocks {
            match block {
                WordBlock::Paragraph(para) => {
                    self.block_counter = self.block_counter.wrapping_add(1);
                    let p_style = self.doc.styles.resolve_paragraph_style(
                        para.props.style_id.as_deref(),
                        Some(&self.doc.numbering),
                        &para.props,
                    );
                    let default_r = self.doc.styles.resolve_run_style(
                        p_style.style_id.as_deref(),
                        None,
                        &RunProperties::default(),
                        &self.doc.theme,
                    );
                    let mut extracted =
                        self.extract_paragraph_items(para, &p_style, 0.0, inner_w, page_num, 1)?;
                    if let Some(ref cond_rpr) = cond_props.run_props {
                        for sp in &mut extracted.spans {
                            if let Some(b) = cond_rpr.bold {
                                sp.bold = b.apply_direct();
                            }
                            if let Some(i) = cond_rpr.italic {
                                sp.italic = i.apply_direct();
                            }
                            if let Some(ref c) = cond_rpr.color
                                && let Some(rgba) = parse_hex_color(c)
                            {
                                sp.color = rgba;
                            }
                        }
                    }
                    let spec = ParagraphSpec {
                        spans: extracted.spans,
                        align: map_alignment(p_style.alignment),
                        line_spacing: map_line_spacing(p_style.line_spacing),
                        max_width: Some(inner_w),
                        first_line_indent: p_style.indent_first_line_pt,
                        origin: Point::new(0.0, y_cursor),
                        page_index: page_idx,
                        block_index: self.block_counter,
                    };
                    let shaped = self.text_engine.layout_paragraph(&spec, &mut self.font_sys);
                    let h = if shaped.lines.is_empty() {
                        default_r.font_size_pt * 1.15
                    } else {
                        shaped.height
                    };
                    for run in shaped.runs {
                        items.push(Item::Glyphs(run));
                    }
                    y_cursor += h + p_style.space_after_pt.min(6.0);
                }
                WordBlock::Table(nested) if depth < self.options.max_recursion_depth => {
                    let nested_widths = self.solve_table_column_widths(nested, inner_w);
                    for (nr_idx, nrow) in nested.rows.iter().enumerate() {
                        let m_nrow = self.measure_table_row(
                            nested,
                            nrow,
                            nr_idx,
                            nested.rows.len(),
                            nested_widths.len(),
                            &nested_widths,
                            depth + 1,
                        )?;
                        let mut nx = 0.0_f32;
                        for m_ncell in &m_nrow.cells {
                            for it in &m_ncell.items {
                                items.push(translate_item(
                                    it.clone(),
                                    nx + m_ncell.pad_left,
                                    y_cursor + m_ncell.pad_top,
                                ));
                            }
                            nx += m_ncell.width;
                        }
                        y_cursor += m_nrow.height;
                    }
                }
                WordBlock::Table(_) => {}
            }
        }

        let content_height = y_cursor + margins.top_pt + margins.bottom_pt;
        Ok(MeasuredTableCell {
            col_index: c_idx,
            grid_span: usize::from(cell.grid_span.max(1)),
            v_merge: cell.v_merge,
            vert_align: cell.vertical_align,
            width: cell_w,
            pad_top: margins.top_pt,
            pad_bottom: margins.bottom_pt,
            pad_left: margins.left_pt,
            content_text_height: y_cursor,
            content_height,
            shading_fill,
            borders: cell.borders.clone(),
            cond_borders: cond_props.borders,
            items,
        })
    }

    fn resolve_table_cell_conditional_style(
        &self,
        table: &WordTable,
        r_idx: usize,
        c_idx: usize,
        num_rows: usize,
        num_cols: usize,
    ) -> ResolvedCellConditional {
        let mut out = ResolvedCellConditional::default();
        let Some(ref style_id) = table.props.style_id else {
            return out;
        };
        let Some(style) = self.doc.styles.styles.get(style_id) else {
            return out;
        };

        let apply_cond = |region: TableStyleRegion, dest: &mut ResolvedCellConditional| {
            if let Some(cond) = style.table_conditionals.get(&region) {
                if cond.shading_fill.is_some() {
                    dest.shading_fill.clone_from(&cond.shading_fill);
                }
                if cond.borders != WordBorders::default() {
                    dest.borders = Some(cond.borders.clone());
                }
                dest.run_props = Some(cond.run_props.clone());
            }
        };

        apply_cond(TableStyleRegion::WholeTable, &mut out);

        if table.props.look_h_band {
            let body_r = if table.props.look_first_row {
                r_idx.saturating_sub(1)
            } else {
                r_idx
            };
            if body_r.is_multiple_of(2) {
                apply_cond(TableStyleRegion::Band1Horz, &mut out);
            } else {
                apply_cond(TableStyleRegion::Band2Horz, &mut out);
            }
        }
        if table.props.look_v_band {
            if c_idx.is_multiple_of(2) {
                apply_cond(TableStyleRegion::Band1Vert, &mut out);
            } else {
                apply_cond(TableStyleRegion::Band2Vert, &mut out);
            }
        }
        if table.props.look_first_col && c_idx == 0 {
            apply_cond(TableStyleRegion::FirstCol, &mut out);
        }
        if table.props.look_last_col && c_idx + 1 == num_cols {
            apply_cond(TableStyleRegion::LastCol, &mut out);
        }
        if table.props.look_first_row && r_idx == 0 {
            apply_cond(TableStyleRegion::FirstRow, &mut out);
        }
        if table.props.look_last_row && r_idx + 1 == num_rows {
            apply_cond(TableStyleRegion::LastRow, &mut out);
        }

        out
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn emit_table_row(
        &mut self,
        table: &WordTable,
        m_row: &MeasuredTableRow,
        r_idx: usize,
        num_rows: usize,
        table_x: f32,
        col_widths: &[f32],
        vmerge_spans: &[Vec<usize>],
        all_rows: &[MeasuredTableRow],
    ) {
        let Some(ref mut page) = self.current_page else {
            return;
        };
        let row_y = self.cursor_y;
        let num_cols = col_widths.len();

        let mut x_cursor = table_x;
        for m_cell in &m_row.cells {
            if m_cell.v_merge == VerticalMerge::Continue {
                x_cursor += m_cell.width;
                continue;
            }

            let row_span = vmerge_spans
                .get(r_idx)
                .and_then(|r| r.get(m_cell.col_index))
                .copied()
                .unwrap_or(1)
                .max(1);
            let cell_h: f32 = all_rows
                .iter()
                .skip(r_idx)
                .take(row_span)
                .map(|r| r.height)
                .sum::<f32>()
                .max(m_row.height);

            let cell_rect = Rect::new(x_cursor, row_y, m_cell.width, cell_h);

            if let Some(fill_color) = m_cell.shading_fill.as_deref().and_then(parse_hex_color) {
                page.page.items.push(Item::Path(PathItem {
                    commands: rect_path_commands(cell_rect),
                    fill: Some(FillPaint::Solid(fill_color)),
                    stroke: None,
                    fill_rule: FillRule::NonZero,
                    transform: Affine::IDENTITY,
                }));
            }

            let avail_inner_h = (cell_h - m_cell.pad_top - m_cell.pad_bottom).max(0.0);
            let v_offset = match m_cell.vert_align {
                PageVerticalAlign::Top | PageVerticalAlign::Both => m_cell.pad_top,
                PageVerticalAlign::Center => {
                    m_cell.pad_top + ((avail_inner_h - m_cell.content_text_height) * 0.5).max(0.0)
                }
                PageVerticalAlign::Bottom => {
                    m_cell.pad_top + (avail_inner_h - m_cell.content_text_height).max(0.0)
                }
            };

            for item in &m_cell.items {
                page.page.items.push(translate_item(
                    item.clone(),
                    x_cursor + m_cell.pad_left,
                    row_y + v_offset,
                ));
            }

            let is_top = r_idx == 0;
            let is_bottom = r_idx + row_span >= num_rows;
            let is_left = m_cell.col_index == 0;
            let is_right = m_cell.col_index + m_cell.grid_span >= num_cols;

            let tbl_b = &table.props.borders;
            let cb = &m_cell.borders;
            let cond_b = m_cell.cond_borders.as_ref();

            let top_spec = resolve_winning_border(
                cb.top.as_ref(),
                cond_b.and_then(|b| b.top.as_ref()),
                if is_top {
                    tbl_b.top.as_ref()
                } else {
                    tbl_b.inside_h.as_ref()
                },
            );
            let bottom_spec = resolve_winning_border(
                cb.bottom.as_ref(),
                cond_b.and_then(|b| b.bottom.as_ref()),
                if is_bottom {
                    tbl_b.bottom.as_ref()
                } else {
                    tbl_b.inside_h.as_ref()
                },
            );
            let left_spec = resolve_winning_border(
                cb.left.as_ref(),
                cond_b.and_then(|b| b.left.as_ref()),
                if is_left {
                    tbl_b.left.as_ref()
                } else {
                    tbl_b.inside_v.as_ref()
                },
            );
            let right_spec = resolve_winning_border(
                cb.right.as_ref(),
                cond_b.and_then(|b| b.right.as_ref()),
                if is_right {
                    tbl_b.right.as_ref()
                } else {
                    tbl_b.inside_v.as_ref()
                },
            );

            if let Some(b) = top_spec {
                emit_border_line(
                    &mut page.page.items,
                    b,
                    Point::new(cell_rect.x, cell_rect.y),
                    Point::new(cell_rect.right(), cell_rect.y),
                );
            }
            if let Some(b) = bottom_spec {
                emit_border_line(
                    &mut page.page.items,
                    b,
                    Point::new(cell_rect.x, cell_rect.bottom()),
                    Point::new(cell_rect.right(), cell_rect.bottom()),
                );
            }
            if let Some(b) = left_spec {
                emit_border_line(
                    &mut page.page.items,
                    b,
                    Point::new(cell_rect.x, cell_rect.y),
                    Point::new(cell_rect.x, cell_rect.bottom()),
                );
            }
            if let Some(b) = right_spec {
                emit_border_line(
                    &mut page.page.items,
                    b,
                    Point::new(cell_rect.right(), cell_rect.y),
                    Point::new(cell_rect.right(), cell_rect.bottom()),
                );
            }

            x_cursor += m_cell.width;
        }
    }

    // =========================================================================
    // M3 — Headers & Footers with Section Inheritance
    // =========================================================================

    fn resolve_header_or_footer_id(
        &self,
        sec_idx: usize,
        is_header: bool,
        kind: HeaderFooterKind,
    ) -> Option<String> {
        for idx in (0..=sec_idx).rev() {
            if let Some(sec) = self.doc.sections.get(idx) {
                let map = if is_header {
                    &sec.props.headers
                } else {
                    &sec.props.footers
                };
                if let Some(id) = map.get(&kind) {
                    return Some(id.clone());
                }
            }
        }
        None
    }

    fn layout_page_header_and_footer(
        &mut self,
        sec_idx: usize,
        sec_props: &SectionProperties,
    ) -> Result<()> {
        let Some(ref page) = self.current_page else {
            return Ok(());
        };
        let page_num = self.finished_pages.len() + 1;
        let is_first = page.section_page_offset == 0;
        let is_even = page_num.is_multiple_of(2);

        let hf_kind = if is_first && sec_props.title_pg {
            HeaderFooterKind::First
        } else if is_even && self.doc.settings.even_and_odd_headers {
            HeaderFooterKind::Even
        } else {
            HeaderFooterKind::Default
        };

        let header_id = self
            .resolve_header_or_footer_id(sec_idx, true, hf_kind)
            .or_else(|| {
                if hf_kind != HeaderFooterKind::Default && !is_first {
                    self.resolve_header_or_footer_id(sec_idx, true, HeaderFooterKind::Default)
                } else {
                    None
                }
            });

        let footer_id = self
            .resolve_header_or_footer_id(sec_idx, false, hf_kind)
            .or_else(|| {
                if hf_kind != HeaderFooterKind::Default && !is_first {
                    self.resolve_header_or_footer_id(sec_idx, false, HeaderFooterKind::Default)
                } else {
                    None
                }
            });

        let sec_pages = self
            .known_section_pages
            .get(sec_idx)
            .copied()
            .unwrap_or(1)
            .max(1);

        if let Some(hid) = header_id
            && let Some(story) = self.doc.headers.get(&hid).cloned()
        {
            let (body_left, body_right) = self
                .current_page
                .as_ref()
                .map_or((72.0, 540.0), |p| (p.body_left, p.body_right));
            let w = (body_right - body_left).max(36.0);
            let top_y = sec_props.header_margin_pt.max(18.0);
            let (hdr_items, hdr_h) =
                self.render_story_blocks(&story.blocks, body_left, top_y, w, page_num, sec_pages)?;
            if let Some(ref mut active) = self.current_page {
                active.page.items.extend(hdr_items);
                if top_y + hdr_h + 6.0 > active.body_top {
                    active.body_top = (top_y + hdr_h + 6.0).min(active.body_bottom - 72.0);
                }
            }
        }

        if let Some(fid) = footer_id
            && let Some(story) = self.doc.footers.get(&fid).cloned()
        {
            let (body_left, body_right, page_h) = self
                .current_page
                .as_ref()
                .map_or((72.0, 540.0, 792.0), |p| {
                    (p.body_left, p.body_right, p.page.size.height)
                });
            let w = (body_right - body_left).max(36.0);
            let (_, ftr_h) =
                self.render_story_blocks(&story.blocks, body_left, 0.0, w, page_num, sec_pages)?;
            let ftr_y = (page_h - sec_props.footer_margin_pt.max(18.0) - ftr_h).max(36.0);
            let (ftr_items, _) =
                self.render_story_blocks(&story.blocks, body_left, ftr_y, w, page_num, sec_pages)?;
            if let Some(ref mut active) = self.current_page {
                active.page.items.extend(ftr_items);
                if ftr_y - 6.0 < active.body_bottom {
                    active.body_bottom = (ftr_y - 6.0).max(active.body_top + 72.0);
                }
            }
        }

        Ok(())
    }

    fn render_story_blocks(
        &mut self,
        blocks: &[WordBlock],
        origin_x: f32,
        origin_y: f32,
        width: f32,
        page_num: usize,
        sec_pages: usize,
    ) -> Result<(Vec<Item>, f32)> {
        let mut items = Vec::new();
        let mut y = origin_y;
        #[allow(clippy::cast_possible_truncation)]
        let page_idx = self.finished_pages.len() as u32;

        for block in blocks {
            if let WordBlock::Paragraph(para) = block {
                self.block_counter = self.block_counter.wrapping_add(1);
                let p_style = self.doc.styles.resolve_paragraph_style(
                    para.props.style_id.as_deref(),
                    Some(&self.doc.numbering),
                    &para.props,
                );
                let extracted = self.extract_paragraph_items(
                    para, &p_style, origin_x, width, page_num, sec_pages,
                )?;
                let spec = ParagraphSpec {
                    spans: extracted.spans,
                    align: map_alignment(p_style.alignment),
                    line_spacing: map_line_spacing(p_style.line_spacing),
                    max_width: Some(width),
                    first_line_indent: p_style.indent_first_line_pt,
                    origin: Point::new(origin_x, y),
                    page_index: page_idx,
                    block_index: self.block_counter,
                };
                let shaped = self.text_engine.layout_paragraph(&spec, &mut self.font_sys);
                let h = if shaped.lines.is_empty() {
                    0.0
                } else {
                    shaped.height
                };
                for run in shaped.runs {
                    items.push(Item::Glyphs(run));
                }
                y += h;
            }
        }

        Ok((items, (y - origin_y).max(0.0)))
    }

    // =========================================================================
    // M4 — Footnotes & Endnotes
    // =========================================================================

    fn register_footnote_on_page(&mut self, fn_id: i32, sec_props: &SectionProperties) {
        let Some(story) = self.doc.footnotes.get(&fn_id).cloned() else {
            return;
        };
        let already_present = self
            .current_page
            .as_ref()
            .is_some_and(|p| p.footnote_ids.contains(&fn_id));
        if already_present {
            return;
        }
        let (_, col_w) = self.column_geometry(sec_props, 0);
        let est_h = self
            .render_story_blocks(&story.blocks, 0.0, 0.0, col_w, 1, 1)
            .map_or(14.0, |(_, h)| h.max(12.0));
        if let Some(ref mut page) = self.current_page {
            page.footnote_ids.push(fn_id);
            page.footnote_height_pt += est_h + 2.0;
        }
    }

    fn layout_endnotes(&mut self, sec_idx: usize, sec_props: &SectionProperties) -> Result<()> {
        let ids = self.referenced_endnotes.clone();
        if ids.is_empty() {
            return Ok(());
        }

        let (col_x, col_w) = self.column_geometry(sec_props, self.current_col);
        if self.cursor_y + 36.0 > self.effective_column_bottom() {
            self.advance_column_or_page(sec_idx, sec_props)?;
        }

        let sep_y = self.cursor_y + 8.0;
        if let Some(ref mut page) = self.current_page {
            page.page.items.push(Item::Path(PathItem {
                commands: vec![
                    PathCommand::MoveTo(Point::new(col_x, sep_y)),
                    PathCommand::LineTo(Point::new(col_x + col_w * 0.35, sep_y)),
                ],
                fill: None,
                stroke: Some(StrokeStyle {
                    color: Rgba::rgb(0x66, 0x66, 0x66),
                    width: 0.75,
                    line_cap: LineCap::Butt,
                    line_join: LineJoin::Miter,
                    dash_array: Vec::new(),
                    dash_offset: 0.0,
                }),
                fill_rule: FillRule::NonZero,
                transform: Affine::IDENTITY,
            }));
        }
        self.cursor_y = sep_y + 6.0;

        for en_id in ids {
            let Some(story) = self.doc.endnotes.get(&en_id).cloned() else {
                continue;
            };
            let display_num = self
                .endnote_display_numbers
                .get(&en_id)
                .copied()
                .unwrap_or(1);
            let roman = format_number(
                i32::try_from(display_num).unwrap_or(1),
                &NumberFormat::LowerRoman,
            );
            let prefix_para = WordParagraph {
                props: ParagraphProperties::default(),
                content: vec![ParagraphContent::Run(WordRun {
                    props: RunProperties {
                        vert_align: Some(VerticalRunAlign::Superscript),
                        ..RunProperties::default()
                    },
                    items: vec![RunItem::Text(format!("{roman} "))],
                    revision: None,
                })],
            };
            self.layout_paragraph_block(&prefix_para, None, sec_idx, sec_props)?;
            self.layout_block_slice(&story.blocks, sec_idx, sec_props, 1)?;
        }

        Ok(())
    }

    // =========================================================================
    // M5 — Floating Objects, Exclusion Zones, Text Boxes & WMF/EMF
    // =========================================================================

    fn place_floating_drawing(
        &mut self,
        drawing: &WordDrawing,
        col_x: f32,
        _col_w: f32,
        para_y: f32,
        sec_props: &SectionProperties,
    ) -> Result<()> {
        let Some(ref anchor) = drawing.anchor else {
            return Ok(());
        };

        let (page_w, page_h, body_left, body_top, sec_idx) =
            self.current_page
                .as_ref()
                .map_or((612.0, 792.0, 72.0, 72.0, 0), |p| {
                    (
                        p.page.size.width,
                        p.page.size.height,
                        p.body_left,
                        p.body_top,
                        p.section_index,
                    )
                });

        let ref_x = match anchor.h_relative_from.as_str() {
            "page" => 0.0,
            "margin" => body_left,
            _ => col_x,
        };
        let x = ref_x + anchor.pos_x_pt;

        let ref_y = match anchor.v_relative_from.as_str() {
            "page" => 0.0,
            "margin" => body_top,
            _ => para_y,
        };
        let y = ref_y + anchor.pos_y_pt;

        let dest = Rect::new(
            x.clamp(0.0, (page_w - drawing.width_pt).max(0.0)),
            y.clamp(0.0, (page_h - drawing.height_pt).max(0.0)),
            drawing.width_pt.max(1.0),
            drawing.height_pt.max(1.0),
        );

        let rendered_items = self.build_drawing_items(drawing, dest, sec_idx, sec_props)?;
        if let Some(ref mut page) = self.current_page {
            if !matches!(drawing.wrap, WrapMode::None | WrapMode::Inline) {
                page.exclusions.push(ExclusionZone {
                    rect: dest,
                    wrap_mode: drawing.wrap.clone(),
                });
            }
            if anchor.behind_doc {
                page.background_items.extend(rendered_items);
            } else {
                page.foreground_items.extend(rendered_items);
            }
        }

        Ok(())
    }

    fn advance_past_top_bottom_exclusions(&mut self) {
        let Some(ref page) = self.current_page else {
            return;
        };
        for zone in &page.exclusions {
            if zone.wrap_mode == WrapMode::TopAndBottom
                && self.cursor_y + 12.0 > zone.rect.y
                && self.cursor_y < zone.rect.bottom()
            {
                self.cursor_y = zone.rect.bottom() + 4.0;
            }
        }
    }

    fn adjust_horizontal_for_exclusions(&self, x: f32, w: f32, y: f32, line_h: f32) -> (f32, f32) {
        let Some(ref page) = self.current_page else {
            return (x, w);
        };
        adjust_band_for_exclusions(&page.exclusions, x, w, y, line_h)
    }

    fn render_drawing_into_rect(
        &mut self,
        drawing: &WordDrawing,
        dest: Rect,
        sec_idx: usize,
        sec_props: &SectionProperties,
    ) -> Result<()> {
        let items = self.build_drawing_items(drawing, dest, sec_idx, sec_props)?;
        if let Some(ref mut page) = self.current_page {
            page.page.items.extend(items);
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn build_drawing_items(
        &mut self,
        drawing: &WordDrawing,
        dest: Rect,
        _sec_idx: usize,
        _sec_props: &SectionProperties,
    ) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        match &drawing.payload {
            DrawingPayload::Image { resource_id, .. } => {
                if let Some(raw_bytes) = self.raw_media.get(resource_id)
                    && is_metafile(raw_bytes)
                {
                    let mf_items =
                        render_metafile(raw_bytes, dest, &mut self.font_sys, &mut self.text_engine);
                    items.extend(mf_items);
                } else if let Some(&(img_id, _, _)) = self.image_map.get(resource_id) {
                    items.push(Item::Image(ImageItem {
                        image: img_id,
                        rect: dest,
                        transform: Affine::IDENTITY,
                        opacity: 1.0,
                        alt_text: drawing.alt_text.clone(),
                    }));
                }
            }
            DrawingPayload::TextBox {
                blocks,
                fill_color,
                stroke_color,
                stroke_width_pt,
            } => {
                let fill = fill_color
                    .as_deref()
                    .and_then(parse_hex_color)
                    .map(FillPaint::Solid);
                let stroke =
                    stroke_color
                        .as_deref()
                        .and_then(parse_hex_color)
                        .map(|c| StrokeStyle {
                            color: c,
                            width: stroke_width_pt.max(0.5),
                            line_cap: LineCap::Butt,
                            line_join: LineJoin::Miter,
                            dash_array: Vec::new(),
                            dash_offset: 0.0,
                        });
                if fill.is_some() || stroke.is_some() {
                    items.push(Item::Path(PathItem {
                        commands: rect_path_commands(dest),
                        fill,
                        stroke,
                        fill_rule: FillRule::NonZero,
                        transform: Affine::IDENTITY,
                    }));
                }

                let inner_x = dest.x + 7.2;
                let inner_y = dest.y + 3.6;
                let inner_w = (dest.width - 14.4).max(12.0);
                let page_num = self.finished_pages.len() + 1;
                let (tb_items, _) =
                    self.render_story_blocks(blocks, inner_x, inner_y, inner_w, page_num, 1)?;
                items.extend(tb_items);
            }
            DrawingPayload::Chart(chart) => {
                let dml_theme = prism_drawingml::DrawingTheme::default();
                let chart_items = prism_drawingml::render_chart(
                    chart,
                    dest,
                    &dml_theme,
                    &mut self.font_sys,
                    &mut self.text_engine,
                );
                items.extend(chart_items);
            }
            DrawingPayload::Placeholder { label } => {
                items.push(Item::Path(PathItem {
                    commands: rect_path_commands(dest),
                    fill: Some(FillPaint::Solid(Rgba::rgb(0xF3, 0xF3, 0xF3))),
                    stroke: Some(StrokeStyle {
                        color: Rgba::rgb(0x99, 0x99, 0x99),
                        width: 0.75,
                        line_cap: LineCap::Butt,
                        line_join: LineJoin::Miter,
                        dash_array: Vec::new(),
                        dash_offset: 0.0,
                    }),
                    fill_rule: FillRule::NonZero,
                    transform: Affine::IDENTITY,
                }));
                if !label.is_empty() {
                    #[allow(clippy::cast_possible_truncation)]
                    let page_idx = self.finished_pages.len() as u32;
                    let spec = ParagraphSpec {
                        spans: vec![SpanSpec {
                            text: label.clone(),
                            family: "Calibri".to_string(),
                            size: 9.0,
                            color: Rgba::rgb(0x55, 0x55, 0x55),
                            ..SpanSpec::default()
                        }],
                        align: TextAlign::Center,
                        max_width: Some(dest.width),
                        origin: Point::new(dest.x, dest.y + dest.height * 0.4),
                        page_index: page_idx,
                        block_index: 99_999,
                        ..ParagraphSpec::default()
                    };
                    let shaped = self.text_engine.layout_paragraph(&spec, &mut self.font_sys);
                    for run in shaped.runs {
                        items.push(Item::Glyphs(run));
                    }
                }
            }
        }
        Ok(items)
    }

    // =========================================================================
    // Finalization (Column Separators, Footnote Rendering, Comment Balloons)
    // =========================================================================

    #[allow(clippy::too_many_lines)]
    fn finish(mut self) -> Result<PassOutput> {
        if let Some(last) = self.current_page.take() {
            self.finished_pages.push(last);
        }

        let mut section_page_counts = vec![0usize; self.doc.sections.len().max(1)];
        let mut out_pages = Vec::with_capacity(self.finished_pages.len());

        let mut finished = std::mem::take(&mut self.finished_pages);
        for (page_idx, mut active) in finished.drain(..).enumerate() {
            if let Some(cnt) = section_page_counts.get_mut(active.section_index) {
                *cnt += 1;
            }
            if let Some(ref r) = self.page_range
                && (page_idx < r.start || page_idx >= r.end)
            {
                continue;
            }
            let sec_props = self
                .doc
                .sections
                .get(active.section_index)
                .map(|s| s.props.clone())
                .unwrap_or_default();

            // 1. Multi-column vertical separator line (`w:cols w:sep="1"`)
            let col_count = usize::from(sec_props.column_count.max(1));
            if col_count > 1 && sec_props.column_separator {
                let avail_w = (active.body_right - active.body_left).max(36.0);
                let space = sec_props.column_space_pt.max(0.0);
                #[allow(clippy::cast_precision_loss)]
                let col_w = ((avail_w - space * (col_count.saturating_sub(1) as f32))
                    / (col_count as f32))
                    .max(18.0);
                for c in 0..(col_count - 1) {
                    #[allow(clippy::cast_precision_loss)]
                    let sep_x =
                        active.body_left + (c as f32 + 1.0) * col_w + (c as f32 + 0.5) * space;
                    active.page.items.push(Item::Path(PathItem {
                        commands: vec![
                            PathCommand::MoveTo(Point::new(sep_x, active.body_top)),
                            PathCommand::LineTo(Point::new(sep_x, active.body_bottom)),
                        ],
                        fill: None,
                        stroke: Some(StrokeStyle {
                            color: Rgba::rgb(0x99, 0x99, 0x99),
                            width: 0.5,
                            line_cap: LineCap::Butt,
                            line_join: LineJoin::Miter,
                            dash_array: Vec::new(),
                            dash_offset: 0.0,
                        }),
                        fill_rule: FillRule::NonZero,
                        transform: Affine::IDENTITY,
                    }));
                }
            }

            // 2. Render same-page footnotes at the bottom of the page
            if !active.footnote_ids.is_empty() {
                let fn_top =
                    (active.body_bottom - active.footnote_height_pt).max(active.body_top + 36.0);
                let sep_y = fn_top - 4.0;
                active.page.items.push(Item::Path(PathItem {
                    commands: vec![
                        PathCommand::MoveTo(Point::new(active.body_left, sep_y)),
                        PathCommand::LineTo(Point::new(
                            active.body_left + (active.body_right - active.body_left) * 0.35,
                            sep_y,
                        )),
                    ],
                    fill: None,
                    stroke: Some(StrokeStyle {
                        color: Rgba::rgb(0x66, 0x66, 0x66),
                        width: 0.75,
                        line_cap: LineCap::Butt,
                        line_join: LineJoin::Miter,
                        dash_array: Vec::new(),
                        dash_offset: 0.0,
                    }),
                    fill_rule: FillRule::NonZero,
                    transform: Affine::IDENTITY,
                }));

                let mut fn_y = fn_top;
                let fn_w = (active.body_right - active.body_left).max(36.0);
                for &fn_id in &active.footnote_ids {
                    if let Some(story) = self.doc.footnotes.get(&fn_id) {
                        let num = self
                            .footnote_display_numbers
                            .get(&fn_id)
                            .copied()
                            .unwrap_or(1);
                        let fn_text = extract_story_plain_text(&story.blocks);
                        #[allow(clippy::cast_possible_truncation)]
                        let p_idx = page_idx as u32;
                        #[allow(clippy::cast_sign_loss)]
                        let b_idx = 90_000u32.wrapping_add(fn_id as u32);
                        let spec = ParagraphSpec {
                            spans: vec![
                                SpanSpec {
                                    text: format!("{num} "),
                                    family: "Calibri".to_string(),
                                    size: 9.0,
                                    script: ScriptPosition::Superscript,
                                    run_index: 0,
                                    ..SpanSpec::default()
                                },
                                SpanSpec {
                                    text: fn_text,
                                    family: "Calibri".to_string(),
                                    size: 9.0,
                                    run_index: 1,
                                    ..SpanSpec::default()
                                },
                            ],
                            align: TextAlign::Left,
                            line_spacing: LineSpacing::Multiple(1.1),
                            max_width: Some(fn_w),
                            first_line_indent: 0.0,
                            origin: Point::new(active.body_left, fn_y),
                            page_index: p_idx,
                            block_index: b_idx,
                        };
                        let shaped = self.text_engine.layout_paragraph(&spec, &mut self.font_sys);
                        for run in shaped.runs {
                            active.page.items.push(Item::Glyphs(run));
                        }
                        fn_y += shaped.height.max(11.0) + 2.0;
                    }
                }
            }

            // 3. Render right-margin comment balloons (M6)
            if !active.comment_ids.is_empty()
                && matches!(
                    self.options.markup_mode,
                    MarkupMode::AllMarkup | MarkupMode::SimpleMarkup
                )
            {
                let balloon_w = (active.page.size.width - active.body_right - 8.0).max(96.0);
                let balloon_x =
                    (active.page.size.width - balloon_w - 4.0).max(active.body_right - 24.0);
                let mut balloon_y = active.body_top;

                for (c_offset, cid) in active.comment_ids.iter().enumerate() {
                    if let Some(comment) = self.doc.comments.get(cid) {
                        let author = comment.author.as_deref().unwrap_or("Reviewer");
                        let initials = comment.initials.as_deref().unwrap_or("");
                        let header_label = if initials.is_empty() {
                            format!("{author}: ")
                        } else {
                            format!("[{initials}] {author}: ")
                        };
                        let body_text = extract_story_plain_text(&comment.blocks);
                        #[allow(clippy::cast_possible_truncation)]
                        let p_idx = page_idx as u32;
                        #[allow(clippy::cast_possible_truncation)]
                        let b_idx = 95_000 + (c_offset as u32);
                        let spec = ParagraphSpec {
                            spans: vec![
                                SpanSpec {
                                    text: header_label,
                                    family: "Calibri".to_string(),
                                    size: 8.0,
                                    bold: true,
                                    color: author_color(author),
                                    run_index: 0,
                                    ..SpanSpec::default()
                                },
                                SpanSpec {
                                    text: body_text,
                                    family: "Calibri".to_string(),
                                    size: 8.0,
                                    color: Rgba::rgb(0x22, 0x22, 0x22),
                                    run_index: 1,
                                    ..SpanSpec::default()
                                },
                            ],
                            align: TextAlign::Left,
                            line_spacing: LineSpacing::Multiple(1.1),
                            max_width: Some((balloon_w - 8.0).max(36.0)),
                            first_line_indent: 0.0,
                            origin: Point::new(balloon_x + 4.0, balloon_y + 3.0),
                            page_index: p_idx,
                            block_index: b_idx,
                        };
                        let shaped = self.text_engine.layout_paragraph(&spec, &mut self.font_sys);
                        let box_h = (shaped.height + 6.0).max(16.0);
                        let b_rect = Rect::new(balloon_x, balloon_y, balloon_w, box_h);

                        active.foreground_items.push(Item::Path(PathItem {
                            commands: rect_path_commands(b_rect),
                            fill: Some(FillPaint::Solid(Rgba::rgb(0xFF, 0xF9, 0xE6))),
                            stroke: Some(StrokeStyle {
                                color: author_color(author),
                                width: 0.75,
                                line_cap: LineCap::Butt,
                                line_join: LineJoin::Round,
                                dash_array: Vec::new(),
                                dash_offset: 0.0,
                            }),
                            fill_rule: FillRule::NonZero,
                            transform: Affine::IDENTITY,
                        }));
                        for run in shaped.runs {
                            active.foreground_items.push(Item::Glyphs(run));
                        }
                        balloon_y += box_h + 4.0;
                    }
                }
            }

            let mut combined = Vec::with_capacity(
                active.background_items.len()
                    + active.page.items.len()
                    + active.foreground_items.len(),
            );
            combined.extend(active.background_items);
            combined.extend(active.page.items);
            combined.extend(active.foreground_items);
            active.page.items = combined;

            let glyph_count = count_page_glyphs(&active.page.items);
            if glyph_count > self.options.max_glyphs_per_page {
                return Err(Error::InvalidInput(format!(
                    "Page {} glyph count ({}) exceeds max_glyphs_per_page ({})",
                    page_idx, glyph_count, self.options.max_glyphs_per_page
                )));
            }

            out_pages.push(active.page);
        }

        Ok(PassOutput {
            document: LayoutDocument {
                pages: out_pages,
                fonts: self.font_sys.store,
                images: self.images,
                font_report: self.font_sys.report,
            },
            section_page_counts,
        })
    }
}

#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
struct ExtractedParagraphItems {
    spans: Vec<SpanSpec>,
    span_flags: Vec<SpanExtraFlags>,
    span_to_hyperlink: BTreeMap<usize, usize>,
    hyperlink_targets: Vec<String>,
    inline_drawings: Vec<WordDrawing>,
    floating_drawings: Vec<WordDrawing>,
    bookmarks: Vec<String>,
    footnote_refs: Vec<i32>,
    endnote_refs: Vec<i32>,
    comment_ids: Vec<String>,
    has_revision: bool,
    has_tab: bool,
    has_page_break: bool,
    has_column_break: bool,
}

#[derive(Debug, Clone, Copy)]
struct SpanExtraFlags {
    hidden_dotted: bool,
    double_strike: bool,
    underline_kind: UnderlineKind,
}

struct MeasuredTableRow {
    cells: Vec<MeasuredTableCell>,
    height: f32,
}

struct MeasuredTableCell {
    col_index: usize,
    grid_span: usize,
    v_merge: VerticalMerge,
    vert_align: PageVerticalAlign,
    width: f32,
    pad_top: f32,
    pad_bottom: f32,
    pad_left: f32,
    content_text_height: f32,
    content_height: f32,
    shading_fill: Option<String>,
    borders: WordBorders,
    cond_borders: Option<WordBorders>,
    items: Vec<Item>,
}

#[derive(Debug, Default)]
struct ResolvedCellConditional {
    shading_fill: Option<String>,
    borders: Option<WordBorders>,
    run_props: Option<RunProperties>,
}

#[allow(clippy::needless_range_loop)]
fn compute_vmerge_row_spans(rows: &[WordTableRow], num_cols: usize) -> Vec<Vec<usize>> {
    let num_rows = rows.len();
    let mut spans = vec![vec![1usize; num_cols]; num_rows];
    for c in 0..num_cols {
        let mut restart_row: Option<usize> = None;
        for (r, row) in rows.iter().enumerate() {
            let mut col_cur = 0usize;
            let mut cell_state = VerticalMerge::None;
            for cell in &row.cells {
                let g = usize::from(cell.grid_span.max(1));
                if c >= col_cur && c < col_cur + g {
                    cell_state = cell.v_merge;
                    break;
                }
                col_cur += g;
            }
            match cell_state {
                VerticalMerge::Restart => restart_row = Some(r),
                VerticalMerge::Continue => {
                    if let Some(start_r) = restart_row {
                        spans[start_r][c] += 1;
                        spans[r][c] = 0;
                    }
                }
                VerticalMerge::None => restart_row = None,
            }
        }
    }
    spans
}

fn resolve_winning_border<'a>(
    cell_border: Option<&'a WordBorder>,
    cond_border: Option<&'a WordBorder>,
    table_border: Option<&'a WordBorder>,
) -> Option<&'a WordBorder> {
    let candidates = [cell_border, cond_border, table_border];
    let mut best: Option<&'a WordBorder> = None;
    for cand in candidates.into_iter().flatten() {
        if cand.style == WordBorderStyle::None || cand.width_pt <= 0.0 {
            continue;
        }
        if let Some(curr) = best {
            if compare_border_specs(cand, curr).is_gt() {
                best = Some(cand);
            }
        } else {
            best = Some(cand);
        }
    }
    best
}

fn compare_border_specs(a: &WordBorder, b: &WordBorder) -> std::cmp::Ordering {
    a.width_pt
        .partial_cmp(&b.width_pt)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.style.cmp(&b.style))
}

fn emit_border_line(items: &mut Vec<Item>, spec: &WordBorder, p1: Point, p2: Point) {
    if spec.style == WordBorderStyle::None || spec.width_pt <= 0.0 {
        return;
    }
    let color = parse_hex_color(&spec.color).unwrap_or(Rgba::BLACK);
    let width = spec.width_pt.clamp(0.5, 8.0);
    let dash_array = match spec.style {
        WordBorderStyle::Dotted => vec![width, width * 2.0],
        WordBorderStyle::Dashed => vec![width * 3.0, width * 2.0],
        _ => Vec::new(),
    };
    items.push(Item::Path(PathItem {
        commands: vec![PathCommand::MoveTo(p1), PathCommand::LineTo(p2)],
        fill: None,
        stroke: Some(StrokeStyle {
            color,
            width,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            dash_array,
            dash_offset: 0.0,
        }),
        fill_rule: FillRule::NonZero,
        transform: Affine::IDENTITY,
    }));
}

fn build_extra_run_decorations(run: &GlyphRun, flags: Option<&SpanExtraFlags>) -> Vec<Item> {
    let Some(flags) = flags else {
        return Vec::new();
    };
    let mut items = Vec::new();
    let run_w: f32 = run.glyphs.iter().map(|g| g.advance).sum();
    if run_w <= 0.0 {
        return items;
    }
    let x1 = run.transform.coeffs[4];
    let x2 = x1 + run_w;
    let baseline_y = run.transform.coeffs[5];

    if flags.hidden_dotted
        || matches!(
            flags.underline_kind,
            UnderlineKind::Dotted | UnderlineKind::Dashed
        )
    {
        let y = baseline_y + run.size * 0.14;
        items.push(Item::Path(PathItem {
            commands: vec![
                PathCommand::MoveTo(Point::new(x1, y)),
                PathCommand::LineTo(Point::new(x2, y)),
            ],
            fill: None,
            stroke: Some(StrokeStyle {
                color: run.color,
                width: (run.size * 0.06).max(0.5),
                line_cap: LineCap::Round,
                line_join: LineJoin::Miter,
                dash_array: vec![1.0, 2.0],
                dash_offset: 0.0,
            }),
            fill_rule: FillRule::NonZero,
            transform: Affine::IDENTITY,
        }));
    }

    if flags.double_strike {
        let y = baseline_y - run.size * 0.22;
        items.push(Item::Path(PathItem {
            commands: vec![
                PathCommand::MoveTo(Point::new(x1, y)),
                PathCommand::LineTo(Point::new(x2, y)),
            ],
            fill: None,
            stroke: Some(StrokeStyle {
                color: run.color,
                width: (run.size * 0.06).max(0.5),
                line_cap: LineCap::Butt,
                line_join: LineJoin::Miter,
                dash_array: Vec::new(),
                dash_offset: 0.0,
            }),
            fill_rule: FillRule::NonZero,
            transform: Affine::IDENTITY,
        }));
    }

    items
}

fn count_page_glyphs(items: &[Item]) -> usize {
    let mut total = 0usize;
    for item in items {
        match item {
            Item::Glyphs(g) => total = total.saturating_add(g.glyphs.len()),
            Item::Group(grp) => total = total.saturating_add(count_page_glyphs(&grp.children)),
            _ => {}
        }
    }
    total
}

fn translate_item(item: Item, dx: f32, dy: f32) -> Item {
    match item {
        Item::Glyphs(mut g) => {
            g.transform.coeffs[4] = normalize_zero(g.transform.coeffs[4] + dx);
            g.transform.coeffs[5] = normalize_zero(g.transform.coeffs[5] + dy);
            Item::Glyphs(g)
        }
        Item::Path(mut p) => {
            p.transform.coeffs[4] = normalize_zero(p.transform.coeffs[4] + dx);
            p.transform.coeffs[5] = normalize_zero(p.transform.coeffs[5] + dy);
            Item::Path(p)
        }
        Item::Image(mut img) => {
            img.rect.x = normalize_zero(img.rect.x + dx);
            img.rect.y = normalize_zero(img.rect.y + dy);
            Item::Image(img)
        }
        Item::Group(mut grp) => {
            grp.transform.coeffs[4] = normalize_zero(grp.transform.coeffs[4] + dx);
            grp.transform.coeffs[5] = normalize_zero(grp.transform.coeffs[5] + dy);
            Item::Group(grp)
        }
        Item::Link(mut lnk) => {
            lnk.rect.x = normalize_zero(lnk.rect.x + dx);
            lnk.rect.y = normalize_zero(lnk.rect.y + dy);
            Item::Link(lnk)
        }
        Item::Anchor(mut anc) => {
            anc.rect.x = normalize_zero(anc.rect.x + dx);
            anc.rect.y = normalize_zero(anc.rect.y + dy);
            Item::Anchor(anc)
        }
    }
}

fn rect_path_commands(r: Rect) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(Point::new(r.x, r.y)),
        PathCommand::LineTo(Point::new(r.right(), r.y)),
        PathCommand::LineTo(Point::new(r.right(), r.bottom())),
        PathCommand::LineTo(Point::new(r.x, r.bottom())),
        PathCommand::Close,
    ]
}

fn next_tab_stop_x(tabs: &[TabStop], cur_x: f32, default_step: f32) -> f32 {
    for stop in tabs {
        if stop.kind != TabStopKind::Clear
            && stop.kind != TabStopKind::Bar
            && stop.position_pt > cur_x + 1.0
        {
            return stop.position_pt;
        }
    }
    let step = default_step.max(18.0);
    ((cur_x / step).floor() + 1.0) * step
}

fn estimate_cell_min_width(cell: &WordTableCell) -> f32 {
    let mut longest_word = 0usize;
    for block in &cell.blocks {
        if let WordBlock::Paragraph(p) = block {
            for r in p.runs() {
                for item in &r.items {
                    if let RunItem::Text(t) = item {
                        for w in t.split_whitespace() {
                            longest_word = longest_word.max(w.chars().count());
                        }
                    }
                }
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let est = (longest_word.min(20) as f32) * 5.5 + 12.0;
    est.max(24.0)
}

fn extract_story_plain_text(blocks: &[WordBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        if let WordBlock::Paragraph(p) = block {
            for r in p.runs() {
                for item in &r.items {
                    if let RunItem::Text(t) = item {
                        out.push_str(t);
                    }
                }
            }
        }
    }
    out
}

fn pick_font_family(text: &str, r_style: &ResolvedRunStyle) -> String {
    for ch in text.chars() {
        let code = u32::from(ch);
        if (0x0590..=0x08FF).contains(&code)
            && let Some(ref cs) = r_style.cs_font
        {
            return cs.clone();
        }
        if ((0x3000..=0x9FFF).contains(&code) || (0xAC00..=0xD7AF).contains(&code))
            && let Some(ref ea) = r_style.east_asia_font
        {
            return ea.clone();
        }
    }
    r_style.font_family.clone()
}

fn parse_hex_color(hex: &str) -> Option<Rgba> {
    let clean = hex.trim().trim_start_matches('#');
    // `get` rather than indexing: the value comes from the document and may not be ASCII.
    let r = u8::from_str_radix(clean.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(clean.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(clean.get(4..6)?, 16).ok()?;
    Some(Rgba::rgb(r, g, b))
}

fn author_color(author: &str) -> Rgba {
    let hash = author.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(usize::from(b))
    });
    AUTHOR_PALETTE[hash % AUTHOR_PALETTE.len()]
}

fn map_alignment(align: WordAlignment) -> TextAlign {
    match align {
        WordAlignment::Left => TextAlign::Left,
        WordAlignment::Center => TextAlign::Center,
        WordAlignment::Right => TextAlign::Right,
        WordAlignment::Justify | WordAlignment::Distribute => TextAlign::Justify,
    }
}

fn map_line_spacing(ls: WordLineSpacing) -> LineSpacing {
    match ls {
        WordLineSpacing::Auto(m) => LineSpacing::Multiple(m),
        WordLineSpacing::ExactPt(pt) => LineSpacing::ExactPt(pt),
        WordLineSpacing::AtLeastPt(pt) => LineSpacing::AtLeastPt(pt),
    }
}

fn probe_image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() >= 24 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        if w > 0 && h > 0 {
            return Some((w, h));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToggleProp, WordComment, WordRevisionMeta, WordStory};

    #[test]
    fn non_ascii_hex_color_is_rejected() {
        assert_eq!(parse_hex_color("\u{20ac}\u{20ac}"), None);
        assert_eq!(
            parse_hex_color("#1F4E79"),
            Some(Rgba::rgb(0x1F, 0x4E, 0x79))
        );
    }

    #[test]
    fn lays_out_sections_footnotes_tables_and_markup_modes() {
        let mut doc = WordDocument::default();
        doc.footnotes.insert(
            1,
            WordStory {
                id: "1".to_string(),
                blocks: vec![WordBlock::Paragraph(WordParagraph {
                    props: ParagraphProperties::default(),
                    content: vec![ParagraphContent::Run(WordRun {
                        props: RunProperties::default(),
                        items: vec![RunItem::Text("Footnote body text".to_string())],
                        revision: None,
                    })],
                })],
            },
        );
        doc.comments.insert(
            "1".to_string(),
            WordComment {
                id: "1".to_string(),
                author: Some("Alice".to_string()),
                initials: Some("A".to_string()),
                date: None,
                parent_id: None,
                done: false,
                blocks: vec![WordBlock::Paragraph(WordParagraph {
                    props: ParagraphProperties::default(),
                    content: vec![ParagraphContent::Run(WordRun {
                        props: RunProperties::default(),
                        items: vec![RunItem::Text("Please verify this clause".to_string())],
                        revision: None,
                    })],
                })],
            },
        );

        doc.sections.push(WordSection {
            props: SectionProperties {
                column_count: 2,
                column_separator: true,
                ..SectionProperties::default()
            },
            blocks: vec![WordBlock::Paragraph(WordParagraph {
                props: ParagraphProperties::default(),
                content: vec![
                    ParagraphContent::CommentRangeStart("1".to_string()),
                    ParagraphContent::Run(WordRun {
                        props: RunProperties {
                            small_caps: Some(ToggleProp::Toggle),
                            ..RunProperties::default()
                        },
                        items: vec![
                            RunItem::Text("Hello World".to_string()),
                            RunItem::FootnoteRef(1),
                        ],
                        revision: None,
                    }),
                    ParagraphContent::CommentRangeEnd("1".to_string()),
                    ParagraphContent::Run(WordRun {
                        props: RunProperties::default(),
                        items: vec![RunItem::Text(" DeletedText".to_string())],
                        revision: Some(WordRevisionMeta {
                            id: "10".to_string(),
                            kind: RevisionKind::Deleted,
                            author: Some("Bob".to_string()),
                            date: None,
                        }),
                    }),
                    ParagraphContent::Field(WordField {
                        instruction: "NUMPAGES".to_string(),
                        kind: FieldKind::NumPages,
                        cached_result: vec![ParagraphContent::Run(WordRun {
                            props: RunProperties::default(),
                            items: vec![RunItem::Text("99".to_string())],
                            revision: None,
                        })],
                        locked: false,
                        dirty: false,
                    }),
                ],
            })],
        });

        let opts_all = LayoutOptions {
            markup_mode: MarkupMode::AllMarkup,
            ..LayoutOptions::default()
        };
        let layout_all = layout_word_document(&doc, &opts_all).unwrap();
        assert_eq!(layout_all.pages.len(), 1);
        let text_all = layout_all.pages[0].text();
        assert!(text_all.contains("HELLO WORLD"));
        assert!(text_all.contains("DeletedText"));
        assert!(text_all.contains("Footnote body text"));
        assert!(text_all.contains("Please verify this clause"));

        let opts_no = LayoutOptions {
            markup_mode: MarkupMode::NoMarkup,
            ..LayoutOptions::default()
        };
        let layout_no = layout_word_document(&doc, &opts_no).unwrap();
        let text_no = layout_no.pages[0].text();
        assert!(text_no.contains("HELLO WORLD"));
        assert!(!text_no.contains("DeletedText"));
        assert!(!text_no.contains("Please verify this clause"));
    }
}
