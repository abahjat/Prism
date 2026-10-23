// SPDX-License-Identifier: AGPL-3.0-only
//! Conversion from [`WordDocument`] to [`prism_core::document::Document`] (Unified Document Model).

use crate::model::{
    DrawingPayload, ParagraphContent, RevisionKind, RunItem, UnderlineKind, WordBlock,
    WordDocument, WordParagraph, WordRun, WordTable,
};
use prism_core::document::{
    ContentBlock, Dimensions, Document, ImageBlock, Page, PageMargins, PageMetadata, Rect,
    ShapeStyle, TableBlock, TableCell, TableRow, TextBlock, TextRun, TextStyle,
};
use prism_core::metadata::Metadata;

/// Color used to mark tracked revisions (`w:ins`, `w:del`, `w:moveFrom`, `w:moveTo`) in UDM output.
const REVISION_COLOR: &str = "#C00000";

/// Maximum paragraphs per UDM flow page (kept at 50 for backwards compatibility with Phase 0 UDM consumers).
const PARAS_PER_PAGE: usize = 50;

/// Convert a [`WordDocument`] into a [`prism_core::document::Document`].
#[must_use]
pub fn word_to_udm(doc: &WordDocument, format_label: &str) -> Document {
    let final_section = doc.sections.last();
    let page_size = final_section.map_or(Dimensions::LETTER, |s| {
        Dimensions::new(
            f64::from(s.props.page_width_pt),
            f64::from(s.props.page_height_pt),
        )
    });
    let page_margins = final_section.map(|s| {
        PageMargins::new(
            f64::from(s.props.margin_top_pt),
            f64::from(s.props.margin_right_pt),
            f64::from(s.props.margin_bottom_pt),
            f64::from(s.props.margin_left_pt),
        )
    });

    let mut pages: Vec<Page> = Vec::new();
    let mut current_page_content: Vec<ContentBlock> = Vec::new();
    let mut para_count = 0usize;

    for section in &doc.sections {
        for block in &section.blocks {
            match block {
                WordBlock::Paragraph(para) => {
                    para_count += 1;
                    convert_paragraph_to_udm(doc, para, &mut current_page_content);
                    #[allow(clippy::cast_possible_truncation)]
                    if para_count >= PARAS_PER_PAGE && !current_page_content.is_empty() {
                        pages.push(Page {
                            number: (pages.len() + 1) as u32,
                            dimensions: page_size,
                            content: std::mem::take(&mut current_page_content),
                            annotations: Vec::new(),
                            metadata: PageMetadata {
                                margins: page_margins,
                                ..PageMetadata::default()
                            },
                        });
                        para_count = 0;
                    }
                }
                WordBlock::Table(tbl) => {
                    current_page_content.push(ContentBlock::Table(convert_table_to_udm(doc, tbl)));
                }
            }
        }
    }

    // Stories whose position is unknown follow the body, so their text is still extracted.
    for story in &doc.unplaced_stories {
        for block in &story.blocks {
            match block {
                WordBlock::Paragraph(para) => {
                    convert_paragraph_to_udm(doc, para, &mut current_page_content);
                }
                WordBlock::Table(tbl) => {
                    current_page_content.push(ContentBlock::Table(convert_table_to_udm(doc, tbl)));
                }
            }
        }
    }

    if !current_page_content.is_empty() {
        #[allow(clippy::cast_possible_truncation)]
        pages.push(Page {
            number: (pages.len() + 1) as u32,
            dimensions: page_size,
            content: current_page_content,
            annotations: Vec::new(),
            metadata: PageMetadata {
                margins: page_margins,
                ..PageMetadata::default()
            },
        });
    }

    if pages.is_empty() {
        let mut p = Page::new(1, page_size);
        p.metadata.margins = page_margins;
        pages.push(p);
    }

    let mut metadata = Metadata::new();
    metadata.title.clone_from(&doc.title);
    metadata.author.clone_from(&doc.author);
    metadata.add_custom("format", format_label);

    let mut document = Document::builder().metadata(metadata).build();
    document.pages = pages;
    document.resources.images.clone_from(&doc.images);
    document.resources.fonts.clone_from(&doc.fonts);

    document
}

fn convert_paragraph_to_udm(
    doc: &WordDocument,
    para: &WordParagraph,
    out_blocks: &mut Vec<ContentBlock>,
) {
    let para_style_id = para.props.style_id.as_deref();
    let mut text_runs: Vec<TextRun> = Vec::new();
    let mut drawings_after: Vec<ContentBlock> = Vec::new();

    collect_inline_udm(
        doc,
        para_style_id,
        &para.content,
        &mut text_runs,
        &mut drawings_after,
    );

    if !text_runs.is_empty() {
        out_blocks.push(ContentBlock::Text(TextBlock {
            runs: text_runs,
            paragraph_style: para.props.style_id.clone(),
            bounds: Rect::default(),
            vertical_alignment: None,
            style: ShapeStyle::default(),
            rotation: 0.0,
        }));
    }

    out_blocks.extend(drawings_after);
}

fn collect_inline_udm(
    doc: &WordDocument,
    para_style_id: Option<&str>,
    items: &[ParagraphContent],
    text_runs: &mut Vec<TextRun>,
    extra_blocks: &mut Vec<ContentBlock>,
) {
    for item in items {
        match item {
            ParagraphContent::Run(run) => {
                convert_run_to_udm(doc, para_style_id, run, text_runs, extra_blocks);
            }
            ParagraphContent::Field(field) => {
                collect_inline_udm(
                    doc,
                    para_style_id,
                    &field.cached_result,
                    text_runs,
                    extra_blocks,
                );
            }
            ParagraphContent::Hyperlink(hlink) => {
                collect_inline_udm(doc, para_style_id, &hlink.content, text_runs, extra_blocks);
            }
            ParagraphContent::BookmarkStart { .. }
            | ParagraphContent::BookmarkEnd { .. }
            | ParagraphContent::CommentRangeStart(_)
            | ParagraphContent::CommentRangeEnd(_) => {}
        }
    }
}

fn convert_run_to_udm(
    doc: &WordDocument,
    para_style_id: Option<&str>,
    run: &WordRun,
    text_runs: &mut Vec<TextRun>,
    extra_blocks: &mut Vec<ContentBlock>,
) {
    let resolved = doc.styles.resolve_run_style(
        para_style_id,
        run.props.style_id.as_deref(),
        &run.props,
        &doc.theme,
    );

    let mut style = TextStyle {
        font_family: Some(resolved.font_family),
        font_size: Some(f64::from(resolved.font_size_pt)),
        bold: resolved.bold,
        italic: resolved.italic,
        underline: resolved.underline != UnderlineKind::None,
        strikethrough: resolved.strike || resolved.double_strike,
        color: Some(resolved.color),
        background_color: resolved.highlight_color,
        ..TextStyle::default()
    };

    if let Some(ref rev) = run.revision {
        style.color = Some(REVISION_COLOR.to_string());
        match rev.kind {
            RevisionKind::Inserted | RevisionKind::MoveTo => style.underline = true,
            RevisionKind::Deleted | RevisionKind::MoveFrom => style.strikethrough = true,
        }
    }

    let mut text = String::new();
    for item in &run.items {
        match item {
            RunItem::Text(s) => text.push_str(s),
            RunItem::Tab => text.push('\t'),
            RunItem::LineBreak | RunItem::PageBreak | RunItem::ColumnBreak => text.push('\n'),
            RunItem::NoBreakHyphen => text.push('\u{2011}'),
            RunItem::SoftHyphen | RunItem::FootnoteRef(_) | RunItem::EndnoteRef(_) => {}
            RunItem::Symbol { char_code, .. } => {
                if let Some(ch) = char::from_u32(u32::from(*char_code)) {
                    text.push(ch);
                }
            }
            RunItem::Drawing(drawing) => match &drawing.payload {
                DrawingPayload::Image { resource_id, .. } => {
                    extra_blocks.push(ContentBlock::Image(ImageBlock {
                        bounds: Rect::default(),
                        resource_id: resource_id.clone(),
                        alt_text: drawing.alt_text.clone(),
                        format: None,
                        original_size: Some(Dimensions::new(
                            f64::from(drawing.width_pt),
                            f64::from(drawing.height_pt),
                        )),
                        style: ShapeStyle::default(),
                        rotation: f64::from(drawing.rotation_deg),
                    }));
                }
                DrawingPayload::TextBox { blocks, .. } => {
                    for b in blocks {
                        match b {
                            WordBlock::Paragraph(p) => {
                                convert_paragraph_to_udm(doc, p, extra_blocks);
                            }
                            WordBlock::Table(t) => {
                                extra_blocks
                                    .push(ContentBlock::Table(convert_table_to_udm(doc, t)));
                            }
                        }
                    }
                }
                DrawingPayload::Placeholder { .. } => {}
            },
        }
    }

    if !text.is_empty() {
        text_runs.push(TextRun {
            text,
            style,
            bounds: None,
            char_positions: None,
        });
    }
}

fn convert_table_to_udm(doc: &WordDocument, tbl: &WordTable) -> TableBlock {
    let mut rows = Vec::new();
    let mut max_cols = tbl.grid_col_widths_pt.len();

    for row in &tbl.rows {
        let mut cells = Vec::new();
        let mut row_cols = 0usize;
        for cell in &row.cells {
            let mut content = Vec::new();
            for block in &cell.blocks {
                match block {
                    WordBlock::Paragraph(p) => convert_paragraph_to_udm(doc, p, &mut content),
                    WordBlock::Table(nested) => {
                        content.push(ContentBlock::Table(convert_table_to_udm(doc, nested)));
                    }
                }
            }
            let span = usize::from(cell.grid_span.max(1));
            row_cols += span;
            cells.push(TableCell {
                content,
                col_span: span,
                row_span: 1,
                background_color: cell.shading_fill.clone(),
                borders: None,
            });
        }
        max_cols = max_cols.max(row_cols);
        rows.push(TableRow {
            cells,
            height: row.height_pt.map(f64::from),
        });
    }

    TableBlock {
        bounds: Rect::default(),
        rows,
        column_count: max_cols,
        column_widths: tbl
            .grid_col_widths_pt
            .iter()
            .map(|&w| f64::from(w))
            .collect(),
        style: ShapeStyle::default(),
        rotation: 0.0,
    }
}
