// SPDX-License-Identifier: AGPL-3.0-only
//! Conversion from the Unified Document Model ([`prism_core::document::Document`]) to [`LayoutDocument`].
//!
//! Handles both:
//! - **Fixed-layout documents** (PPTX slides, XPS pages, positioned shapes/vectors/tables/text boxes), and
//! - **Flow-layout documents** (DOCX, ODT, TXT, RTF, CSV, etc.) with real line breaking, paragraph
//!   indents/spacing, simple tables, and multi-page pagination inside [`PageMargins`].

use crate::engine::{
    LineSpacing, ParagraphSpec, ScriptPosition, SpanSpec, TextAlign, TextLayoutEngine,
};
use crate::fonts::FontSystem;
use crate::model::{
    Affine, AnchorItem, FillPaint, FillRule, GlyphRun, GradientStop, GroupItem, ImageId, ImageItem,
    ImageStore, Item, LayoutDocument, LayoutOptions, LayoutPage, LinearGradient, PathCommand,
    PathItem, Point, Rect, Rgba, Size, StrokeStyle, normalize_zero,
};
use prism_core::document::{
    AnnotationType, CellBorders, ContainerBlock, ContentBlock, Document, ImageBlock, Page,
    PageMargins, ParagraphStyle, ShapeStyle, TableBlock, TextAlignment, TextBlock, TextRun,
    VectorBlock, VerticalAlignment,
};
use prism_core::error::{Error, Result};
use std::collections::BTreeMap;

/// Default `PowerPoint` text box horizontal inset in points (`0.1 in = 7.2 pt`).
const TEXTBOX_INSET_X: f32 = 7.2;
/// Default `PowerPoint` text box vertical inset in points (`0.05 in = 3.6 pt`).
const TEXTBOX_INSET_Y: f32 = 3.6;
/// Default table cell horizontal padding in points.
const TABLE_CELL_PAD_X: f32 = 5.0;
/// Default table cell vertical padding in points.
const TABLE_CELL_PAD_Y: f32 = 4.0;

struct UdmLayoutContext<'a> {
    doc: &'a Document,
    options: &'a LayoutOptions,
    font_sys: FontSystem,
    text_engine: TextLayoutEngine,
    images: ImageStore,
    image_id_map: BTreeMap<String, ImageId>,
    named_para_styles: BTreeMap<String, &'a ParagraphStyle>,
    page_range: Option<std::ops::Range<usize>>,
}

/// Lay out a UDM [`Document`] into a positioned [`LayoutDocument`].
///
/// Automatically selects fixed-page layout (for PPTX slides, XPS, etc.) or paginated
/// flow layout (for DOCX, TXT, RTF, ODT, etc.).
///
/// # Errors
///
/// Returns an [`Error`] if resource limits (`max_pages`, `max_glyphs_per_page`, `max_image_pixels`,
/// `max_recursion_depth`) are exceeded.
pub fn layout_document(doc: &Document, options: &LayoutOptions) -> Result<LayoutDocument> {
    layout_document_inner(doc, options, None)
}

fn layout_document_inner(
    doc: &Document,
    options: &LayoutOptions,
    page_range: Option<std::ops::Range<usize>>,
) -> Result<LayoutDocument> {
    // If the document is a PDF (single EmbeddedBlock::pdf), interpret it directly via hayro
    for page in &doc.pages {
        for block in &page.content {
            if let ContentBlock::Embedded(eb) = block
                && eb.kind == prism_core::document::EmbeddedKind::Pdf
            {
                // `interpret_pdf_bytes` enforces `max_pages`. On failure the UDM holds nothing
                // else to lay out, so a fallback would be a blank page; report the error instead
                // and let the caller use the HTML path.
                return crate::pdf_input::interpret_pdf_bytes(&eb.data, options);
            }
        }
    }

    // Embedded markup (HTML, generated HTML, SVG) has no positioned layout yet. Laying out the
    // rest of the document would silently drop that content (an email body, a whole EPUB), so
    // refuse instead and let the caller fall back to `HtmlRenderer`.
    if let Some(kind) = first_embedded_kind(&doc.pages) {
        return Err(Error::UnsupportedFormat(format!(
            "{kind:?} content is not supported by the layout engine yet; render it with HtmlRenderer"
        )));
    }

    let mut font_sys = FontSystem::new(options);
    font_sys.register_document_fonts(&doc.resources.fonts);

    let mut images = ImageStore::new();
    let mut image_id_map = BTreeMap::new();
    for res in &doc.resources.images {
        if let Some(ref data) = res.data {
            let (w, h) = if res.width > 0 && res.height > 0 {
                (res.width, res.height)
            } else {
                (1, 1)
            };
            let id = images.register(res.mime_type.clone(), w, h, data.clone(), options)?;
            image_id_map.insert(res.id.clone(), id);
        }
    }

    let mut named_para_styles = BTreeMap::new();
    for ns in &doc.styles.paragraph_styles {
        named_para_styles.insert(ns.name.clone(), &ns.style);
    }

    let mut ctx = UdmLayoutContext {
        doc,
        options,
        font_sys,
        text_engine: TextLayoutEngine::new(),
        images,
        image_id_map,
        named_para_styles,
        page_range,
    };

    let pages = if is_fixed_layout_document(doc) {
        let mut all_pages = layout_fixed_pages(&mut ctx)?;
        if let Some(ref r) = ctx.page_range {
            all_pages.retain(|p| (p.index as usize) >= r.start && (p.index as usize) < r.end);
        }
        all_pages
    } else {
        layout_flow_pages(&mut ctx)?
    };

    if pages.len() > options.max_pages {
        return Err(Error::InvalidInput(format!(
            "Document page count ({}) exceeds max_pages ({})",
            pages.len(),
            options.max_pages
        )));
    }

    Ok(LayoutDocument {
        pages,
        fonts: ctx.font_sys.store,
        images: ctx.images,
        font_report: ctx.font_sys.report,
    })
}

/// Lay out a UDM [`Document`] into a paginated [`LayoutDocument`], converting any
/// embedded HTML, generated HTML, or SVG blocks (`MSG`, `HTML`, `MHT`, `EPUB`, `MD`, `SVG`)
/// into native flow-layout paragraphs, tables, rules, and inline images.
///
/// Unlike [`layout_document`], which returns [`Error::UnsupportedFormat`] on
/// [`ContentBlock::Embedded`] so [`prism_render::HtmlRenderer`] can fall back to raw HTML,
/// `layout_document_paginated` always produces a positioned [`LayoutDocument`] suitable for
/// page-by-page raster/SVG viewing, text search with character bounding boxes, destructive
/// redaction, and Bates-numbered PDF/TIFF production.
///
/// # Errors
///
/// Returns an [`Error`] if resource limits (`max_pages`, `max_glyphs_per_page`, `max_image_pixels`,
/// `max_recursion_depth`) are exceeded.
pub fn layout_document_paginated(
    doc: &Document,
    options: &LayoutOptions,
) -> Result<LayoutDocument> {
    for page in &doc.pages {
        for block in &page.content {
            if let ContentBlock::Embedded(eb) = block
                && eb.kind == prism_core::document::EmbeddedKind::Pdf
            {
                return crate::pdf_input::interpret_pdf_bytes(&eb.data, options);
            }
        }
    }

    if first_embedded_kind(&doc.pages).is_none() {
        return layout_document_inner(doc, options, None);
    }

    let normalized = expand_embedded_blocks_to_flow(doc);
    layout_document_inner(&normalized, options, None)
}

/// Lay out only the requested 0-based `page_range` (`start_page..end_page`) of a UDM [`Document`].
///
/// Stops flow pagination as soon as `page_range.end` pages have been produced and discards
/// item buffers for pages preceding `page_range.start`.
///
/// # Errors
///
/// Returns an [`Error`] if resource limits in `options` are exceeded.
pub fn layout_document_paginated_range(
    doc: &Document,
    options: &LayoutOptions,
    page_range: std::ops::Range<usize>,
) -> Result<LayoutDocument> {
    if page_range.start >= page_range.end {
        return Ok(LayoutDocument::new());
    }
    for page in &doc.pages {
        for block in &page.content {
            if let ContentBlock::Embedded(eb) = block
                && eb.kind == prism_core::document::EmbeddedKind::Pdf
            {
                let mut pdf_doc = crate::pdf_input::interpret_pdf_bytes(&eb.data, options)?;
                pdf_doc.pages.retain(|p| {
                    (p.index as usize) >= page_range.start && (p.index as usize) < page_range.end
                });
                return Ok(pdf_doc);
            }
        }
    }

    if first_embedded_kind(&doc.pages).is_none() {
        return layout_document_inner(doc, options, Some(page_range));
    }

    let normalized = expand_embedded_blocks_to_flow(doc);
    layout_document_inner(&normalized, options, Some(page_range))
}

/// Kind of the first [`ContentBlock::Embedded`] block in `pages`, including inside containers.
fn first_embedded_kind(
    pages: &[prism_core::document::Page],
) -> Option<prism_core::document::EmbeddedKind> {
    // Explicit stack: container nesting comes from the document and must not overflow ours
    let mut stack: Vec<&ContentBlock> = pages
        .iter()
        .flat_map(|page| page.content.iter())
        .rev()
        .collect();
    while let Some(block) = stack.pop() {
        match block {
            ContentBlock::Embedded(eb) => return Some(eb.kind),
            ContentBlock::Container(container) => stack.extend(container.children.iter().rev()),
            _ => {}
        }
    }
    None
}

fn is_fixed_layout_document(doc: &Document) -> bool {
    if let Some(prism_core::metadata::MetadataValue::String(fmt)) =
        doc.metadata.custom.get("format")
    {
        let upper = fmt.to_ascii_uppercase();
        if matches!(upper.as_str(), "PPTX" | "PPSX" | "POTX" | "ODP" | "XPS") {
            return true;
        }
        if matches!(
            upper.as_str(),
            "DOCX"
                | "ODT"
                | "TXT"
                | "RTF"
                | "CSV"
                | "EML"
                | "MSG"
                | "MBOX"
                | "PST"
                | "OST"
                | "RSMF"
                | "SLACK"
                | "TEAMS"
                | "WHATSAPP"
                | "HTML"
                | "MHT"
                | "EPUB"
                | "MD"
                | "MARKDOWN"
                | "SVG"
        ) {
            return false;
        }
    }
    if let Some(ref fmt) = doc.source.format {
        let ext = fmt.extension.to_ascii_lowercase();
        if matches!(ext.as_str(), "pptx" | "ppsx" | "potx" | "odp" | "xps") {
            return true;
        }
        if matches!(
            ext.as_str(),
            "docx"
                | "odt"
                | "txt"
                | "rtf"
                | "csv"
                | "log"
                | "eml"
                | "msg"
                | "mbox"
                | "pst"
                | "ost"
                | "rsmf"
                | "slack"
                | "teams"
                | "whatsapp"
                | "html"
                | "htm"
                | "mht"
                | "mhtml"
                | "epub"
                | "md"
                | "markdown"
                | "svg"
        ) {
            return false;
        }
    }

    // Heuristic: if any block on the first page has explicit positive bounds, treat as fixed layout
    doc.pages.first().is_some_and(|page| {
        page.content.iter().any(|block| match block {
            ContentBlock::Text(b) => b.bounds.width > 0.0 && b.bounds.height > 0.0,
            ContentBlock::Vector(b) => b.bounds.width > 0.0 && b.bounds.height > 0.0,
            ContentBlock::Table(b) => b.bounds.width > 0.0 && b.bounds.height > 0.0,
            ContentBlock::Container(b) => b.bounds.width > 0.0 && b.bounds.height > 0.0,
            ContentBlock::Image(b) => {
                b.bounds.x > 0.0
                    || b.bounds.y > 0.0
                    || (b.bounds.width > 0.0 && b.bounds.height > 0.0)
            }
            ContentBlock::Embedded(_) => false,
        })
    })
}

fn layout_fixed_pages(ctx: &mut UdmLayoutContext<'_>) -> Result<Vec<LayoutPage>> {
    let mut out_pages = Vec::with_capacity(ctx.doc.pages.len());

    for (page_idx, page) in ctx.doc.pages.iter().enumerate() {
        if page_idx >= ctx.options.max_pages {
            return Err(Error::InvalidInput(format!(
                "Page count exceeds max_pages ({})",
                ctx.options.max_pages
            )));
        }
        #[allow(clippy::cast_possible_truncation)]
        let p_idx = page_idx as u32;
        let size = to_layout_size(page.dimensions);
        let mut layout_page = LayoutPage {
            index: p_idx,
            size,
            items: Vec::new(),
            label: page.metadata.label.clone(),
        };

        for (block_idx, block) in page.content.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let b_idx = block_idx as u32;
            layout_fixed_block(ctx, block, p_idx, b_idx, 0, &mut layout_page.items)?;
        }

        append_page_annotations(page, &mut layout_page.items);
        out_pages.push(layout_page);
    }

    if out_pages.is_empty() {
        out_pages.push(LayoutPage::new(0, Size::LETTER));
    }
    Ok(out_pages)
}

fn layout_fixed_block(
    ctx: &mut UdmLayoutContext<'_>,
    block: &ContentBlock,
    page_idx: u32,
    block_idx: u32,
    depth: u32,
    out_items: &mut Vec<Item>,
) -> Result<()> {
    if depth > ctx.options.max_recursion_depth {
        return Err(Error::InvalidInput(format!(
            "Container recursion depth ({depth}) exceeds max_recursion_depth ({})",
            ctx.options.max_recursion_depth
        )));
    }

    match block {
        ContentBlock::Text(text_block) => {
            layout_fixed_text_block(ctx, text_block, page_idx, block_idx, out_items);
        }
        ContentBlock::Image(img_block) => {
            layout_fixed_image_block(ctx, img_block, out_items);
        }
        ContentBlock::Table(table_block) => {
            layout_fixed_table_block(ctx, table_block, page_idx, block_idx, out_items);
        }
        ContentBlock::Vector(vec_block) => {
            layout_vector_block(vec_block, out_items);
        }
        ContentBlock::Container(container) => {
            layout_container_block(ctx, container, page_idx, block_idx, depth, out_items)?;
        }
        ContentBlock::Embedded(_) => {}
    }
    Ok(())
}

fn layout_fixed_text_block(
    ctx: &mut UdmLayoutContext<'_>,
    text_block: &TextBlock,
    page_idx: u32,
    block_idx: u32,
    out_items: &mut Vec<Item>,
) {
    let bounds = to_layout_rect(text_block.bounds);
    let mut local_items = Vec::new();

    // 1. Emit shape background / border / fill_image if present
    emit_shape_visuals(ctx, bounds, &text_block.style, &mut local_items);

    // 2. Shape text runs inside the box
    if !text_block.runs.is_empty() {
        let has_box = bounds.width > 1.0 && bounds.height > 1.0;
        let inset_x = if has_box && bounds.width > TEXTBOX_INSET_X * 2.5 {
            TEXTBOX_INSET_X
        } else {
            0.0
        };
        let inset_y = if has_box && bounds.height > TEXTBOX_INSET_Y * 2.5 {
            TEXTBOX_INSET_Y
        } else {
            0.0
        };
        let avail_width = has_box.then(|| (bounds.width - inset_x * 2.0).max(1.0));

        let paragraphs = split_runs_into_paragraphs(
            &text_block.runs,
            text_block.style.font_color.as_deref(),
            text_block.style.font_size,
        );

        let mut shaped_paras = Vec::with_capacity(paragraphs.len());
        let mut total_text_h = 0.0_f32;

        for para in &paragraphs {
            let para_w = avail_width.map(|w| (w - para.left_indent).max(1.0));
            let spec = ParagraphSpec {
                spans: para.spans.clone(),
                align: para.align,
                line_spacing: LineSpacing::Multiple(1.15),
                max_width: para_w,
                first_line_indent: 0.0,
                origin: Point::ZERO,
                page_index: page_idx,
                block_index: block_idx,
            };
            let shaped = ctx.text_engine.layout_paragraph(&spec, &mut ctx.font_sys);
            total_text_h += shaped.height;
            shaped_paras.push((para.left_indent, shaped));
        }

        let start_y = if has_box {
            let content_h = (bounds.height - inset_y * 2.0).max(0.0);
            match text_block
                .vertical_alignment
                .unwrap_or(VerticalAlignment::Top)
            {
                VerticalAlignment::Top => bounds.y + inset_y,
                VerticalAlignment::Center
                | VerticalAlignment::Justify
                | VerticalAlignment::Distributed => {
                    bounds.y + inset_y + ((content_h - total_text_h) * 0.5).max(0.0)
                }
                VerticalAlignment::Bottom => {
                    bounds.y + inset_y + (content_h - total_text_h).max(0.0)
                }
            }
        } else {
            bounds.y
        };

        let mut cursor_y = start_y;
        for (left_indent, shaped) in shaped_paras {
            let dx = bounds.x + inset_x + left_indent;
            let dy = cursor_y;
            for mut run in shaped.runs {
                run.transform = Affine::translate(dx, dy).then(run.transform);
                local_items.push(Item::Glyphs(run));
            }
            cursor_y += shaped.height;
        }
    }

    push_with_optional_rotation(
        local_items,
        bounds,
        f32_from_f64(text_block.rotation),
        text_block.style.opacity.map_or(1.0, f32_from_f64),
        out_items,
    );
}

fn layout_fixed_image_block(
    ctx: &UdmLayoutContext<'_>,
    img_block: &ImageBlock,
    out_items: &mut Vec<Item>,
) {
    let bounds = if img_block.bounds.width > 0.0 && img_block.bounds.height > 0.0 {
        to_layout_rect(img_block.bounds)
    } else if let Some(orig) = img_block.original_size {
        Rect::new(
            f32_from_f64(img_block.bounds.x),
            f32_from_f64(img_block.bounds.y),
            f32_from_f64(orig.width),
            f32_from_f64(orig.height),
        )
    } else {
        return;
    };

    let mut local_items = Vec::new();
    emit_shape_visuals(ctx, bounds, &img_block.style, &mut local_items);

    if let Some(&image_id) = ctx.image_id_map.get(&img_block.resource_id) {
        local_items.push(Item::Image(ImageItem {
            image: image_id,
            rect: bounds,
            transform: Affine::IDENTITY,
            opacity: img_block.style.opacity.map_or(1.0, f32_from_f64),
            alt_text: img_block.alt_text.clone(),
        }));
    }

    push_with_optional_rotation(
        local_items,
        bounds,
        f32_from_f64(img_block.rotation),
        1.0,
        out_items,
    );
}

fn layout_fixed_table_block(
    ctx: &mut UdmLayoutContext<'_>,
    table: &TableBlock,
    page_idx: u32,
    block_idx: u32,
    out_items: &mut Vec<Item>,
) {
    let bounds = to_layout_rect(table.bounds);
    let total_w = if bounds.width > 0.0 {
        bounds.width
    } else {
        468.0
    };
    let col_widths = resolve_table_column_widths(table, total_w);
    if col_widths.is_empty() {
        return;
    }

    let mut local_items = Vec::new();
    emit_shape_visuals(ctx, bounds, &table.style, &mut local_items);

    let mut row_y = bounds.y;
    for row in &table.rows {
        let (row_h, mut row_items) =
            layout_table_row(ctx, row, &col_widths, bounds.x, row_y, page_idx, block_idx);
        local_items.append(&mut row_items);
        row_y += row_h;
    }

    push_with_optional_rotation(
        local_items,
        bounds,
        f32_from_f64(table.rotation),
        table.style.opacity.map_or(1.0, f32_from_f64),
        out_items,
    );
}

fn layout_vector_block(vec_block: &VectorBlock, out_items: &mut Vec<Item>) {
    let offset = Affine::translate(
        f32_from_f64(vec_block.bounds.x),
        f32_from_f64(vec_block.bounds.y),
    );
    for vp in &vec_block.paths {
        let commands: Vec<PathCommand> = vp
            .commands
            .iter()
            .map(|cmd| match *cmd {
                prism_core::document::PathCommand::MoveTo(p) => {
                    PathCommand::MoveTo(to_layout_point(p))
                }
                prism_core::document::PathCommand::LineTo(p) => {
                    PathCommand::LineTo(to_layout_point(p))
                }
                prism_core::document::PathCommand::QuadTo { cp, end } => PathCommand::QuadTo {
                    ctrl: to_layout_point(cp),
                    to: to_layout_point(end),
                },
                prism_core::document::PathCommand::CurveTo { cp1, cp2, end } => {
                    PathCommand::CurveTo {
                        ctrl1: to_layout_point(cp1),
                        ctrl2: to_layout_point(cp2),
                        to: to_layout_point(end),
                    }
                }
                prism_core::document::PathCommand::Close => PathCommand::Close,
            })
            .collect();

        if commands.is_empty() {
            continue;
        }

        let fill = vp
            .fill
            .as_deref()
            .and_then(|s| parse_fill_paint(s, to_layout_rect(vec_block.bounds)));
        let stroke = vp
            .stroke
            .as_deref()
            .and_then(Rgba::from_css_str)
            .filter(|c| c.a > 0)
            .map(|color| StrokeStyle {
                color,
                width: vp.stroke_width.map_or(1.0, f32_from_f64).max(0.25),
                ..StrokeStyle::default()
            });

        out_items.push(Item::Path(PathItem {
            commands,
            fill,
            fill_rule: FillRule::NonZero,
            stroke,
            transform: offset,
        }));
    }
}

fn layout_container_block(
    ctx: &mut UdmLayoutContext<'_>,
    container: &ContainerBlock,
    page_idx: u32,
    block_idx: u32,
    depth: u32,
    out_items: &mut Vec<Item>,
) -> Result<()> {
    let mut children = Vec::new();
    for child in &container.children {
        layout_fixed_block(ctx, child, page_idx, block_idx, depth + 1, &mut children)?;
    }
    if children.is_empty() {
        return Ok(());
    }
    let tx = f32_from_f64(container.bounds.x);
    let ty = f32_from_f64(container.bounds.y);
    out_items.push(Item::Group(GroupItem {
        transform: Affine::translate(tx, ty),
        clip: None,
        opacity: 1.0,
        children,
    }));
    Ok(())
}

// ============================================================================
// Flow Layout & Pagination (M3: DOCX, TXT, RTF, ODT, CSV)
// ============================================================================

struct FlowPaginator {
    page_size: Size,
    margins: PageMargins,
    max_pages: usize,
    page_range: Option<std::ops::Range<usize>>,
    pages: Vec<LayoutPage>,
    current_page: LayoutPage,
    cursor_y: f32,
}

impl FlowPaginator {
    fn new(
        page_size: Size,
        margins: PageMargins,
        first_label: Option<String>,
        max_pages: usize,
        page_range: Option<std::ops::Range<usize>>,
    ) -> Self {
        let mut first = LayoutPage::new(0, page_size);
        first.label = first_label;
        let top = f32_from_f64(margins.top);
        Self {
            page_size,
            margins,
            max_pages,
            page_range,
            pages: Vec::new(),
            current_page: first,
            cursor_y: top,
        }
    }

    fn reached_limit(&self) -> bool {
        self.page_range
            .as_ref()
            .is_some_and(|r| self.pages.len() >= r.end)
    }

    fn content_left(&self) -> f32 {
        f32_from_f64(self.margins.left)
    }

    fn content_top(&self) -> f32 {
        f32_from_f64(self.margins.top)
    }

    fn content_bottom(&self) -> f32 {
        (self.page_size.height - f32_from_f64(self.margins.bottom)).max(self.content_top() + 36.0)
    }

    fn content_width(&self) -> f32 {
        (self.page_size.width - f32_from_f64(self.margins.left) - f32_from_f64(self.margins.right))
            .max(72.0)
    }

    fn new_page(&mut self) -> Result<()> {
        let next_idx = self.pages.len() + 1;
        if next_idx >= self.max_pages {
            return Err(Error::InvalidInput(format!(
                "Flow pagination exceeded max_pages ({})",
                self.max_pages
            )));
        }
        #[allow(clippy::cast_possible_truncation)]
        let next_page = LayoutPage::new(next_idx as u32, self.page_size);
        let mut completed = std::mem::replace(&mut self.current_page, next_page);
        if let Some(ref r) = self.page_range
            && (completed.index as usize) < r.start
        {
            completed.items.clear();
        }
        self.pages.push(completed);
        self.cursor_y = self.content_top();
        Ok(())
    }

    fn ensure_space(&mut self, height: f32) -> Result<()> {
        if !self.current_page.items.is_empty() && self.cursor_y + height > self.content_bottom() {
            self.new_page()?;
        }
        Ok(())
    }

    fn finish(mut self) -> Vec<LayoutPage> {
        if !self.current_page.items.is_empty() || self.pages.is_empty() {
            self.pages.push(self.current_page);
        }
        if let Some(ref r) = self.page_range {
            self.pages
                .retain(|p| (p.index as usize) >= r.start && (p.index as usize) < r.end);
        }
        self.pages
    }
}

fn layout_flow_pages(ctx: &mut UdmLayoutContext<'_>) -> Result<Vec<LayoutPage>> {
    let first_page = ctx.doc.pages.first();
    let page_size = first_page.map_or(Size::LETTER, |p| to_layout_size(p.dimensions));
    let margins = first_page
        .and_then(|p| p.metadata.margins)
        .unwrap_or(PageMargins::ONE_INCH);
    let first_label = first_page.and_then(|p| p.metadata.label.clone());

    let mut paginator = FlowPaginator::new(
        page_size,
        margins,
        first_label,
        ctx.options.max_pages,
        ctx.page_range.clone(),
    );
    let mut global_block_idx = 0u32;

    for page in &ctx.doc.pages {
        if paginator.reached_limit() {
            break;
        }
        for block in &page.content {
            if paginator.reached_limit() {
                break;
            }
            #[allow(clippy::cast_possible_truncation)]
            let cur_page_idx = paginator.current_page.index;
            layout_flow_block(ctx, &mut paginator, block, cur_page_idx, global_block_idx)?;
            global_block_idx = global_block_idx.saturating_add(1);
        }
        if !paginator.reached_limit() {
            append_page_annotations(page, &mut paginator.current_page.items);
        }
    }

    Ok(paginator.finish())
}

fn layout_flow_block(
    ctx: &mut UdmLayoutContext<'_>,
    paginator: &mut FlowPaginator,
    block: &ContentBlock,
    page_idx: u32,
    block_idx: u32,
) -> Result<()> {
    match block {
        ContentBlock::Text(text_block) => {
            layout_flow_text_block(ctx, paginator, text_block, page_idx, block_idx)?;
        }
        ContentBlock::Table(table_block) => {
            layout_flow_table_block(ctx, paginator, table_block, page_idx, block_idx)?;
        }
        ContentBlock::Image(img_block) => {
            layout_flow_image_block(ctx, paginator, img_block)?;
        }
        ContentBlock::Vector(vec_block) => {
            let h = f32_from_f64(vec_block.bounds.height).max(20.0);
            paginator.ensure_space(h)?;
            let mut shifted = vec_block.clone();
            shifted.bounds.x = f64::from(paginator.content_left());
            shifted.bounds.y = f64::from(paginator.cursor_y);
            layout_vector_block(&shifted, &mut paginator.current_page.items);
            paginator.cursor_y += h + 6.0;
        }
        ContentBlock::Container(container) => {
            for child in &container.children {
                layout_flow_block(ctx, paginator, child, page_idx, block_idx)?;
            }
        }
        ContentBlock::Embedded(_) => {}
    }
    Ok(())
}

fn layout_flow_text_block(
    ctx: &mut UdmLayoutContext<'_>,
    paginator: &mut FlowPaginator,
    text_block: &TextBlock,
    page_idx: u32,
    block_idx: u32,
) -> Result<()> {
    let named_style = text_block
        .paragraph_style
        .as_deref()
        .and_then(|name| ctx.named_para_styles.get(name).copied());

    let space_before = named_style
        .and_then(|s| s.space_before)
        .map_or(0.0, f32_from_f64);
    let space_after = named_style
        .and_then(|s| s.space_after)
        .map_or(6.0, f32_from_f64);
    let style_left_indent = named_style
        .and_then(|s| s.left_indent)
        .map_or(0.0, f32_from_f64);
    let style_right_indent = named_style
        .and_then(|s| s.right_indent)
        .map_or(0.0, f32_from_f64);
    let first_line_indent = named_style
        .and_then(|s| s.first_line_indent)
        .map_or(0.0, f32_from_f64);
    let line_spacing = named_style
        .and_then(|s| s.line_height)
        .map_or(LineSpacing::Multiple(1.15), |lh| {
            LineSpacing::Multiple(f32_from_f64(lh).clamp(0.8, 4.0))
        });

    let paragraphs = split_runs_into_paragraphs(
        &text_block.runs,
        text_block.style.font_color.as_deref(),
        text_block.style.font_size,
    );

    if paragraphs.is_empty() {
        return Ok(());
    }

    if !paginator.current_page.items.is_empty() {
        paginator.cursor_y += space_before;
    }

    for para in paragraphs {
        let align = if para.has_explicit_align {
            para.align
        } else if let Some(ns) = named_style {
            map_text_alignment(ns.alignment)
        } else {
            TextAlign::Left
        };

        let left_indent = style_left_indent.max(para.left_indent);
        let avail_w = (paginator.content_width() - left_indent - style_right_indent).max(36.0);

        let spec = ParagraphSpec {
            spans: para.spans,
            align,
            line_spacing,
            max_width: Some(avail_w),
            first_line_indent,
            origin: Point::ZERO,
            page_index: page_idx,
            block_index: block_idx,
        };

        let shaped = ctx.text_engine.layout_paragraph(&spec, &mut ctx.font_sys);
        if shaped.runs.is_empty() {
            paginator.cursor_y += 12.0;
            continue;
        }

        // Group shaped runs by line (baseline Y) so lines paginate cleanly across pages
        let line_groups = group_runs_by_line(shaped.runs, shaped.height);
        let para_x = paginator.content_left() + left_indent;

        // The text engine already offsets the first line by `first_line_indent`.
        for (line_top_offset, line_h, line_runs) in line_groups {
            paginator.ensure_space(line_h)?;
            let dx = para_x;
            let dy = paginator.cursor_y - line_top_offset;
            for mut run in line_runs {
                run.source.page = paginator.current_page.index;
                run.transform = Affine::translate(dx, dy).then(run.transform);
                paginator.current_page.items.push(Item::Glyphs(run));
            }
            paginator.cursor_y += line_h;
        }
    }

    paginator.cursor_y += space_after;
    Ok(())
}

fn group_runs_by_line(runs: Vec<GlyphRun>, total_height: f32) -> Vec<(f32, f32, Vec<GlyphRun>)> {
    let mut groups: Vec<(f32, Vec<GlyphRun>)> = Vec::new();
    for run in runs {
        let base_y = run.transform.coeffs[5];
        if let Some(last) = groups.last_mut()
            && (base_y - last.0).abs() <= 2.0
        {
            last.1.push(run);
        } else {
            groups.push((base_y, vec![run]));
        }
    }

    let count = groups.len();
    let mut out = Vec::with_capacity(count);
    let mut prev_top = 0.0_f32;

    for (idx, (_base_y, line_runs)) in groups.into_iter().enumerate() {
        let next_top = if idx + 1 < count {
            #[allow(clippy::cast_precision_loss)]
            let frac = ((idx + 1) as f32) / (count as f32);
            total_height * frac
        } else {
            total_height
        };
        let line_h = (next_top - prev_top).max(10.0);
        out.push((prev_top, line_h, line_runs));
        prev_top = next_top;
    }
    out
}

fn layout_flow_table_block(
    ctx: &mut UdmLayoutContext<'_>,
    paginator: &mut FlowPaginator,
    table: &TableBlock,
    page_idx: u32,
    block_idx: u32,
) -> Result<()> {
    let max_w = paginator.content_width();
    let col_widths = resolve_table_column_widths(table, max_w);
    if col_widths.is_empty() {
        return Ok(());
    }

    for row in &table.rows {
        let (row_h, _) = layout_table_row(
            ctx,
            row,
            &col_widths,
            paginator.content_left(),
            0.0,
            page_idx,
            block_idx,
        );
        paginator.ensure_space(row_h)?;
        let cur_page_idx = paginator.current_page.index;
        let (actual_h, mut row_items) = layout_table_row(
            ctx,
            row,
            &col_widths,
            paginator.content_left(),
            paginator.cursor_y,
            cur_page_idx,
            block_idx,
        );
        paginator.current_page.items.append(&mut row_items);
        paginator.cursor_y += actual_h;
    }

    paginator.cursor_y += 6.0;
    Ok(())
}

fn layout_flow_image_block(
    ctx: &UdmLayoutContext<'_>,
    paginator: &mut FlowPaginator,
    img_block: &ImageBlock,
) -> Result<()> {
    let Some(&image_id) = ctx.image_id_map.get(&img_block.resource_id) else {
        return Ok(());
    };
    let (mut w, mut h) = if img_block.bounds.width > 0.0 && img_block.bounds.height > 0.0 {
        (
            f32_from_f64(img_block.bounds.width),
            f32_from_f64(img_block.bounds.height),
        )
    } else if let Some(orig) = img_block.original_size {
        (f32_from_f64(orig.width), f32_from_f64(orig.height))
    } else if let Some(entry) = ctx.images.get(image_id) {
        #[allow(clippy::cast_precision_loss)]
        {
            (entry.width as f32 * 0.75, entry.height as f32 * 0.75)
        }
    } else {
        (100.0, 100.0)
    };

    let max_w = paginator.content_width();
    let max_h = (paginator.content_bottom() - paginator.content_top()).max(72.0);
    if w > max_w && w > 0.0 {
        let ratio = max_w / w;
        w = max_w;
        h *= ratio;
    }
    if h > max_h && h > 0.0 {
        let ratio = max_h / h;
        h = max_h;
        w *= ratio;
    }

    paginator.ensure_space(h)?;
    let rect = Rect::new(paginator.content_left(), paginator.cursor_y, w, h);
    paginator.current_page.items.push(Item::Image(ImageItem {
        image: image_id,
        rect,
        transform: Affine::IDENTITY,
        opacity: img_block.style.opacity.map_or(1.0, f32_from_f64),
        alt_text: img_block.alt_text.clone(),
    }));
    paginator.cursor_y += h + 6.0;
    Ok(())
}

// ============================================================================
// Shared Helpers (Paragraph Splitting, Tables, Shapes, Gradients)
// ============================================================================

struct LogicalParagraph {
    spans: Vec<SpanSpec>,
    align: TextAlign,
    has_explicit_align: bool,
    left_indent: f32,
}

fn split_runs_into_paragraphs(
    runs: &[TextRun],
    default_color: Option<&str>,
    default_size: Option<f64>,
) -> Vec<LogicalParagraph> {
    let mut paragraphs: Vec<LogicalParagraph> = Vec::new();
    let fallback_color = default_color
        .and_then(Rgba::from_css_str)
        .unwrap_or(Rgba::BLACK);
    let fallback_size = default_size.map_or(11.0, f32_from_f64).max(1.0);

    for (run_idx, run) in runs.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let r_idx = run_idx as u32;
        if run.style.bullet.is_some() && !paragraphs.is_empty() {
            paragraphs.push(LogicalParagraph {
                spans: Vec::new(),
                align: TextAlign::Left,
                has_explicit_align: false,
                left_indent: 0.0,
            });
        }

        let parts: Vec<&str> = run.text.split('\n').collect();
        for (part_idx, raw_part) in parts.into_iter().enumerate() {
            if part_idx > 0 {
                paragraphs.push(LogicalParagraph {
                    spans: Vec::new(),
                    align: TextAlign::Left,
                    has_explicit_align: false,
                    left_indent: 0.0,
                });
            }
            if paragraphs.is_empty() {
                paragraphs.push(LogicalParagraph {
                    spans: Vec::new(),
                    align: TextAlign::Left,
                    has_explicit_align: false,
                    left_indent: 0.0,
                });
            }

            let cur_para = paragraphs.last_mut().expect("non-empty paragraphs");
            if let Some(align) = run.style.alignment {
                cur_para.align = map_text_alignment(align);
                cur_para.has_explicit_align = true;
            }
            if let Some(indent) = run.style.left_indent {
                cur_para.left_indent = cur_para.left_indent.max(f32_from_f64(indent));
            }

            let mut text = if part_idx == 0
                && cur_para.spans.is_empty()
                && let Some(ref bullet) = run.style.bullet
            {
                if cur_para.left_indent <= 0.0 {
                    cur_para.left_indent = 18.0;
                }
                format!("{bullet}{raw_part}")
            } else {
                raw_part.to_string()
            };

            if text.contains('\t') {
                text = text.replace('\t', "    ");
            }
            if text.is_empty() {
                continue;
            }

            let family = run
                .style
                .font_family
                .clone()
                .unwrap_or_else(|| "Calibri".to_string());
            let size = run.style.font_size.map_or(fallback_size, f32_from_f64);
            let color = run
                .style
                .color
                .as_deref()
                .and_then(Rgba::from_css_str)
                .unwrap_or(fallback_color);
            let highlight = run
                .style
                .background_color
                .as_deref()
                .and_then(Rgba::from_css_str);

            cur_para.spans.push(SpanSpec {
                text,
                family,
                size,
                bold: run.style.bold,
                italic: run.style.italic,
                color,
                underline: run.style.underline,
                strikethrough: run.style.strikethrough,
                highlight,
                script: ScriptPosition::Normal,
                run_index: r_idx,
            });
        }
    }

    paragraphs.retain(|p| !p.spans.is_empty());
    paragraphs
}

fn resolve_table_column_widths(table: &TableBlock, available_width: f32) -> Vec<f32> {
    let col_count = table
        .column_count
        .max(table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0));
    if col_count == 0 {
        return Vec::new();
    }

    if !table.column_widths.is_empty() {
        let mut widths: Vec<f32> = table
            .column_widths
            .iter()
            .map(|&w| f32_from_f64(w).max(18.0))
            .collect();
        while widths.len() < col_count {
            widths.push(72.0);
        }
        let sum: f32 = widths.iter().sum();
        if sum > available_width && sum > 0.0 {
            let scale = available_width / sum;
            for w in &mut widths {
                *w *= scale;
            }
        }
        return widths;
    }

    #[allow(clippy::cast_precision_loss)]
    let each = (available_width / (col_count as f32)).max(18.0);
    vec![each; col_count]
}

fn layout_table_row(
    ctx: &mut UdmLayoutContext<'_>,
    row: &prism_core::document::TableRow,
    col_widths: &[f32],
    table_x: f32,
    row_y: f32,
    page_idx: u32,
    block_idx: u32,
) -> (f32, Vec<Item>) {
    let mut cell_layouts = Vec::with_capacity(row.cells.len());
    let mut col_idx = 0usize;
    let mut max_cell_h = row.height.map_or(18.0, f32_from_f64).max(18.0);
    let mut cell_x = table_x;

    for cell in &row.cells {
        if col_idx >= col_widths.len() {
            break;
        }
        let span = cell.col_span.max(1);
        let end_col = (col_idx + span).min(col_widths.len());
        let cell_w: f32 = col_widths[col_idx..end_col].iter().sum();
        let text_max_w = (cell_w - TABLE_CELL_PAD_X * 2.0).max(12.0);

        let mut cell_runs = Vec::new();
        let mut cell_text_h = 0.0_f32;

        for block in &cell.content {
            if let ContentBlock::Text(tb) = block {
                let paras = split_runs_into_paragraphs(
                    &tb.runs,
                    tb.style.font_color.as_deref(),
                    tb.style.font_size,
                );
                for para in paras {
                    let spec = ParagraphSpec {
                        spans: para.spans,
                        align: para.align,
                        line_spacing: LineSpacing::Multiple(1.15),
                        max_width: Some(text_max_w),
                        first_line_indent: 0.0,
                        origin: Point::new(
                            cell_x + TABLE_CELL_PAD_X,
                            row_y + TABLE_CELL_PAD_Y + cell_text_h,
                        ),
                        page_index: page_idx,
                        block_index: block_idx,
                    };
                    let shaped = ctx.text_engine.layout_paragraph(&spec, &mut ctx.font_sys);
                    cell_text_h += shaped.height;
                    cell_runs.extend(shaped.runs);
                }
            }
        }

        let needed_h = cell_text_h + TABLE_CELL_PAD_Y * 2.0;
        max_cell_h = max_cell_h.max(needed_h);
        cell_layouts.push((cell, Rect::new(cell_x, row_y, cell_w, 0.0), cell_runs));

        cell_x += cell_w;
        col_idx = end_col;
    }

    let mut items = Vec::new();
    for (cell, mut rect, runs) in cell_layouts {
        rect.height = max_cell_h;

        // Cell background fill
        if let Some(ref bg_str) = cell.background_color
            && let Some(bg) = Rgba::from_css_str(bg_str)
            && bg.a > 0
        {
            items.push(Item::Path(PathItem {
                commands: rect_commands(rect),
                fill: Some(FillPaint::Solid(bg)),
                fill_rule: FillRule::NonZero,
                stroke: None,
                transform: Affine::IDENTITY,
            }));
        }

        // Cell borders (explicit or default subtle grid)
        emit_cell_borders(rect, cell.borders.as_ref(), &mut items);

        for run in runs {
            items.push(Item::Glyphs(run));
        }
    }

    (max_cell_h, items)
}

fn emit_cell_borders(rect: Rect, borders: Option<&CellBorders>, out: &mut Vec<Item>) {
    if let Some(b) = borders {
        let mut any = false;
        let edges = [
            (
                b.top_color.as_deref(),
                b.top_width,
                Point::new(rect.x, rect.y),
                Point::new(rect.right(), rect.y),
            ),
            (
                b.bottom_color.as_deref(),
                b.bottom_width,
                Point::new(rect.x, rect.bottom()),
                Point::new(rect.right(), rect.bottom()),
            ),
            (
                b.left_color.as_deref(),
                b.left_width,
                Point::new(rect.x, rect.y),
                Point::new(rect.x, rect.bottom()),
            ),
            (
                b.right_color.as_deref(),
                b.right_width,
                Point::new(rect.right(), rect.y),
                Point::new(rect.right(), rect.bottom()),
            ),
        ];
        for (color_str, width_opt, p1, p2) in edges {
            if let Some(cs) = color_str
                && let Some(color) = Rgba::from_css_str(cs)
            {
                any = true;
                let width = width_opt.map_or(0.75, f32_from_f64).max(0.25);
                out.push(Item::Path(PathItem {
                    commands: vec![PathCommand::MoveTo(p1), PathCommand::LineTo(p2)],
                    fill: None,
                    fill_rule: FillRule::NonZero,
                    stroke: Some(StrokeStyle {
                        color,
                        width,
                        ..StrokeStyle::default()
                    }),
                    transform: Affine::IDENTITY,
                }));
            }
        }
        if any {
            return;
        }
    }

    // Default table grid border
    out.push(Item::Path(PathItem {
        commands: rect_commands(rect),
        fill: None,
        fill_rule: FillRule::NonZero,
        stroke: Some(StrokeStyle {
            color: Rgba::rgb(0xD0, 0xD7, 0xDE),
            width: 0.5,
            ..StrokeStyle::default()
        }),
        transform: Affine::IDENTITY,
    }));
}

fn emit_shape_visuals(
    ctx: &UdmLayoutContext<'_>,
    bounds: Rect,
    style: &ShapeStyle,
    out: &mut Vec<Item>,
) {
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }

    let fill = style
        .fill_color
        .as_deref()
        .and_then(|s| parse_fill_paint(s, bounds));
    let stroke = style
        .stroke_color
        .as_deref()
        .and_then(Rgba::from_css_str)
        .filter(|c| c.a > 0)
        .map(|color| StrokeStyle {
            color,
            width: style.stroke_width.map_or(1.0, f32_from_f64).max(0.25),
            ..StrokeStyle::default()
        });

    if fill.is_some() || stroke.is_some() {
        let commands = match style.border_radius.as_deref() {
            Some("50%") => ellipse_commands(bounds),
            Some(r) if r.contains("px") || r.contains("pt") => rounded_rect_commands(bounds, 10.0),
            _ => rect_commands(bounds),
        };
        out.push(Item::Path(PathItem {
            commands,
            fill,
            fill_rule: FillRule::NonZero,
            stroke,
            transform: Affine::IDENTITY,
        }));
    }

    if let Some(ref fill_img_id) = style.fill_image
        && let Some(&image_id) = ctx.image_id_map.get(fill_img_id)
    {
        out.push(Item::Image(ImageItem {
            image: image_id,
            rect: bounds,
            transform: Affine::IDENTITY,
            opacity: style.opacity.map_or(1.0, f32_from_f64),
            alt_text: None,
        }));
    }
}

fn parse_fill_paint(input: &str, bounds: Rect) -> Option<FillPaint> {
    let trimmed = input.trim();
    if let Some(inner) = trimmed
        .strip_prefix("linear-gradient(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() >= 2 {
            let mut angle_deg = 180.0_f32;
            let mut stop_start_idx = 0usize;
            if let Some(deg_str) = parts[0].strip_suffix("deg")
                && let Ok(parsed_deg) = deg_str.trim().parse::<f32>()
            {
                angle_deg = parsed_deg;
                stop_start_idx = 1;
            }

            let mut stops = Vec::new();
            let stop_parts = &parts[stop_start_idx..];
            for (idx, part) in stop_parts.iter().enumerate() {
                let tokens: Vec<&str> = part.split_whitespace().collect();
                if tokens.is_empty() {
                    continue;
                }
                let Some(color) = Rgba::from_css_str(tokens[0]) else {
                    continue;
                };
                let offset = if tokens.len() >= 2
                    && let Some(pct_str) = tokens[1].strip_suffix('%')
                    && let Ok(pct) = pct_str.parse::<f32>()
                {
                    (pct / 100.0).clamp(0.0, 1.0)
                } else if stop_parts.len() > 1 {
                    #[allow(clippy::cast_precision_loss)]
                    {
                        (idx as f32) / ((stop_parts.len() - 1) as f32)
                    }
                } else {
                    0.0
                };
                stops.push(GradientStop { offset, color });
            }

            if !stops.is_empty() {
                // Convert CSS gradient angle (0deg = up, 90deg = right, 180deg = down) to start/end points
                let rad = (angle_deg - 90.0).to_radians();
                let (sin, cos) = rad.sin_cos();
                let cx = bounds.x + bounds.width * 0.5;
                let cy = bounds.y + bounds.height * 0.5;
                let half_w = bounds.width * 0.5;
                let half_h = bounds.height * 0.5;
                let start = Point::new(
                    normalize_zero(cx - cos * half_w),
                    normalize_zero(cy - sin * half_h),
                );
                let end = Point::new(
                    normalize_zero(cx + cos * half_w),
                    normalize_zero(cy + sin * half_h),
                );
                return Some(FillPaint::LinearGradient(LinearGradient {
                    start,
                    end,
                    stops,
                }));
            }
        }
    }

    Rgba::from_css_str(trimmed)
        .filter(|c| c.a > 0)
        .map(FillPaint::Solid)
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

fn rounded_rect_commands(r: Rect, radius: f32) -> Vec<PathCommand> {
    let rad = radius.min(r.width * 0.5).min(r.height * 0.5).max(0.0);
    if rad <= 0.5 {
        return rect_commands(r);
    }
    let right = r.right();
    let bottom = r.bottom();
    vec![
        PathCommand::MoveTo(Point::new(r.x + rad, r.y)),
        PathCommand::LineTo(Point::new(right - rad, r.y)),
        PathCommand::QuadTo {
            ctrl: Point::new(right, r.y),
            to: Point::new(right, r.y + rad),
        },
        PathCommand::LineTo(Point::new(right, bottom - rad)),
        PathCommand::QuadTo {
            ctrl: Point::new(right, bottom),
            to: Point::new(right - rad, bottom),
        },
        PathCommand::LineTo(Point::new(r.x + rad, bottom)),
        PathCommand::QuadTo {
            ctrl: Point::new(r.x, bottom),
            to: Point::new(r.x, bottom - rad),
        },
        PathCommand::LineTo(Point::new(r.x, r.y + rad)),
        PathCommand::QuadTo {
            ctrl: Point::new(r.x, r.y),
            to: Point::new(r.x + rad, r.y),
        },
        PathCommand::Close,
    ]
}

fn ellipse_commands(r: Rect) -> Vec<PathCommand> {
    // 4-segment cubic Bézier approximation of an ellipse (`kappa ≈ 0.55228475`)
    const KAPPA: f32 = 0.552_284_8;
    let rx = r.width * 0.5;
    let ry = r.height * 0.5;
    let cx = r.x + rx;
    let cy = r.y + ry;
    let ox = rx * KAPPA;
    let oy = ry * KAPPA;

    vec![
        PathCommand::MoveTo(Point::new(cx, r.y)),
        PathCommand::CurveTo {
            ctrl1: Point::new(cx + ox, r.y),
            ctrl2: Point::new(r.right(), cy - oy),
            to: Point::new(r.right(), cy),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(r.right(), cy + oy),
            ctrl2: Point::new(cx + ox, r.bottom()),
            to: Point::new(cx, r.bottom()),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(cx - ox, r.bottom()),
            ctrl2: Point::new(r.x, cy + oy),
            to: Point::new(r.x, cy),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(r.x, cy - oy),
            ctrl2: Point::new(cx - ox, r.y),
            to: Point::new(cx, r.y),
        },
        PathCommand::Close,
    ]
}

fn push_with_optional_rotation(
    mut items: Vec<Item>,
    bounds: Rect,
    rotation_deg: f32,
    opacity: f32,
    out: &mut Vec<Item>,
) {
    if items.is_empty() {
        return;
    }
    let needs_group = rotation_deg.abs() > 1e-3 || (opacity > 0.0 && opacity < 1.0 - 1e-4);
    if needs_group {
        let center = Point::new(
            bounds.x + bounds.width * 0.5,
            bounds.y + bounds.height * 0.5,
        );
        let transform = if rotation_deg.abs() > 1e-3 {
            Affine::rotate_around(rotation_deg, center)
        } else {
            Affine::IDENTITY
        };
        out.push(Item::Group(GroupItem {
            transform,
            clip: None,
            opacity: opacity.clamp(0.0, 1.0),
            children: items,
        }));
    } else {
        out.append(&mut items);
    }
}

fn append_page_annotations(page: &Page, out_items: &mut Vec<Item>) {
    for ann in &page.annotations {
        let rect = to_layout_rect(ann.bounds);
        match &ann.annotation_type {
            AnnotationType::Link { url } => {
                out_items.push(Item::Link(crate::model::LinkItem {
                    rect,
                    target: url.clone(),
                }));
            }
            AnnotationType::Comment | AnnotationType::Redaction => {
                if let Some(ref content) = ann.content {
                    out_items.push(Item::Anchor(AnchorItem {
                        name: content.clone(),
                        rect,
                    }));
                }
            }
            _ => {}
        }
    }
}

fn map_text_alignment(align: TextAlignment) -> TextAlign {
    match align {
        TextAlignment::Left => TextAlign::Left,
        TextAlignment::Center => TextAlign::Center,
        TextAlignment::Right => TextAlign::Right,
        TextAlignment::Justify => TextAlign::Justify,
    }
}

fn to_layout_size(dims: prism_core::document::Dimensions) -> Size {
    Size::new(
        f32_from_f64(dims.width).max(1.0),
        f32_from_f64(dims.height).max(1.0),
    )
}

fn to_layout_rect(r: prism_core::document::Rect) -> Rect {
    Rect::new(
        f32_from_f64(r.x),
        f32_from_f64(r.y),
        f32_from_f64(r.width).max(0.0),
        f32_from_f64(r.height).max(0.0),
    )
}

fn to_layout_point(p: prism_core::document::Point) -> Point {
    Point::new(f32_from_f64(p.x), f32_from_f64(p.y))
}

#[allow(clippy::cast_possible_truncation)]
fn f32_from_f64(val: f64) -> f32 {
    normalize_zero(val as f32)
}

// ============================================================================
// Embedded HTML / SVG -> Flow Blocks Converter (Phase 5 M5.2)
// ============================================================================

fn expand_embedded_blocks_to_flow(doc: &Document) -> Document {
    let mut normalized = doc.clone();
    if !normalized.metadata.custom.contains_key("format") {
        normalized.metadata.add_custom("format", "HTML");
    }

    let mut next_img_id = normalized.resources.images.len();
    for page in &mut normalized.pages {
        let old_blocks = std::mem::take(&mut page.content);
        let mut new_blocks = Vec::with_capacity(old_blocks.len());
        for block in old_blocks {
            expand_block_into_flow(
                block,
                &mut normalized.resources.images,
                &mut next_img_id,
                &mut new_blocks,
            );
        }
        page.content = new_blocks;
    }

    normalized
}

fn expand_block_into_flow(
    block: ContentBlock,
    images: &mut Vec<prism_core::document::ImageResource>,
    next_img_id: &mut usize,
    out: &mut Vec<ContentBlock>,
) {
    match block {
        ContentBlock::Embedded(eb) => {
            let markup = std::str::from_utf8(&eb.data).unwrap_or("");
            let fallback = eb.text.as_deref();
            let mut converted = convert_html_to_flow_blocks(markup, fallback, images, next_img_id);
            if converted.is_empty()
                && let Some(fb) = fallback
                && !fb.trim().is_empty()
            {
                let mut tb = TextBlock::new(prism_core::document::Rect::default());
                tb.add_run(TextRun::new(fb.trim()));
                converted.push(ContentBlock::Text(tb));
            }
            out.append(&mut converted);
        }
        ContentBlock::Container(mut container) => {
            let old_children = std::mem::take(&mut container.children);
            let mut new_children = Vec::with_capacity(old_children.len());
            for child in old_children {
                expand_block_into_flow(child, images, next_img_id, &mut new_children);
            }
            if container.bounds.width > 0.0 && container.bounds.height > 0.0 {
                container.children = new_children;
                out.push(ContentBlock::Container(container));
            } else {
                out.append(&mut new_children);
            }
        }
        mut other => {
            // Ensure flow-compatible bounds on top-level text/table blocks when expanding embedded docs
            if let ContentBlock::Text(ref mut tb) = other
                && tb.bounds.x == 0.0
                && tb.bounds.y == 0.0
            {
                tb.bounds = prism_core::document::Rect::default();
            }
            out.push(other);
        }
    }
}

#[derive(Debug)]
enum HtmlToken {
    Tag {
        name: String,
        is_end: bool,
        attrs: Vec<(String, String)>,
    },
    Text(String),
}

fn tokenize_html(input: &str) -> Vec<HtmlToken> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'<' {
            if input[i..].starts_with("<!--") {
                if let Some(end) = input[i + 4..].find("-->") {
                    i += 4 + end + 3;
                } else {
                    break;
                }
                continue;
            }
            if input[i..].starts_with("<!") || input[i..].starts_with("<?") {
                if let Some(end) = input[i + 2..].find('>') {
                    i += 2 + end + 1;
                } else {
                    break;
                }
                continue;
            }

            // Scan to matching '>' respecting quotes
            let mut j = i + 1;
            let mut quote: Option<u8> = None;
            while j < len {
                let b = bytes[j];
                if let Some(q) = quote {
                    if b == q {
                        quote = None;
                    }
                } else if b == b'"' || b == b'\'' {
                    quote = Some(b);
                } else if b == b'>' {
                    break;
                }
                j += 1;
            }

            if j < len && bytes[j] == b'>' {
                let raw_tag = input[i + 1..j].trim();
                if let Some(tok) = parse_html_tag(raw_tag) {
                    tokens.push(tok);
                }
                i = j + 1;
                continue;
            }
        }

        let next_lt = input[i..].find('<').map_or(len, |off| i + off);
        if next_lt > i {
            tokens.push(HtmlToken::Text(input[i..next_lt].to_string()));
            i = next_lt;
        } else {
            i += 1;
        }
    }

    tokens
}

fn parse_html_tag(raw: &str) -> Option<HtmlToken> {
    let mut s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let is_end = if let Some(rest) = s.strip_prefix('/') {
        s = rest.trim_start();
        true
    } else {
        false
    };
    if let Some(rest) = s.strip_suffix('/') {
        s = rest.trim_end();
    }

    let name_end = s
        .find(|c: char| c.is_ascii_whitespace() || c == '/')
        .unwrap_or(s.len());
    let name = s[..name_end].to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }

    let mut attrs = Vec::new();
    let mut rest = s[name_end..].trim();
    while !rest.is_empty() {
        let key_end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        if let Some(after_eq) = rest.strip_prefix('=') {
            rest = after_eq.trim_start();
            let val = if let Some(q) = rest.chars().next().filter(|&c| c == '"' || c == '\'') {
                let inner = &rest[1..];
                if let Some(end_q) = inner.find(q) {
                    let v = &inner[..end_q];
                    rest = inner[end_q + 1..].trim_start();
                    v
                } else {
                    let v = inner;
                    rest = "";
                    v
                }
            } else {
                let val_end = rest
                    .find(|c: char| c.is_ascii_whitespace())
                    .unwrap_or(rest.len());
                let v = &rest[..val_end];
                rest = rest[val_end..].trim_start();
                v
            };
            if !key.is_empty() {
                attrs.push((key, decode_html_entities(val)));
            }
        } else if !key.is_empty() {
            attrs.push((key, String::new()));
        } else {
            break;
        }
    }

    Some(HtmlToken::Tag {
        name,
        is_end,
        attrs,
    })
}

fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';').filter(|&pos| pos <= 12) {
            let entity = &after[..semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                "ndash" => Some('\u{2013}'),
                "mdash" => Some('\u{2014}'),
                "bull" => Some('\u{2022}'),
                "copy" => Some('\u{00A9}'),
                "reg" => Some('\u{00AE}'),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => {
                    entity[1..].parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn collapse_html_whitespace(text: &str, trim_start: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = trim_start;
    for ch in text.chars() {
        if ch.is_ascii_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

fn decode_base64_bytes(b64: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b64.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for byte in b64.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            break;
        }
        let val = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            #[allow(clippy::cast_possible_truncation)]
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[allow(clippy::too_many_lines)]
fn convert_html_to_flow_blocks(
    html: &str,
    fallback_text: Option<&str>,
    images: &mut Vec<prism_core::document::ImageResource>,
    next_img_id: &mut usize,
) -> Vec<ContentBlock> {
    let tokens = tokenize_html(html);
    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut current_runs: Vec<TextRun> = Vec::new();

    let mut ignore_depth = 0u32;
    let mut bold_depth = 0u32;
    let mut italic_depth = 0u32;
    let mut underline_depth = 0u32;
    let mut strike_depth = 0u32;
    let mut code_depth = 0u32;
    let mut pre_depth = 0u32;
    let mut blockquote_depth = 0u32;
    let mut heading_level: Option<u8> = None;
    let mut list_stack: Vec<Option<u32>> = Vec::new();

    // Table state (for depth 1 tables)
    let mut table_depth = 0u32;
    let mut table_rows: Vec<prism_core::document::TableRow> = Vec::new();
    let mut current_row: Vec<prism_core::document::TableCell> = Vec::new();
    let mut in_cell = false;
    let mut cell_is_header = false;
    let mut cell_col_span = 1usize;
    let mut cell_row_span = 1usize;
    let mut cell_runs: Vec<TextRun> = Vec::new();

    let flush_paragraph = |runs: &mut Vec<TextRun>, out: &mut Vec<ContentBlock>| {
        if runs.is_empty() {
            return;
        }
        let combined: String = runs.iter().map(|r| r.text.as_str()).collect();
        if combined.trim().is_empty() {
            runs.clear();
            return;
        }
        let mut tb = TextBlock::new(prism_core::document::Rect::default());
        tb.runs = std::mem::take(runs);
        out.push(ContentBlock::Text(tb));
    };

    let flush_cell = |cell_runs: &mut Vec<TextRun>,
                      in_cell: &mut bool,
                      is_header: bool,
                      col_span: usize,
                      row_span: usize,
                      current_row: &mut Vec<prism_core::document::TableCell>| {
        if !*in_cell {
            return;
        }
        *in_cell = false;
        let mut tb = TextBlock::new(prism_core::document::Rect::default());
        if cell_runs.is_empty() {
            tb.add_run(TextRun::new(""));
        } else {
            tb.runs = std::mem::take(cell_runs);
        }
        let border_color = Some("#CBD5E1".to_string());
        current_row.push(prism_core::document::TableCell {
            content: vec![ContentBlock::Text(tb)],
            col_span: col_span.max(1),
            row_span: row_span.max(1),
            background_color: is_header.then(|| "#F1F5F9".to_string()),
            borders: Some(CellBorders {
                top_color: border_color.clone(),
                top_width: Some(0.5),
                bottom_color: border_color.clone(),
                bottom_width: Some(0.5),
                left_color: border_color.clone(),
                left_width: Some(0.5),
                right_color: border_color,
                right_width: Some(0.5),
            }),
        });
    };

    for token in tokens {
        match token {
            HtmlToken::Tag {
                name,
                is_end,
                attrs,
            } => {
                // `</head>` is optional in HTML, so `<head>` itself is not skipped (its `title`,
                // `style` and `script` children are), and `<body>` ends any element left open.
                if name == "body" && !is_end {
                    ignore_depth = 0;
                    continue;
                }
                if matches!(
                    name.as_str(),
                    "script" | "style" | "title" | "noscript" | "template"
                ) {
                    if is_end {
                        ignore_depth = ignore_depth.saturating_sub(1);
                    } else {
                        ignore_depth += 1;
                    }
                    continue;
                }
                if ignore_depth > 0 {
                    continue;
                }

                match name.as_str() {
                    "b" | "strong" => {
                        if is_end {
                            bold_depth = bold_depth.saturating_sub(1);
                        } else {
                            bold_depth += 1;
                        }
                    }
                    "i" | "em" | "cite" | "dfn" => {
                        if is_end {
                            italic_depth = italic_depth.saturating_sub(1);
                        } else {
                            italic_depth += 1;
                        }
                    }
                    "u" | "ins" | "a" => {
                        if is_end {
                            underline_depth = underline_depth.saturating_sub(1);
                        } else {
                            underline_depth += 1;
                        }
                    }
                    "s" | "strike" | "del" => {
                        if is_end {
                            strike_depth = strike_depth.saturating_sub(1);
                        } else {
                            strike_depth += 1;
                        }
                    }
                    "code" | "kbd" | "samp" | "tt" => {
                        if is_end {
                            code_depth = code_depth.saturating_sub(1);
                        } else {
                            code_depth += 1;
                        }
                    }
                    "pre" => {
                        if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                        }
                        if is_end {
                            pre_depth = pre_depth.saturating_sub(1);
                            code_depth = code_depth.saturating_sub(1);
                        } else {
                            pre_depth += 1;
                            code_depth += 1;
                        }
                    }
                    "blockquote" => {
                        if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                        }
                        if is_end {
                            blockquote_depth = blockquote_depth.saturating_sub(1);
                            italic_depth = italic_depth.saturating_sub(1);
                        } else {
                            blockquote_depth += 1;
                            italic_depth += 1;
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                        }
                        if is_end {
                            heading_level = None;
                            bold_depth = bold_depth.saturating_sub(1);
                        } else {
                            let lvl = name.as_bytes()[1] - b'0';
                            heading_level = Some(lvl);
                            bold_depth += 1;
                        }
                    }
                    "ul" => {
                        if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                        }
                        if is_end {
                            list_stack.pop();
                        } else {
                            list_stack.push(None);
                        }
                    }
                    "ol" => {
                        if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                        }
                        if is_end {
                            list_stack.pop();
                        } else {
                            list_stack.push(Some(0));
                        }
                    }
                    "li" => {
                        if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                        } else if in_cell && !cell_runs.is_empty() {
                            cell_runs.push(TextRun::new("\n"));
                        }
                        if !is_end {
                            let indent = "  ".repeat(list_stack.len().saturating_sub(1));
                            let prefix = match list_stack.last_mut() {
                                Some(Some(n)) => {
                                    *n += 1;
                                    format!("{indent}{n}. ")
                                }
                                _ => format!("{indent}\u{2022} "),
                            };
                            let target = if table_depth > 0 && in_cell {
                                &mut cell_runs
                            } else {
                                &mut current_runs
                            };
                            target.push(TextRun::new(prefix));
                        }
                    }
                    "br" if !is_end => {
                        let target = if table_depth > 0 && in_cell {
                            &mut cell_runs
                        } else {
                            &mut current_runs
                        };
                        target.push(TextRun::new("\n"));
                    }
                    "hr" if !is_end => {
                        if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                            blocks.push(ContentBlock::Vector(VectorBlock {
                                bounds: prism_core::document::Rect::new(0.0, 0.0, 468.0, 8.0),
                                paths: vec![prism_core::document::VectorPath {
                                    commands: vec![
                                        prism_core::document::PathCommand::MoveTo(
                                            prism_core::document::Point::new(0.0, 4.0),
                                        ),
                                        prism_core::document::PathCommand::LineTo(
                                            prism_core::document::Point::new(468.0, 4.0),
                                        ),
                                    ],
                                    fill: None,
                                    stroke: Some("#CBD5E1".to_string()),
                                    stroke_width: Some(1.0),
                                }],
                            }));
                        }
                    }
                    "img" if !is_end => {
                        let src = attrs
                            .iter()
                            .find(|(k, _)| k == "src")
                            .map(|(_, v)| v.as_str());
                        let alt = attrs
                            .iter()
                            .find(|(k, _)| k == "alt")
                            .map(|(_, v)| v.clone());
                        if let Some(src_val) = src
                            && let Some(rest) = src_val.strip_prefix("data:image/")
                            && let Some((mime_sub, b64_part)) = rest.split_once(";base64,")
                            && let Some(raw_bytes) = decode_base64_bytes(b64_part)
                            && !raw_bytes.is_empty()
                        {
                            if table_depth == 0 {
                                flush_paragraph(&mut current_runs, &mut blocks);
                            }
                            let mime_type = format!("image/{mime_sub}");
                            // Header-only probe: decoding the pixels just for the size would let
                            // a small data URI allocate a huge buffer.
                            let (px_w, px_h) =
                                image::ImageReader::new(std::io::Cursor::new(&raw_bytes))
                                    .with_guessed_format()
                                    .ok()
                                    .and_then(|r| r.into_dimensions().ok())
                                    .map_or((120, 80), |(w, h)| (w.max(1), h.max(1)));
                            let res_id = format!("html_img_{}", *next_img_id);
                            *next_img_id += 1;
                            images.push(prism_core::document::ImageResource {
                                id: res_id.clone(),
                                mime_type,
                                width: px_w,
                                height: px_h,
                                data: Some(raw_bytes),
                                url: None,
                            });
                            let disp_w = f64::from(px_w).min(420.0);
                            let disp_h =
                                (f64::from(px_h) * (disp_w / f64::from(px_w))).clamp(12.0, 480.0);
                            blocks.push(ContentBlock::Image(ImageBlock {
                                bounds: prism_core::document::Rect::new(0.0, 0.0, disp_w, disp_h),
                                resource_id: res_id,
                                alt_text: alt,
                                format: None,
                                original_size: Some(prism_core::document::Dimensions::new(
                                    disp_w, disp_h,
                                )),
                                style: ShapeStyle::default(),
                                rotation: 0.0,
                            }));
                        } else if let Some(alt_str) = alt.filter(|a| !a.trim().is_empty()) {
                            let target = if table_depth > 0 && in_cell {
                                &mut cell_runs
                            } else {
                                &mut current_runs
                            };
                            target.push(TextRun::new(format!("[Image: {}]", alt_str.trim())));
                        }
                    }
                    "table" => {
                        if is_end {
                            if table_depth == 1 {
                                flush_cell(
                                    &mut cell_runs,
                                    &mut in_cell,
                                    cell_is_header,
                                    cell_col_span,
                                    cell_row_span,
                                    &mut current_row,
                                );
                                if !current_row.is_empty() {
                                    table_rows.push(prism_core::document::TableRow {
                                        cells: std::mem::take(&mut current_row),
                                        height: None,
                                    });
                                }
                                let col_count = table_rows
                                    .iter()
                                    .map(|r| r.cells.iter().map(|c| c.col_span).sum())
                                    .max()
                                    .unwrap_or(0);
                                if col_count > 0 && !table_rows.is_empty() {
                                    let mut tb = TableBlock::new(
                                        prism_core::document::Rect::default(),
                                        col_count,
                                    );
                                    tb.rows = std::mem::take(&mut table_rows);
                                    blocks.push(ContentBlock::Table(tb));
                                } else {
                                    table_rows.clear();
                                }
                            }
                            table_depth = table_depth.saturating_sub(1);
                        } else {
                            if table_depth == 0 {
                                flush_paragraph(&mut current_runs, &mut blocks);
                                table_rows.clear();
                                current_row.clear();
                                in_cell = false;
                                cell_runs.clear();
                            }
                            table_depth += 1;
                        }
                    }
                    "tr" => {
                        if table_depth == 1 {
                            flush_cell(
                                &mut cell_runs,
                                &mut in_cell,
                                cell_is_header,
                                cell_col_span,
                                cell_row_span,
                                &mut current_row,
                            );
                            if is_end && !current_row.is_empty() {
                                table_rows.push(prism_core::document::TableRow {
                                    cells: std::mem::take(&mut current_row),
                                    height: None,
                                });
                            }
                        } else if table_depth > 1 && in_cell && is_end && !cell_runs.is_empty() {
                            cell_runs.push(TextRun::new("\n"));
                        } else if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                        }
                    }
                    "td" | "th" => {
                        let is_th = name == "th";
                        if table_depth == 1 {
                            if is_end {
                                if is_th {
                                    bold_depth = bold_depth.saturating_sub(1);
                                }
                                flush_cell(
                                    &mut cell_runs,
                                    &mut in_cell,
                                    cell_is_header,
                                    cell_col_span,
                                    cell_row_span,
                                    &mut current_row,
                                );
                            } else {
                                flush_cell(
                                    &mut cell_runs,
                                    &mut in_cell,
                                    cell_is_header,
                                    cell_col_span,
                                    cell_row_span,
                                    &mut current_row,
                                );
                                in_cell = true;
                                cell_is_header = is_th;
                                if is_th {
                                    bold_depth += 1;
                                }
                                cell_col_span = attrs
                                    .iter()
                                    .find(|(k, _)| k == "colspan")
                                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                                    .unwrap_or(1)
                                    .clamp(1, 64);
                                cell_row_span = attrs
                                    .iter()
                                    .find(|(k, _)| k == "rowspan")
                                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                                    .unwrap_or(1)
                                    .clamp(1, 256);
                            }
                        } else if table_depth > 1 && in_cell && is_end {
                            cell_runs.push(TextRun::new("  "));
                        }
                    }
                    "p" | "div" | "section" | "article" | "header" | "footer" | "main"
                    | "aside" | "dl" | "dt" | "dd" => {
                        if table_depth == 0 {
                            flush_paragraph(&mut current_runs, &mut blocks);
                        } else if in_cell && is_end && !cell_runs.is_empty() {
                            cell_runs.push(TextRun::new("\n"));
                        }
                    }
                    _ => {}
                }
            }
            HtmlToken::Text(raw) => {
                if ignore_depth > 0 {
                    continue;
                }
                let decoded = decode_html_entities(&raw);
                let target = if table_depth > 0 && in_cell {
                    &mut cell_runs
                } else {
                    &mut current_runs
                };

                let text = if pre_depth > 0 {
                    decoded
                } else {
                    let trim_start =
                        target.is_empty() || target.last().is_some_and(|r| r.text.ends_with('\n'));
                    collapse_html_whitespace(&decoded, trim_start)
                };
                if text.is_empty() {
                    continue;
                }

                if blockquote_depth > 0
                    && (target.is_empty() || target.last().is_some_and(|r| r.text.ends_with('\n')))
                {
                    target.push(TextRun::new("    "));
                }

                let font_size = match heading_level {
                    Some(1) => Some(20.0),
                    Some(2) => Some(16.5),
                    Some(3) => Some(14.0),
                    Some(4) => Some(12.5),
                    Some(5) => Some(11.5),
                    Some(6) => Some(10.5),
                    _ => Some(11.0),
                };
                let font_family = if code_depth > 0 {
                    Some("Courier New".to_string())
                } else {
                    None
                };
                let color = if underline_depth > 0 && bold_depth == 0 {
                    Some("#1D4ED8".to_string())
                } else {
                    None
                };

                let style = prism_core::document::TextStyle {
                    font_family,
                    font_size,
                    bold: bold_depth > 0,
                    italic: italic_depth > 0,
                    underline: underline_depth > 0,
                    strikethrough: strike_depth > 0,
                    color,
                    ..prism_core::document::TextStyle::default()
                };

                target.push(TextRun::with_style(text, style));
            }
        }
    }

    // Malformed markup can end inside a table; keep what it held.
    if table_depth > 0 {
        flush_cell(
            &mut cell_runs,
            &mut in_cell,
            cell_is_header,
            cell_col_span,
            cell_row_span,
            &mut current_row,
        );
        if !current_row.is_empty() {
            table_rows.push(prism_core::document::TableRow {
                cells: std::mem::take(&mut current_row),
                height: None,
            });
        }
        let col_count = table_rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.col_span).sum())
            .max()
            .unwrap_or(0);
        if col_count > 0 {
            let mut tb = TableBlock::new(prism_core::document::Rect::default(), col_count);
            tb.rows = std::mem::take(&mut table_rows);
            blocks.push(ContentBlock::Table(tb));
        }
    }
    flush_paragraph(&mut current_runs, &mut blocks);

    if blocks.is_empty()
        && let Some(fb) = fallback_text
        && !fb.trim().is_empty()
    {
        for para in fb.split("\n\n") {
            let trimmed = para.trim();
            if !trimmed.is_empty() {
                let mut tb = TextBlock::new(prism_core::document::Rect::default());
                tb.add_run(TextRun::new(trimmed));
                blocks.push(ContentBlock::Text(tb));
            }
        }
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::document::{Dimensions, TableCell, TableRow, TextStyle};

    #[test]
    fn test_pptx_fixed_slide_to_layout_document() {
        let mut slide = Page::new(1, Dimensions::new(960.0, 540.0));

        // Slide gradient background
        let mut bg = TextBlock::new(prism_core::document::Rect::new(0.0, 0.0, 960.0, 540.0));
        bg.style.fill_color = Some("linear-gradient(135deg, #1E3A8A 0%, #3B82F6 100%)".to_string());
        slide.add_content(ContentBlock::Text(bg));

        // Centered title shape with rotation and border
        let mut title = TextBlock::new(prism_core::document::Rect::new(100.0, 80.0, 760.0, 120.0));
        title.vertical_alignment = Some(VerticalAlignment::Center);
        title.style.fill_color = Some("#FFFFFF".to_string());
        title.style.stroke_color = Some("#0F172A".to_string());
        title.style.stroke_width = Some(2.0);
        title.style.border_radius = Some("15px".to_string());
        title.add_run(TextRun::with_style(
            "Quarterly Review Slide",
            TextStyle {
                font_family: Some("Calibri".to_string()),
                font_size: Some(32.0),
                bold: true,
                alignment: Some(TextAlignment::Center),
                color: Some("#0F172A".to_string()),
                ..TextStyle::default()
            },
        ));
        slide.add_content(ContentBlock::Text(title));

        let mut doc = Document::new();
        doc.metadata.add_custom("format", "PPTX");
        doc.pages.push(slide);

        let layout_doc = layout_document(&doc, &LayoutOptions::default()).expect("layout pptx");
        assert_eq!(layout_doc.pages.len(), 1);
        assert!((layout_doc.pages[0].size.width - 960.0).abs() < 1e-3);
        assert!((layout_doc.pages[0].size.height - 540.0).abs() < 1e-3);
        assert_eq!(layout_doc.pages[0].text(), "Quarterly Review Slide");
    }

    #[test]
    fn test_embedded_markup_is_refused_not_dropped() {
        use prism_core::document::{ContainerBlock, EmbeddedBlock, EmbeddedKind};

        // An email-like page: a header line plus an HTML body nested in a container
        let mut page = Page::new(1, Dimensions::LETTER);
        let mut header = TextBlock::new(prism_core::document::Rect::default());
        header.add_run(TextRun::new("From: someone@example.invalid"));
        page.add_content(ContentBlock::Text(header));
        page.add_content(ContentBlock::Container(ContainerBlock {
            bounds: prism_core::document::Rect::default(),
            children: vec![ContentBlock::Embedded(EmbeddedBlock::markup(
                EmbeddedKind::Html,
                "<p>The body a reviewer must not miss</p>",
            ))],
            container_type: None,
        }));

        let mut doc = Document::new();
        doc.pages.push(page);

        let err = layout_document(&doc, &LayoutOptions::default())
            .expect_err("embedded HTML must not be laid out as a blank page");
        assert!(matches!(err, Error::UnsupportedFormat(_)), "{err}");

        // And verify `layout_document_paginated` expands the embedded HTML into flow layout
        let paginated = layout_document_paginated(&doc, &LayoutOptions::default())
            .expect("layout_document_paginated must expand embedded HTML");
        assert_eq!(paginated.pages.len(), 1);
        let text = paginated.pages[0].text();
        assert!(text.contains("From: someone@example.invalid"));
        assert!(text.contains("The body a reviewer must not miss"));
    }

    #[test]
    fn test_malformed_html_keeps_body_and_unclosed_tables() {
        use prism_core::document::{EmbeddedBlock, EmbeddedKind};

        // `</head>` is optional, the `<title>` is never closed, and the table never ends.
        let html = "<html><head><meta charset=utf-8><title>Subject<body>\
                    <p>Body after an open head</p>\
                    <table><tr><td>Last cell</td><td>kept</td>";
        let mut page = Page::new(1, Dimensions::LETTER);
        page.add_content(ContentBlock::Embedded(EmbeddedBlock::markup(
            EmbeddedKind::Html,
            html,
        )));
        let mut doc = Document::new();
        doc.pages.push(page);

        let laid_out = layout_document_paginated(&doc, &LayoutOptions::default()).unwrap();
        let text = laid_out.pages[0].text();
        assert!(text.contains("Body after an open head"), "{text}");
        assert!(text.contains("Last cell"), "{text}");
        assert!(text.contains("kept"), "{text}");
    }

    #[test]
    fn test_flow_layout_pagination_and_tables() {
        let mut page = Page::new(1, Dimensions::LETTER);
        page.metadata.margins = Some(PageMargins::new(72.0, 72.0, 72.0, 72.0));

        // Add 60 multi-line paragraphs to force real multi-page flow pagination
        for i in 1..=60 {
            let mut tb = TextBlock::new(prism_core::document::Rect::default());
            tb.add_run(TextRun::with_style(
                format!(
                    "Paragraph {i}: This is a flowing paragraph of text that wraps across lines and paginates across page margins."
                ),
                TextStyle {
                    font_family: Some("Calibri".to_string()),
                    font_size: Some(12.0),
                    ..TextStyle::default()
                },
            ));
            page.add_content(ContentBlock::Text(tb));
        }

        // Add a simple table
        let mut table = TableBlock::new(prism_core::document::Rect::default(), 2);
        let mut cell1 = TextBlock::new(prism_core::document::Rect::default());
        cell1.add_run(TextRun::new("Header A"));
        let mut cell2 = TextBlock::new(prism_core::document::Rect::default());
        cell2.add_run(TextRun::new("Header B"));
        table.add_row(TableRow {
            cells: vec![
                TableCell {
                    content: vec![ContentBlock::Text(cell1)],
                    col_span: 1,
                    row_span: 1,
                    background_color: Some("#F1F5F9".to_string()),
                    borders: None,
                },
                TableCell {
                    content: vec![ContentBlock::Text(cell2)],
                    col_span: 1,
                    row_span: 1,
                    background_color: Some("#F1F5F9".to_string()),
                    borders: None,
                },
            ],
            height: None,
        });
        page.add_content(ContentBlock::Table(table));

        let mut doc = Document::new();
        doc.metadata.add_custom("format", "DOCX");
        doc.pages.push(page);

        let layout_doc = layout_document(&doc, &LayoutOptions::default()).expect("layout docx");
        assert!(
            layout_doc.pages.len() >= 2,
            "Expected flow pagination to produce multiple pages, got {}",
            layout_doc.pages.len()
        );
        let all_text: String = layout_doc
            .pages
            .iter()
            .map(LayoutPage::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("Paragraph 1:"));
        assert!(all_text.contains("Paragraph 60:"));
        assert!(all_text.contains("Header A"));
        assert!(all_text.contains("Header B"));
    }
}
