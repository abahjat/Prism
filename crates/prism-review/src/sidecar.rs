// SPDX-License-Identifier: AGPL-3.0-only
//! Annotation sidecar schema (`AnnotationSidecar`), geometric + text anchors (`AnnotationAnchor`,
//! `TextAnchor`), cross-engine-version anchor re-resolution, pattern-based proposed redactions,
//! and non-destructive annotation page preview rendering.

use prism_core::ENGINE_VERSION;
use prism_core::error::{Error, Result};
use prism_core::hash::sha256_hex;
use prism_core::review::{ReviewRect, SourceRef};
use prism_layout::{
    Affine, ClipPath, Cluster, Decorations, FillPaint, FillRule, FontSourceKind, GlyphRun,
    GroupItem, Item, LayoutDocument, LayoutPage, PathCommand, PathItem, Point, PositionedGlyph,
    Rect, Rgba, SearchOptions, StrokeStyle,
};
use serde::{Deserialize, Serialize};

/// Current JSON schema version for [`AnnotationSidecar`].
pub const ANNOTATION_SIDECAR_SCHEMA_VERSION: u32 = 1;

const BUNDLED_SANS_BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf");

/// Versioned external sidecar storing annotations and redactions for an unmodified source document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationSidecar {
    /// Sidecar schema version (must be [`ANNOTATION_SIDECAR_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Lowercase hex SHA-256 digest of the original unmodified document bytes.
    pub document_sha256: String,
    /// Prism engine + layout algorithm version string when the sidecar was created.
    pub engine_version: String,
    /// Ordered list of annotations and redactions.
    pub annotations: Vec<Annotation>,
}

impl AnnotationSidecar {
    /// Create a new empty [`AnnotationSidecar`] keyed to `document_bytes` and [`ENGINE_VERSION`].
    #[must_use]
    pub fn new(document_bytes: &[u8]) -> Self {
        Self {
            schema_version: ANNOTATION_SIDECAR_SCHEMA_VERSION,
            document_sha256: sha256_hex(document_bytes),
            engine_version: ENGINE_VERSION.to_string(),
            annotations: Vec::new(),
        }
    }

    /// Parse and validate an [`AnnotationSidecar`] from JSON.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if the JSON is malformed or has an unsupported `schema_version`.
    pub fn from_json(json: &str) -> Result<Self> {
        let sidecar: Self = serde_json::from_str(json)
            .map_err(|e| Error::InvalidInput(format!("Invalid annotation sidecar JSON: {e}")))?;
        if sidecar.schema_version != ANNOTATION_SIDECAR_SCHEMA_VERSION {
            return Err(Error::InvalidInput(format!(
                "Unsupported annotation sidecar schema_version: {} (expected {})",
                sidecar.schema_version, ANNOTATION_SIDECAR_SCHEMA_VERSION
            )));
        }
        if sidecar.document_sha256.len() != 64
            || !sidecar
                .document_sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err(Error::InvalidInput(
                "Invalid document_sha256 in annotation sidecar".to_string(),
            ));
        }
        Ok(sidecar)
    }

    /// Serialize this [`AnnotationSidecar`] to deterministic pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Internal`] if serialization fails.
    pub fn to_json_deterministic(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| Error::Internal(format!("Sidecar JSON serialization error: {e}")))
    }

    /// Add an [`Annotation`] to the sidecar.
    pub fn add(&mut self, annotation: Annotation) {
        self.annotations.push(annotation);
    }

    /// Remove an [`Annotation`] by `id`, returning `true` if one was removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.annotations.len();
        self.annotations.retain(|a| a.id != id);
        self.annotations.len() < before
    }

    /// Approve a proposed redaction or annotation by `id` (`proposed = false`).
    pub fn approve(&mut self, id: &str) -> bool {
        if let Some(ann) = self.annotations.iter_mut().find(|a| a.id == id) {
            ann.proposed = false;
            true
        } else {
            false
        }
    }

    /// Verify that `document_bytes` matches `self.document_sha256` and resolve all annotations
    /// against `layout` under `current_engine_version`.
    ///
    /// When `self.engine_version != current_engine_version`, text-anchored annotations are
    /// re-resolved by [`TextAnchor`] (`SourceRef` range + quoted text + prefix/suffix context),
    /// reporting [`AnchorResolutionStatus::Moved`] or [`AnchorResolutionStatus::Unresolved`]
    /// and **never** silently dropping an annotation.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `sha256_hex(document_bytes) != self.document_sha256`.
    pub fn resolve_against(
        &self,
        document_bytes: &[u8],
        current_engine_version: &str,
        layout: &LayoutDocument,
    ) -> Result<Vec<ResolvedAnnotation>> {
        let actual_sha = sha256_hex(document_bytes);
        if !actual_sha.eq_ignore_ascii_case(&self.document_sha256) {
            return Err(Error::InvalidInput(format!(
                "Annotation sidecar document_sha256 mismatch: sidecar is for {}, document is {}",
                self.document_sha256, actual_sha
            )));
        }

        let same_engine = self.engine_version == current_engine_version;
        let mut out = Vec::with_capacity(self.annotations.len());

        for ann in &self.annotations {
            if same_engine && (!ann.anchor.rects_pt.is_empty() || ann.anchor.text_anchor.is_none())
            {
                // Verify page exists for page-targeted annotations
                let page_ok = match ann.anchor.target {
                    RedactionTarget::CellRange { .. } | RedactionTarget::MetadataField { .. } => {
                        true
                    }
                    RedactionTarget::WholePage { page_index } => {
                        layout.pages.iter().any(|p| p.index == page_index)
                    }
                    RedactionTarget::TextRange | RedactionTarget::PageRect => layout
                        .pages
                        .iter()
                        .any(|p| p.index == ann.anchor.page_index),
                };
                if page_ok {
                    out.push(ResolvedAnnotation {
                        annotation: ann.clone(),
                        status: AnchorResolutionStatus::Exact,
                        effective_page_index: ann.anchor.page_index,
                        effective_rects_pt: ann.anchor.rects_pt.clone(),
                    });
                } else {
                    out.push(ResolvedAnnotation {
                        annotation: ann.clone(),
                        status: AnchorResolutionStatus::Unresolved {
                            reason: format!(
                                "Page {} does not exist in document",
                                ann.anchor.page_index
                            ),
                        },
                        effective_page_index: ann.anchor.page_index,
                        effective_rects_pt: Vec::new(),
                    });
                }
                continue;
            }

            // Cross-engine-version resolution
            if let Some(ref text_anchor) = ann.anchor.text_anchor {
                if let Some((new_page, new_rects)) =
                    re_resolve_text_anchor(layout, ann.anchor.page_index, text_anchor)
                {
                    let unchanged = new_page == ann.anchor.page_index
                        && rects_approx_equal(&new_rects, &ann.anchor.rects_pt);
                    let status = if unchanged {
                        AnchorResolutionStatus::Exact
                    } else {
                        AnchorResolutionStatus::Moved {
                            old_page: ann.anchor.page_index,
                            new_page,
                            new_rects_pt: new_rects.clone(),
                        }
                    };
                    out.push(ResolvedAnnotation {
                        annotation: ann.clone(),
                        status,
                        effective_page_index: new_page,
                        effective_rects_pt: new_rects,
                    });
                } else {
                    out.push(ResolvedAnnotation {
                        annotation: ann.clone(),
                        status: AnchorResolutionStatus::Unresolved {
                            reason: format!(
                                "Could not re-resolve quoted text '{}' under engine {}",
                                text_anchor.quoted_text, current_engine_version
                            ),
                        },
                        effective_page_index: ann.anchor.page_index,
                        effective_rects_pt: Vec::new(),
                    });
                }
            } else {
                // Non-text geometric anchor: check page bounds
                if layout
                    .pages
                    .iter()
                    .any(|p| p.index == ann.anchor.page_index)
                    || matches!(
                        ann.anchor.target,
                        RedactionTarget::CellRange { .. } | RedactionTarget::MetadataField { .. }
                    )
                {
                    out.push(ResolvedAnnotation {
                        annotation: ann.clone(),
                        status: AnchorResolutionStatus::Exact,
                        effective_page_index: ann.anchor.page_index,
                        effective_rects_pt: ann.anchor.rects_pt.clone(),
                    });
                } else {
                    out.push(ResolvedAnnotation {
                        annotation: ann.clone(),
                        status: AnchorResolutionStatus::Unresolved {
                            reason: format!(
                                "Target page {} missing under engine {}",
                                ann.anchor.page_index, current_engine_version
                            ),
                        },
                        effective_page_index: ann.anchor.page_index,
                        effective_rects_pt: Vec::new(),
                    });
                }
            }
        }

        Ok(out)
    }
}

/// A single annotation or redaction stored in an [`AnnotationSidecar`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// Deterministic or UUID identifier.
    pub id: String,
    /// Annotation kind (`Highlight`, `Note`, `AreaBox`, `Redaction`).
    pub kind: AnnotationKind,
    /// Geometric and optional text anchor.
    pub anchor: AnnotationAnchor,
    /// Optional reviewer identifier.
    pub author: Option<String>,
    /// Optional ISO-8601 creation timestamp (supplied by caller/options, never `now()`).
    pub created_at: Option<String>,
    /// Whether this item is a proposed redaction awaiting reviewer confirmation (`true` = not applied on export).
    pub proposed: bool,
}

/// Kind of mark-up annotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnnotationKind {
    /// Translucent text or area highlight.
    Highlight {
        /// Highlight color (`#RRGGBB` or `#RRGGBBAA`, e.g. `"#FFEB3B"`).
        color: String,
    },
    /// Reviewer sticky note / comment attached to a region or text span.
    Note {
        /// Note comment body.
        text: String,
        /// Optional marker color (`#RRGGBB`).
        color: Option<String>,
    },
    /// Stroked callout rectangle on a page.
    AreaBox {
        /// Stroke color (`#RRGGBB`).
        color: String,
        /// Optional box caption.
        label: Option<String>,
    },
    /// Burned-in destructive redaction (applied on export when `proposed == false`).
    Redaction {
        /// Structured reason code (e.g. `"PII"`, `"PRIV"`, `"HIPAA"`, `"TRADE_SECRET"`).
        reason_code: String,
        /// Visible text stamped across the burned-in redaction box (e.g. `"REDACTED - PRIVILEGED"`).
        label: String,
        /// Optional fill color (`#RRGGBB`, defaults to opaque black `"#000000"`).
        fill_color: Option<String>,
    },
}

/// Target scope of an [`AnnotationAnchor`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target_type", rename_all = "snake_case")]
pub enum RedactionTarget {
    /// A text range anchored via [`TextAnchor`] and `rects_pt`.
    TextRange,
    /// One or more rectangular regions on `page_index` (removes all glyphs and image pixels inside).
    PageRect,
    /// An entire 0-based page (`page_index`).
    WholePage {
        /// Zero-based page index.
        page_index: u32,
    },
    /// A spreadsheet cell or range (`sheet_name`, `"B2"` or `"A1:D10"`).
    CellRange {
        /// Worksheet name.
        sheet_name: String,
        /// A1 cell or range reference.
        cell_range: String,
    },
    /// A document metadata field (e.g. `"author"`, `"title"`, `"subject"`, or custom key).
    MetadataField {
        /// Metadata field key.
        field_name: String,
    },
}

/// Geometric and optional text anchor for an [`Annotation`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationAnchor {
    /// Target kind.
    pub target: RedactionTarget,
    /// Zero-based page index.
    pub page_index: u32,
    /// Bounding rectangles in page coordinates (points, top-left origin).
    pub rects_pt: Vec<ReviewRect>,
    /// Optional text anchor for cross-engine-version re-resolution.
    pub text_anchor: Option<TextAnchor>,
}

/// Text anchor recording `SourceRef` endpoints, exact quoted text, and surrounding context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextAnchor {
    /// Starting `SourceRef` in the page model.
    pub start_source: SourceRef,
    /// Ending `SourceRef` in the page model.
    pub end_source: SourceRef,
    /// Exact quoted text covered by the anchor.
    pub quoted_text: String,
    /// Up to 32 characters preceding `quoted_text` on the page.
    pub prefix: String,
    /// Up to 32 characters following `quoted_text` on the page.
    pub suffix: String,
}

impl TextAnchor {
    /// Build a [`TextAnchor`] and its bounding [`ReviewRect`]s for the first occurrence of `quoted_text`
    /// on `page_index` in `layout`.
    #[must_use]
    pub fn from_page_quote(
        layout: &LayoutDocument,
        page_index: u32,
        quoted_text: &str,
    ) -> Option<(Self, Vec<ReviewRect>)> {
        let page = layout.pages.iter().find(|p| p.index == page_index)?;
        let char_boxes = page.char_boxes();
        let page_text: String = char_boxes.iter().map(|cb| cb.ch).collect();
        let byte_start = page_text.find(quoted_text)?;
        let byte_end = byte_start + quoted_text.len();

        let matched: Vec<_> = char_boxes
            .iter()
            .filter(|cb| cb.byte_offset >= byte_start && cb.byte_offset < byte_end)
            .collect();
        if matched.is_empty() {
            return None;
        }

        let start_source = matched.first()?.source;
        let end_source = matched.last()?.source;
        let prefix: String = page_text[..byte_start]
            .chars()
            .rev()
            .take(32)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let suffix: String = page_text[byte_end..].chars().take(32).collect();

        let rects = coalesce_char_rects(&matched);
        Some((
            Self {
                start_source,
                end_source,
                quoted_text: quoted_text.to_string(),
                prefix,
                suffix,
            },
            rects,
        ))
    }
}

/// Resolution status when anchoring an [`Annotation`] against a [`LayoutDocument`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AnchorResolutionStatus {
    /// Anchor resolved to the exact original page and coordinates.
    Exact,
    /// Anchor was re-resolved across an engine version change to updated coordinates.
    Moved {
        /// Original 0-based page index.
        old_page: u32,
        /// Re-resolved 0-based page index.
        new_page: u32,
        /// Re-resolved bounding rectangles in points.
        new_rects_pt: Vec<ReviewRect>,
    },
    /// Anchor could not be resolved against the document (never silently dropped).
    Unresolved {
        /// Reason why resolution failed.
        reason: String,
    },
}

/// Result of resolving an [`Annotation`] against a [`LayoutDocument`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedAnnotation {
    /// Original annotation.
    pub annotation: Annotation,
    /// Resolution status (`Exact`, `Moved`, or `Unresolved`).
    pub status: AnchorResolutionStatus,
    /// Effective 0-based page index.
    pub effective_page_index: u32,
    /// Effective bounding rectangles in points.
    pub effective_rects_pt: Vec<ReviewRect>,
}

/// Generate **proposed** redactions (`proposed: true`) across `layout` from a search term or regular expression.
///
/// Proposed redactions are never applied on export until explicitly approved (`proposed = false`).
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if `is_regex` is `true` and `pattern` is not a valid regular expression.
pub fn propose_pattern_redactions(
    layout: &LayoutDocument,
    pattern: &str,
    is_regex: bool,
    reason_code: &str,
    label: &str,
) -> Result<Vec<Annotation>> {
    if pattern.is_empty() {
        return Ok(Vec::new());
    }
    let mut proposals = Vec::new();

    if is_regex {
        let re = regex::Regex::new(pattern).map_err(|e| {
            Error::InvalidInput(format!("Invalid redaction regex '{pattern}': {e}"))
        })?;

        for page in &layout.pages {
            let char_boxes = page.char_boxes();
            if char_boxes.is_empty() {
                continue;
            }
            let page_text: String = char_boxes.iter().map(|cb| cb.ch).collect();
            for mat in re.find_iter(&page_text) {
                let byte_start = mat.start();
                let byte_end = mat.end();
                let matched: Vec<_> = char_boxes
                    .iter()
                    .filter(|cb| cb.byte_offset >= byte_start && cb.byte_offset < byte_end)
                    .collect();
                if matched.is_empty() {
                    continue;
                }
                let start_source = matched[0].source;
                let end_source = matched[matched.len() - 1].source;
                let prefix: String = page_text[..byte_start]
                    .chars()
                    .rev()
                    .take(32)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let suffix: String = page_text[byte_end..].chars().take(32).collect();
                let rects_pt = coalesce_char_rects(&matched);
                let id = format!("prop-{}-{}", page.index, proposals.len() + 1);
                proposals.push(Annotation {
                    id,
                    kind: AnnotationKind::Redaction {
                        reason_code: reason_code.to_string(),
                        label: label.to_string(),
                        fill_color: None,
                    },
                    anchor: AnnotationAnchor {
                        target: RedactionTarget::TextRange,
                        page_index: page.index,
                        rects_pt,
                        text_anchor: Some(TextAnchor {
                            start_source,
                            end_source,
                            quoted_text: mat.as_str().to_string(),
                            prefix,
                            suffix,
                        }),
                    },
                    author: None,
                    created_at: None,
                    proposed: true,
                });
            }
        }
    } else {
        let hits = layout.find(pattern, SearchOptions::default());
        for (idx, hit) in hits.into_iter().enumerate() {
            let rects_pt: Vec<ReviewRect> = hit
                .rects
                .iter()
                .map(|r| ReviewRect::new(r.x, r.y, r.width, r.height))
                .collect();
            let text_anchor =
                TextAnchor::from_page_quote(layout, hit.page, &hit.text).map(|(ta, _)| ta);
            proposals.push(Annotation {
                id: format!("prop-{}-{}", hit.page, idx + 1),
                kind: AnnotationKind::Redaction {
                    reason_code: reason_code.to_string(),
                    label: label.to_string(),
                    fill_color: None,
                },
                anchor: AnnotationAnchor {
                    target: RedactionTarget::TextRange,
                    page_index: hit.page,
                    rects_pt,
                    text_anchor,
                },
                author: None,
                created_at: None,
                proposed: true,
            });
        }
    }

    Ok(proposals)
}

/// Produce a copy of `page` with non-destructive visual overlays for [`ResolvedAnnotation`]s
/// (`Highlight`, `Note`, `AreaBox`, and — when `preview_redactions` is `true` — `Redaction` boxes).
#[must_use]
pub fn render_page_with_annotations(
    doc: &mut LayoutDocument,
    page_index: u32,
    resolved: &[ResolvedAnnotation],
    preview_redactions: bool,
) -> Option<LayoutPage> {
    let mut page = doc.pages.iter().find(|p| p.index == page_index)?.clone();
    let label_font_id = doc.fonts.register_ttf(
        "Liberation Sans",
        700,
        false,
        FontSourceKind::Bundled,
        BUNDLED_SANS_BOLD.to_vec(),
        0,
    );

    for res in resolved {
        if matches!(res.status, AnchorResolutionStatus::Unresolved { .. }) {
            continue;
        }
        if res.effective_page_index != page_index {
            continue;
        }
        let rects = if matches!(
            res.annotation.anchor.target,
            RedactionTarget::WholePage { .. }
        ) {
            vec![ReviewRect::new(0.0, 0.0, page.size.width, page.size.height)]
        } else {
            res.effective_rects_pt.clone()
        };

        match &res.annotation.kind {
            AnnotationKind::Highlight { color } => {
                let mut c = Rgba::from_css_str(color).unwrap_or(Rgba::new(255, 235, 59, 96));
                if c.a == 255 {
                    c.a = 96;
                }
                for r in rects {
                    page.items
                        .push(Item::Path(rect_path_item(r, Some(c), None)));
                }
            }
            AnnotationKind::Note { text, color } => {
                let c = color
                    .as_deref()
                    .and_then(Rgba::from_css_str)
                    .unwrap_or(Rgba::rgb(255, 193, 7));
                for r in rects {
                    let badge = ReviewRect::new(r.x, r.y, 16.0, 16.0);
                    page.items.push(Item::Path(rect_path_item(
                        badge,
                        Some(c),
                        Some((Rgba::rgb(120, 80, 0), 1.0)),
                    )));
                    let _ = text;
                }
            }
            AnnotationKind::AreaBox { color, label } => {
                let c = Rgba::from_css_str(color).unwrap_or(Rgba::rgb(220, 38, 38));
                for r in rects {
                    page.items
                        .push(Item::Path(rect_path_item(r, None, Some((c, 1.5)))));
                    if let Some(lbl) = label
                        && !lbl.is_empty()
                    {
                        page.items.push(build_box_label_item(
                            &doc.fonts,
                            label_font_id,
                            r,
                            lbl,
                            c,
                            page_index,
                        ));
                    }
                }
            }
            AnnotationKind::Redaction {
                label, fill_color, ..
            } if preview_redactions => {
                let mut fill = fill_color
                    .as_deref()
                    .and_then(Rgba::from_css_str)
                    .unwrap_or(Rgba::BLACK);
                if res.annotation.proposed {
                    fill.a = 160;
                }
                for r in rects {
                    let stroke = if res.annotation.proposed {
                        Some((Rgba::rgb(220, 38, 38), 1.5))
                    } else {
                        None
                    };
                    page.items
                        .push(Item::Path(rect_path_item(r, Some(fill), stroke)));
                    if !label.is_empty() {
                        page.items.push(build_box_label_item(
                            &doc.fonts,
                            label_font_id,
                            r,
                            label,
                            Rgba::WHITE,
                            page_index,
                        ));
                    }
                }
            }
            AnnotationKind::Redaction { .. } => {}
        }
    }

    Some(page)
}

pub(crate) fn rect_path_item(
    r: ReviewRect,
    fill: Option<Rgba>,
    stroke: Option<(Rgba, f32)>,
) -> PathItem {
    let commands = vec![
        PathCommand::MoveTo(Point::new(r.x, r.y)),
        PathCommand::LineTo(Point::new(r.x + r.width, r.y)),
        PathCommand::LineTo(Point::new(r.x + r.width, r.y + r.height)),
        PathCommand::LineTo(Point::new(r.x, r.y + r.height)),
        PathCommand::Close,
    ];
    PathItem {
        commands,
        fill: fill.map(FillPaint::Solid),
        fill_rule: FillRule::NonZero,
        stroke: stroke.map(|(color, width)| StrokeStyle {
            color,
            width,
            line_cap: prism_layout::LineCap::Butt,
            line_join: prism_layout::LineJoin::Miter,
            dash_array: Vec::new(),
            dash_offset: 0.0,
        }),
        transform: Affine::IDENTITY,
    }
}

pub(crate) fn build_box_label_item(
    fonts: &prism_layout::FontStore,
    font_id: prism_layout::FontId,
    r: ReviewRect,
    label: &str,
    color: Rgba,
    page_index: u32,
) -> Item {
    let font_size = (r.height * 0.55).clamp(6.0, 11.0);
    let char_adv = font_size * 0.55;
    let mut glyphs = Vec::new();
    let mut clusters = Vec::new();
    let mut cur_x = 0.0_f32;
    let mut cur_byte = 0u32;

    for (idx, ch) in label.chars().enumerate() {
        let gid = fonts.map_char_to_glyph(font_id, ch);
        glyphs.push(PositionedGlyph {
            glyph_id: gid,
            x: cur_x,
            y: 0.0,
            advance: char_adv,
        });
        #[allow(clippy::cast_possible_truncation)]
        let ch_len = ch.len_utf8() as u32;
        #[allow(clippy::cast_possible_truncation)]
        let g_idx = idx as u32;
        clusters.push(Cluster {
            text_range: cur_byte..(cur_byte + ch_len),
            glyph_range: g_idx..(g_idx + 1),
            x: cur_x,
            advance: char_adv,
        });
        cur_x += char_adv;
        cur_byte += ch_len;
    }

    let baseline_x = r.x + 2.0;
    let baseline_y = r.y + (r.height + font_size * 0.7) * 0.5;
    let run = GlyphRun {
        font: font_id,
        size: font_size,
        color,
        transform: Affine::translate(baseline_x, baseline_y),
        glyphs,
        text: label.to_string(),
        clusters,
        decorations: Decorations::default(),
        source: SourceRef::new(page_index, u32::MAX, u32::MAX),
    };

    let clip = ClipPath {
        commands: vec![
            PathCommand::MoveTo(Point::new(r.x, r.y)),
            PathCommand::LineTo(Point::new(r.x + r.width, r.y)),
            PathCommand::LineTo(Point::new(r.x + r.width, r.y + r.height)),
            PathCommand::LineTo(Point::new(r.x, r.y + r.height)),
            PathCommand::Close,
        ],
        fill_rule: FillRule::NonZero,
    };

    Item::Group(GroupItem {
        transform: Affine::IDENTITY,
        clip: Some(clip),
        opacity: 1.0,
        children: vec![Item::Glyphs(run)],
    })
}

fn re_resolve_text_anchor(
    layout: &LayoutDocument,
    preferred_page: u32,
    ta: &TextAnchor,
) -> Option<(u32, Vec<ReviewRect>)> {
    if ta.quoted_text.is_empty() {
        return None;
    }

    // Order pages with `preferred_page` first, then all other pages
    let mut page_order: Vec<&LayoutPage> = Vec::with_capacity(layout.pages.len());
    if let Some(pref) = layout.pages.iter().find(|p| p.index == preferred_page) {
        page_order.push(pref);
    }
    for p in &layout.pages {
        if p.index != preferred_page {
            page_order.push(p);
        }
    }

    // 1. Try exact match with prefix/suffix context or SourceRef match
    for page in &page_order {
        let char_boxes = page.char_boxes();
        if char_boxes.is_empty() {
            continue;
        }
        let page_text: String = char_boxes.iter().map(|cb| cb.ch).collect();

        let mut search_from = 0usize;
        let mut best_match: Option<(usize, Vec<ReviewRect>)> = None;

        while let Some(rel_idx) = page_text[search_from..].find(&ta.quoted_text) {
            let byte_start = search_from + rel_idx;
            let byte_end = byte_start + ta.quoted_text.len();
            let matched: Vec<_> = char_boxes
                .iter()
                .filter(|cb| cb.byte_offset >= byte_start && cb.byte_offset < byte_end)
                .collect();
            if !matched.is_empty() {
                let mut score = 1usize;
                if matched[0].source == ta.start_source {
                    score += 4;
                }
                if !ta.prefix.is_empty() && page_text[..byte_start].ends_with(&ta.prefix) {
                    score += 2;
                }
                if !ta.suffix.is_empty() && page_text[byte_end..].starts_with(&ta.suffix) {
                    score += 2;
                }
                let rects = coalesce_char_rects(&matched);
                if best_match
                    .as_ref()
                    .is_none_or(|(best_s, _)| score > *best_s)
                {
                    best_match = Some((score, rects));
                }
            }
            search_from = byte_start + ta.quoted_text.chars().next().map_or(1, char::len_utf8);
        }

        if let Some((_, rects)) = best_match {
            return Some((page.index, rects));
        }
    }

    // 2. Fallback: case-insensitive / whitespace-normalized search via `LayoutDocument::find`
    let hits = layout.find(&ta.quoted_text, SearchOptions::default());
    let hit = hits
        .iter()
        .find(|h| h.page == preferred_page)
        .or_else(|| hits.first())?;
    let rects = hit
        .rects
        .iter()
        .map(|r| ReviewRect::new(r.x, r.y, r.width, r.height))
        .collect();
    Some((hit.page, rects))
}

fn coalesce_char_rects(boxes: &[&prism_layout::CharBox]) -> Vec<ReviewRect> {
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
        .into_iter()
        .map(|r| ReviewRect::new(r.x, r.y, r.width, r.height))
        .collect()
}

fn rects_approx_equal(a: &[ReviewRect], b: &[ReviewRect]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(r1, r2)| {
        (r1.x - r2.x).abs() <= 0.5
            && (r1.y - r2.y).abs() <= 0.5
            && (r1.width - r2.width).abs() <= 0.5
            && (r1.height - r2.height).abs() <= 0.5
    })
}
