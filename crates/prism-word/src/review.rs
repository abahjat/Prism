// SPDX-License-Identifier: AGPL-3.0-only
//! Hidden-content and forensic findings provider for Word documents (`.docx` and `.doc`).

use crate::model::{
    DrawingPayload, FieldKind, ParagraphContent, RevisionKind, RunItem, WordBlock, WordDocument,
    WordParagraph,
};
use prism_core::review::{
    Finding, FindingKind, HiddenContentReport, Location, ReviewRect, Severity, SourceRef,
};
use prism_layout::{LayoutDocument, SearchOptions};
use std::collections::BTreeSet;

impl WordDocument {
    /// Generate the cross-format [`HiddenContentReport`] for this Word document.
    ///
    /// When a positioned [`LayoutDocument`] is supplied, findings are enriched with exact
    /// page indices, bounding rectangles, and layout-level concealment checks (off-page text,
    /// occluded/covered text, and low-contrast glyph runs).
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn hidden_content_report(
        &self,
        format_id: &str,
        layout: Option<&LayoutDocument>,
    ) -> HiddenContentReport {
        let mut report = HiddenContentReport::for_format(format_id);

        // 1. Walk all document blocks (sections, headers, footers, footnotes, endnotes, unplaced stories)
        let mut block_idx: u32 = 0;
        for section in &self.sections {
            for block in &section.blocks {
                self.inspect_word_block(block, &mut block_idx, layout, &mut report);
            }
        }
        for story in self
            .headers
            .values()
            .chain(self.footers.values())
            .chain(self.footnotes.values())
            .chain(self.endnotes.values())
            .chain(self.unplaced_stories.iter())
        {
            for block in &story.blocks {
                self.inspect_word_block(block, &mut block_idx, layout, &mut report);
            }
        }

        // 2. Comments
        for comment in self.comments.values() {
            let text = extract_blocks_text(&comment.blocks);
            let loc = find_excerpt_location(layout, &text).unwrap_or(Location::Document);
            report.push(Finding::new(
                FindingKind::Comment,
                Severity::Notable,
                loc,
                None,
                (!text.is_empty()).then_some(text),
                serde_json::json!({
                    "id": comment.id,
                    "author": comment.author,
                    "initials": comment.initials,
                    "date": comment.date,
                    "parent_id": comment.parent_id,
                    "done": comment.done,
                }),
            ));
        }

        // 3. Active & linked content from forensic facts
        if self.forensic.vba_project_present {
            report.push(Finding::new(
                FindingKind::MacroCode,
                Severity::High,
                Location::PartPath {
                    path: "word/vbaProject.bin".to_string(),
                },
                None,
                Some("VBA macro project present".to_string()),
                serde_json::json!({ "vba_project_present": true }),
            ));
        }

        for obj_path in &self.forensic.embedded_objects {
            let lower = obj_path.to_ascii_lowercase();
            let (kind, sev) = if lower.contains("activex") {
                (FindingKind::ActiveXControl, Severity::High)
            } else {
                (FindingKind::EmbeddedOleObject, Severity::Notable)
            };
            report.push(Finding::new(
                kind,
                sev,
                Location::PartPath {
                    path: obj_path.clone(),
                },
                None,
                Some(obj_path.clone()),
                serde_json::json!({ "object_path": obj_path }),
            ));
        }

        let mut seen_dynamic_targets = BTreeSet::new();
        for ext in &self.forensic.external_relationships {
            let upper_type = ext.ref_type.to_ascii_uppercase();
            if matches!(
                upper_type.as_str(),
                "INCLUDEPICTURE" | "INCLUDETEXT" | "DDE" | "DDEAUTO"
            ) {
                seen_dynamic_targets.insert(ext.target.clone());
                report.push(Finding::new(
                    FindingKind::DynamicField,
                    Severity::High,
                    Location::Document,
                    None,
                    Some(format!("{} {}", ext.ref_type, ext.target)),
                    serde_json::json!({
                        "id": ext.id,
                        "field_type": ext.ref_type,
                        "target": ext.target,
                    }),
                ));
            } else {
                let sev = if ext.ref_type.contains("attachedTemplate") {
                    Severity::High
                } else {
                    Severity::Notable
                };
                report.push(Finding::new(
                    FindingKind::ExternalLinkOrConnection,
                    sev,
                    Location::Document,
                    None,
                    Some(ext.target.clone()),
                    serde_json::json!({
                        "id": ext.id,
                        "relationship_type": ext.ref_type,
                        "target": ext.target,
                    }),
                ));
            }
        }

        // 4. Metadata of interest
        if self.author.is_some()
            || self.forensic.attached_template.is_some()
            || !self.forensic.revision_authors.is_empty()
            || self.properties.contains_key("lastModifiedBy")
            || self.properties.contains_key("revision")
        {
            let sev = if self.forensic.attached_template.is_some() {
                Severity::Notable
            } else {
                Severity::Info
            };
            report.push(Finding::new(
                FindingKind::AuthorAndModificationHistory,
                sev,
                Location::Document,
                None,
                self.author
                    .clone()
                    .or_else(|| self.properties.get("lastModifiedBy").cloned()),
                serde_json::json!({
                    "author": self.author,
                    "last_modified_by": self.properties.get("lastModifiedBy"),
                    "revision": self.properties.get("revision"),
                    "attached_template": self.forensic.attached_template,
                    "revision_authors": self.forensic.revision_authors,
                    "revision_count": self.forensic.revision_count,
                }),
            ));
        }

        for (k, v) in &self.forensic.document_variables {
            report.push(Finding::new(
                FindingKind::CustomPropertiesOrVariables,
                Severity::Notable,
                Location::Document,
                None,
                Some(format!("{k} = {v}")),
                serde_json::json!({
                    "kind": "document_variable",
                    "name": k,
                    "value": v,
                }),
            ));
        }
        for (k, v) in &self.forensic.custom_properties {
            report.push(Finding::new(
                FindingKind::CustomPropertiesOrVariables,
                Severity::Notable,
                Location::Document,
                None,
                Some(format!("{k} = {v}")),
                serde_json::json!({
                    "kind": "custom_property",
                    "name": k,
                    "value": v,
                }),
            ));
        }

        // 5. Merge positioned layout concealment findings (off-page, covered text, etc.)
        if let Some(layout_doc) = layout {
            for lf in layout_doc.inspect_concealed_text(false) {
                let duplicate = report.items.iter().any(|existing| {
                    existing.kind == lf.kind
                        && (existing.source == lf.source || existing.excerpt == lf.excerpt)
                });
                if !duplicate {
                    report.push(lf);
                }
            }
        }

        let _ = seen_dynamic_targets;
        report
    }

    fn inspect_word_block(
        &self,
        block: &WordBlock,
        block_idx: &mut u32,
        layout: Option<&LayoutDocument>,
        report: &mut HiddenContentReport,
    ) {
        match block {
            WordBlock::Paragraph(para) => {
                let current_block = *block_idx;
                *block_idx = block_idx.saturating_add(1);
                self.inspect_word_paragraph(para, current_block, layout, report);
            }
            WordBlock::Table(tbl) => {
                let current_block = *block_idx;
                *block_idx = block_idx.saturating_add(1);
                for row in &tbl.rows {
                    for cell in &row.cells {
                        for inner in &cell.blocks {
                            let mut sub_idx = current_block;
                            self.inspect_word_block(inner, &mut sub_idx, layout, report);
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn inspect_word_paragraph(
        &self,
        para: &WordParagraph,
        block_idx: u32,
        layout: Option<&LayoutDocument>,
        report: &mut HiddenContentReport,
    ) {
        let resolved_para = self.styles.resolve_paragraph_style(
            para.props.style_id.as_deref(),
            Some(&self.numbering),
            &para.props,
        );

        // Check fields in paragraph content
        self.inspect_paragraph_fields(&para.content, layout, report);

        for (run_idx, run) in para.runs().into_iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let source = SourceRef::new(0, block_idx, run_idx as u32);
            let run_text = extract_run_text(&run.items);

            // Inspect any nested text boxes inside drawings
            for item in &run.items {
                if let RunItem::Drawing(drawing) = item
                    && let DrawingPayload::TextBox { blocks, .. } = &drawing.payload
                {
                    let mut tb_idx = block_idx;
                    for b in blocks {
                        self.inspect_word_block(b, &mut tb_idx, layout, report);
                    }
                }
            }

            if run_text.is_empty() {
                continue;
            }

            let resolved_run = self.styles.resolve_run_style(
                para.props.style_id.as_deref(),
                run.props.style_id.as_deref(),
                &run.props,
                &self.theme,
            );

            let (loc, resolved_source) = resolve_source_or_search(layout, source, &run_text);

            // Tracked revisions
            if let Some(ref rev) = run.revision {
                let kind = match rev.kind {
                    RevisionKind::Inserted => FindingKind::TrackedInsertion,
                    RevisionKind::Deleted => FindingKind::TrackedDeletion,
                    RevisionKind::MoveFrom | RevisionKind::MoveTo => FindingKind::TrackedMove,
                };
                report.push(Finding::new(
                    kind,
                    Severity::Notable,
                    loc.clone(),
                    resolved_source,
                    Some(run_text.clone()),
                    serde_json::json!({
                        "id": rev.id,
                        "author": rev.author,
                        "date": rev.date,
                        "revision_kind": format!("{:?}", rev.kind),
                    }),
                ));
            }

            // Hidden text (w:vanish)
            if resolved_run.vanish {
                report.push(Finding::new(
                    FindingKind::HiddenText,
                    Severity::High,
                    loc.clone(),
                    resolved_source,
                    Some(run_text.clone()),
                    serde_json::json!({
                        "vanish": true,
                        "font_size_pt": resolved_run.font_size_pt,
                    }),
                ));
            }

            if !run_text.trim().is_empty() {
                // Tiny text (< 2 pt)
                if resolved_run.font_size_pt < 2.0 {
                    report.push(Finding::new(
                        FindingKind::TinyText,
                        Severity::High,
                        loc.clone(),
                        resolved_source,
                        Some(run_text.clone()),
                        serde_json::json!({
                            "font_size_pt": resolved_run.font_size_pt,
                        }),
                    ));
                }

                // White or same-as-shading text
                let bg_hex = resolved_run
                    .highlight_color
                    .as_deref()
                    .or(resolved_para.shading_fill.as_deref())
                    .unwrap_or("#FFFFFF");
                if colors_nearly_equal(&resolved_run.color, bg_hex) {
                    report.push(Finding::new(
                        FindingKind::WhiteOrLowContrastText,
                        Severity::High,
                        loc.clone(),
                        resolved_source,
                        Some(run_text.clone()),
                        serde_json::json!({
                            "text_color": resolved_run.color,
                            "background_color": bg_hex,
                        }),
                    ));
                }
            }

            // Zero-width characters
            if run_text.chars().any(|c| {
                matches!(
                    c,
                    '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}'
                )
            }) {
                report.push(Finding::new(
                    FindingKind::ZeroWidthCharacters,
                    Severity::Notable,
                    loc,
                    resolved_source,
                    Some(run_text),
                    serde_json::json!({}),
                ));
            }
        }
    }

    fn inspect_paragraph_fields(
        &self,
        items: &[ParagraphContent],
        layout: Option<&LayoutDocument>,
        report: &mut HiddenContentReport,
    ) {
        let _ = self;
        for item in items {
            match item {
                ParagraphContent::Field(field) => {
                    let upper = field.instruction.to_ascii_uppercase();
                    let is_dynamic = matches!(
                        field.kind,
                        FieldKind::IncludePicture(_) | FieldKind::IncludeText(_)
                    ) || upper.contains("DDE")
                        || upper.contains("INCLUDETEXT")
                        || upper.contains("INCLUDEPICTURE");
                    if is_dynamic {
                        let cached_text = extract_para_content_text(&field.cached_result);
                        let loc = find_excerpt_location(layout, &cached_text)
                            .unwrap_or(Location::Document);
                        let duplicate = report.items.iter().any(|f| {
                            f.kind == FindingKind::DynamicField
                                && f.detail.get("instruction").and_then(|v| v.as_str())
                                    == Some(field.instruction.trim())
                        });
                        if !duplicate {
                            report.push(Finding::new(
                                FindingKind::DynamicField,
                                Severity::High,
                                loc,
                                None,
                                Some(field.instruction.trim().to_string()),
                                serde_json::json!({
                                    "instruction": field.instruction.trim(),
                                    "cached_result": cached_text,
                                    "dirty": field.dirty,
                                    "locked": field.locked,
                                }),
                            ));
                        }
                    }
                }
                ParagraphContent::Hyperlink(h) => {
                    self.inspect_paragraph_fields(&h.content, layout, report);
                }
                _ => {}
            }
        }
    }
}

fn resolve_source_or_search(
    layout: Option<&LayoutDocument>,
    candidate: SourceRef,
    text: &str,
) -> (Location, Option<SourceRef>) {
    if let Some(layout_doc) = layout {
        if let Some((page_index, rects)) = layout_doc.rects_for_source(candidate) {
            let review_rects = rects
                .into_iter()
                .map(|r| ReviewRect::new(r.x, r.y, r.width, r.height))
                .collect();
            return (
                Location::PageRects {
                    page_index,
                    rects: review_rects,
                },
                Some(candidate),
            );
        }
        if let Some(loc) = find_excerpt_location(Some(layout_doc), text) {
            return (loc, Some(candidate));
        }
    }
    (
        Location::PageRects {
            page_index: candidate.page,
            rects: Vec::new(),
        },
        Some(candidate),
    )
}

fn find_excerpt_location(layout: Option<&LayoutDocument>, text: &str) -> Option<Location> {
    let layout_doc = layout?;
    let query = text.trim();
    if query.is_empty() {
        return None;
    }
    let short_query: String = query.chars().take(48).collect();
    let hits = layout_doc.find(&short_query, SearchOptions::default());
    let first = hits.into_iter().next()?;
    let rects = first
        .rects
        .into_iter()
        .map(|r| ReviewRect::new(r.x, r.y, r.width, r.height))
        .collect();
    Some(Location::PageRects {
        page_index: first.page,
        rects,
    })
}

fn extract_run_text(items: &[RunItem]) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            RunItem::Text(t) => out.push_str(t),
            RunItem::Tab => out.push('\t'),
            RunItem::LineBreak => out.push('\n'),
            _ => {}
        }
    }
    out
}

fn extract_para_content_text(items: &[ParagraphContent]) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ParagraphContent::Run(r) => out.push_str(&extract_run_text(&r.items)),
            ParagraphContent::Field(f) => {
                out.push_str(&extract_para_content_text(&f.cached_result));
            }
            ParagraphContent::Hyperlink(h) => out.push_str(&extract_para_content_text(&h.content)),
            _ => {}
        }
    }
    out
}

fn extract_blocks_text(blocks: &[WordBlock]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            WordBlock::Paragraph(p) => {
                let s = extract_para_content_text(&p.content);
                if !s.trim().is_empty() {
                    parts.push(s);
                }
            }
            WordBlock::Table(t) => {
                for r in &t.rows {
                    for c in &r.cells {
                        let s = extract_blocks_text(&c.blocks);
                        if !s.trim().is_empty() {
                            parts.push(s);
                        }
                    }
                }
            }
        }
    }
    parts.join(" ")
}

fn colors_nearly_equal(hex_a: &str, hex_b: &str) -> bool {
    let Some((r1, g1, b1)) = parse_hex_rgb(hex_a) else {
        return false;
    };
    let Some((r2, g2, b2)) = parse_hex_rgb(hex_b) else {
        return false;
    };
    let dist = (i32::from(r1) - i32::from(r2)).abs()
        + (i32::from(g1) - i32::from(g2)).abs()
        + (i32::from(b1) - i32::from(b2)).abs();
    dist <= 24
}

fn parse_hex_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}
